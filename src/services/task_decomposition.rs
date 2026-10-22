use std::{future::Future, sync::Arc, time::Duration, time::Instant};

use axum::{body::Bytes, http::HeaderMap};
use futures_util::StreamExt;
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::{
    adapters::{
        ProviderRequestInput, prepare_for_provider, protocol, provider, response::buffered_text,
        validate_for_provider,
    },
    canonical::{CanonicalRequest, PrincipalRef, RequestEnrichment},
    model_registry::{GatewayOperation, ModelRegistry, ProviderKind},
    orchestration::{
        DataPolicy, DecompositionDecision,
        decomposer::{
            CONTRACT_VERSION, DecompositionPrompt, MAX_RESPONSE_BYTES, build_prompt, eligibility,
            parse_decision,
        },
    },
};

const MODEL_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CANDIDATES: usize = 3;
const MAX_OUTPUT_TOKENS: u32 = 1_024;
const DECOMPOSER_TASK_ID: &str = "__decomposer__";

/// Why a model call produced no usable text. Only the code is ever logged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelFailure {
    NoModel,
    Failed,
}

pub trait DecompositionModel {
    fn complete(
        &self,
        prompt: &DecompositionPrompt,
        ceiling: DataPolicy,
    ) -> impl Future<Output = Result<Vec<u8>, ModelFailure>> + Send;
}

/// Owned snapshot of what the decomposer needs from a request, so the work can
/// run after the request handler has moved on.
pub struct DecompositionInput {
    eligibility: Result<(), &'static str>,
    prompt: DecompositionPrompt,
    principal: PrincipalRef,
    workload_class: String,
    urgency: String,
    budget_class: String,
}

impl DecompositionInput {
    pub fn from_request(request: &CanonicalRequest) -> Self {
        Self {
            eligibility: eligibility(request),
            prompt: build_prompt(request),
            principal: request.principal.clone(),
            workload_class: request.workload_class.clone(),
            urgency: request.urgency.clone(),
            budget_class: request.budget_class.clone(),
        }
    }
}

/// Every failure collapses to `Direct`: decomposition must never make a
/// request fail or wait longer than `timeout`. Only the reason code is logged.
pub async fn decompose_with(
    model: &impl DecompositionModel,
    input: &DecompositionInput,
    ceiling: DataPolicy,
    orchestration_id: Uuid,
    timeout: Duration,
) -> DecompositionDecision {
    let decision = run(model, input, ceiling, orchestration_id, timeout).await;
    if let DecompositionDecision::Direct { reason } = &decision {
        tracing::info!(
            event.name = "orchestration.decompose.direct",
            reason = %reason,
            "task decomposition skipped"
        );
    }
    decision
}

async fn run(
    model: &impl DecompositionModel,
    input: &DecompositionInput,
    ceiling: DataPolicy,
    orchestration_id: Uuid,
    timeout: Duration,
) -> DecompositionDecision {
    if let Err(reason) = input.eligibility {
        return DecompositionDecision::direct(reason);
    }
    let raw = match tokio::time::timeout(timeout, model.complete(&input.prompt, ceiling)).await {
        Err(_) => return DecompositionDecision::direct("model_timeout"),
        Ok(Err(ModelFailure::NoModel)) => {
            return DecompositionDecision::direct("no_eligible_model");
        }
        Ok(Err(ModelFailure::Failed)) => return DecompositionDecision::direct("model_error"),
        Ok(Ok(raw)) => raw,
    };
    parse_decision(&raw, orchestration_id, ceiling)
}

/// Shadow mode: runs after the main request has been routed and never
/// influences it. The tenant config is read here, not in the request path, so
/// a disabled tenant costs the request nothing.
pub async fn observe(
    pool: &PgPool,
    model: &impl DecompositionModel,
    input: &DecompositionInput,
    gateway_request_id: Uuid,
    timeout: Duration,
) {
    let tenant_id = input.principal.tenant_id;
    let ceiling = match load_policy(pool, tenant_id).await {
        Ok(Some(ceiling)) => ceiling,
        Ok(None) => return,
        Err(_) => {
            tracing::warn!(
                event.name = "orchestration.config.unavailable",
                "shadow config read failed"
            );
            return;
        }
    };
    let orchestration_id = Uuid::new_v4();
    let started = Instant::now();
    let decision = decompose_with(model, input, ceiling, orchestration_id, timeout).await;
    let elapsed_us = i64::try_from(started.elapsed().as_micros()).unwrap_or(i64::MAX);
    if record(
        pool,
        tenant_id,
        orchestration_id,
        gateway_request_id,
        &decision,
        elapsed_us,
    )
    .await
    .is_err()
    {
        tracing::warn!(
            event.name = "orchestration.record.failed",
            "shadow decomposition not recorded"
        );
    }
}

/// Cheap clones of the AppState parts shadow work needs, so it can outlive the
/// request handler that borrowed the state.
#[derive(Clone)]
pub struct ShadowContext {
    pub(crate) pool: PgPool,
    pub(crate) models: ModelRegistry,
    pub(crate) client: reqwest::Client,
    pub(crate) slots: Arc<Semaphore>,
}

pub fn spawn_shadow(context: ShadowContext, input: DecompositionInput, gateway_request_id: Uuid) {
    let Ok(slot) = context.slots.clone().try_acquire_owned() else {
        return;
    };
    tokio::spawn(async move {
        let _slot = slot;
        observe(
            &context.pool,
            &GatewayModel {
                context: &context,
                input: &input,
            },
            &input,
            gateway_request_id,
            MODEL_TIMEOUT,
        )
        .await;
    });
}

/// `None` unless the tenant opted into shadow mode.
async fn load_policy(pool: &PgPool, tenant_id: Uuid) -> Result<Option<DataPolicy>, sqlx::Error> {
    if tenant_id.is_nil() {
        return Ok(None);
    }
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT mode, data_policy FROM orchestration_configs WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|(mode, policy)| {
        (mode == "shadow").then_some(if policy == "online_allowed" {
            DataPolicy::OnlineAllowed
        } else {
            DataPolicy::LocalOnly
        })
    }))
}

async fn record(
    pool: &PgPool,
    tenant_id: Uuid,
    orchestration_id: Uuid,
    gateway_request_id: Uuid,
    decision: &DecompositionDecision,
    elapsed_us: i64,
) -> Result<(), sqlx::Error> {
    let (label, reason, node_count, max_depth, status) = match decision {
        DecompositionDecision::Decomposed(graph) => {
            let depth = if graph.nodes.iter().any(|node| !node.dependencies.is_empty()) {
                2
            } else {
                1
            };
            (
                "decomposed",
                "decomposed",
                graph.nodes.len(),
                depth,
                "completed",
            )
        }
        DecompositionDecision::Direct { reason } => {
            let status = match reason.as_str() {
                "model_direct" => "completed",
                "model_timeout" | "model_error" | "bad_json" | "response_too_large" => "failed",
                _ => "skipped",
            };
            ("direct", reason.as_str(), 0, 0, status)
        }
    };
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO orchestration_executions
            (tenant_id, orchestration_id, execution_role, task_id, attempt, status, elapsed_us)
         VALUES ($1, $2, 'decomposer', $3, 1, $4, $5)",
    )
    .bind(tenant_id)
    .bind(orchestration_id)
    .bind(DECOMPOSER_TASK_ID)
    .bind(status)
    .bind(elapsed_us)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO task_decompositions
            (tenant_id, orchestration_id, gateway_request_id, decision, reason,
             node_count, max_depth, elapsed_us, contract_version)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(tenant_id)
    .bind(orchestration_id)
    .bind(gateway_request_id)
    .bind(label)
    .bind(reason)
    .bind(node_count as i32)
    .bind(max_depth)
    .bind(elapsed_us)
    .bind(CONTRACT_VERSION)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// LocalOnly data may only reach a local provider; the gateway's own
/// credentials are the only ones usable here, so client-bearer profiles are out.
fn may_serve(ceiling: DataPolicy, kind: ProviderKind) -> bool {
    ceiling != DataPolicy::LocalOnly || matches!(kind, ProviderKind::Vllm | ProviderKind::Ollama)
}

struct GatewayModel<'a> {
    context: &'a ShadowContext,
    input: &'a DecompositionInput,
}

impl DecompositionModel for GatewayModel<'_> {
    async fn complete(
        &self,
        prompt: &DecompositionPrompt,
        ceiling: DataPolicy,
    ) -> Result<Vec<u8>, ModelFailure> {
        let context = self.context;
        let input = self.input;
        let candidates = context
            .models
            .routing_candidates(
                input.principal.tenant_id,
                false,
                GatewayOperation::OpenAiResponses,
                Default::default(),
            )
            .await
            .map_err(|_| ModelFailure::NoModel)?;
        let eligible: Vec<_> = candidates
            .into_iter()
            .filter(|selection| !selection.profile.uses_client_bearer())
            .filter(|selection| may_serve(ceiling, selection.profile.kind))
            .take(MAX_CANDIDATES)
            .collect();
        if eligible.is_empty() {
            return Err(ModelFailure::NoModel);
        }
        for selection in eligible {
            let body = json!({
                "model": selection.provider_model_id,
                "instructions": prompt.system,
                "input": [{"role": "user", "content": prompt.user}],
                "max_output_tokens": MAX_OUTPUT_TOKENS,
                "stream": false,
            });
            let Ok(bytes) = serde_json::to_vec(&body) else {
                continue;
            };
            let Ok(decoded) =
                protocol::decode(GatewayOperation::OpenAiResponses, Bytes::from(bytes))
            else {
                continue;
            };
            let internal = decoded.enrich(RequestEnrichment {
                gateway_request_id: Uuid::new_v4(),
                trace_id: None,
                session_id: None,
                principal: input.principal.clone(),
                requested_model: selection.public_model_id.clone(),
                requested_quality: None,
                workload_class: input.workload_class.clone(),
                urgency: input.urgency.clone(),
                budget_class: input.budget_class.clone(),
                deadline: None,
            });
            let Ok(plan) = validate_for_provider(&internal, &selection) else {
                continue;
            };
            let Ok(prepared) = prepare_for_provider(
                &internal,
                &selection.profile,
                &selection.provider_model_id,
                plan,
                ProviderRequestInput {
                    source_path_and_query: "/v1/responses",
                    client_headers: &HeaderMap::new(),
                    idempotency_key: None,
                },
            ) else {
                continue;
            };
            let Ok(response) = provider::send(
                &context.client,
                &selection.profile.base_url,
                prepared,
                &selection.profile.provider_key,
                &selection.provider_model_id,
            )
            .await
            else {
                continue;
            };
            if !response.status().is_success() {
                continue;
            }
            let Some(raw) = read_capped(response).await else {
                continue;
            };
            if let Ok(text) = buffered_text(plan.target_operation, &raw) {
                return Ok(text.into_bytes());
            }
        }
        Err(ModelFailure::Failed)
    }
}

async fn read_capped(response: reqwest::Response) -> Option<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        body.extend_from_slice(&chunk.ok()?);
        if body.len() > MAX_RESPONSE_BYTES * 4 {
            return None;
        }
    }
    Some(body)
}

#[cfg(test)]
mod tests {
    use std::{
        io::Write,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use sqlx::PgPool;

    use super::*;
    use crate::{
        canonical::PrincipalRef,
        orchestration::decomposer::{MAX_RESPONSE_BYTES, parse_decision},
    };

    const SENTINEL: &str = "sentinel-prompt-7f3a9c";

    fn request(operation: GatewayOperation, body: &str) -> CanonicalRequest {
        protocol::decode(operation, Bytes::from(body.to_owned()))
            .unwrap()
            .enrich(RequestEnrichment {
                gateway_request_id: Uuid::nil(),
                trace_id: None,
                session_id: None,
                principal: PrincipalRef {
                    tenant_id: Uuid::nil(),
                    credential_id: Uuid::nil(),
                    user_id: None,
                    device_id: None,
                    client_id: None,
                    agent_id: None,
                },
                requested_model: "public".to_owned(),
                requested_quality: None,
                workload_class: "interactive".to_owned(),
                urgency: "normal".to_owned(),
                budget_class: "standard".to_owned(),
                deadline: None,
            })
    }

    fn plain(text: &str) -> CanonicalRequest {
        request(
            GatewayOperation::OpenAiResponses,
            &json!({"model": "public", "input": text}).to_string(),
        )
    }

    struct Fake {
        reply: Result<Vec<u8>, ModelFailure>,
        delay: Duration,
        calls: Arc<AtomicUsize>,
        ceilings: Arc<Mutex<Vec<DataPolicy>>>,
    }

    impl Fake {
        fn text(reply: &str) -> Self {
            Self {
                reply: Ok(reply.as_bytes().to_vec()),
                delay: Duration::ZERO,
                calls: Arc::default(),
                ceilings: Arc::default(),
            }
        }
    }

    impl DecompositionModel for Fake {
        async fn complete(
            &self,
            _: &DecompositionPrompt,
            ceiling: DataPolicy,
        ) -> Result<Vec<u8>, ModelFailure> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.ceilings.lock().unwrap().push(ceiling);
            tokio::time::sleep(self.delay).await;
            self.reply.clone()
        }
    }

    const TWO_NODES: &str = r#"{"decision":"decompose","confidence":0.9,"nodes":[
        {"id":"a","objective":"review the code","task_kind":"code_review","output":"text"},
        {"id":"b","objective":"write the tests","task_kind":"code_generation","dependencies":["a"],"output":"text"}]}"#;

    async fn run_with(
        fake: &Fake,
        request: &CanonicalRequest,
        ceiling: DataPolicy,
    ) -> DecompositionDecision {
        decompose_with(
            fake,
            &DecompositionInput::from_request(request),
            ceiling,
            Uuid::nil(),
            Duration::from_millis(100),
        )
        .await
    }

    fn reason(decision: &DecompositionDecision) -> &str {
        match decision {
            DecompositionDecision::Direct { reason } => reason,
            DecompositionDecision::Decomposed(_) => "decomposed",
        }
    }

    #[tokio::test]
    async fn valid_json_becomes_a_validated_graph() {
        let fake = Fake::text(TWO_NODES);
        let decision = run_with(&fake, &plain("review then test"), DataPolicy::OnlineAllowed).await;
        let DecompositionDecision::Decomposed(graph) = decision else {
            panic!("expected a graph");
        };
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.nodes[1].dependencies, ["a"]);
    }

    #[tokio::test]
    async fn ineligible_requests_never_reach_the_model() {
        let cases = [
            (
                "tools_defined",
                GatewayOperation::OpenAiResponses,
                json!({"model":"public","input":"hi","tools":[{"type":"function","name":"lookup","parameters":{"type":"object"}}]}),
            ),
            (
                "tool_choice_set",
                GatewayOperation::OpenAiResponses,
                json!({"model":"public","input":"hi","tool_choice":"required"}),
            ),
            (
                "tool_history",
                GatewayOperation::OpenAiResponses,
                json!({"model":"public","input":[{"type":"function_call","call_id":"c1","name":"lookup","arguments":"{}"},{"type":"function_call_output","call_id":"c1","output":"ok"}]}),
            ),
            (
                "token_count",
                GatewayOperation::AnthropicCountTokens,
                json!({"model":"public","messages":[{"role":"user","content":"hi"}]}),
            ),
        ];
        for (expected, operation, body) in cases {
            let fake = Fake::text(TWO_NODES);
            let decision = run_with(
                &fake,
                &request(operation, &body.to_string()),
                DataPolicy::OnlineAllowed,
            )
            .await;
            assert_eq!(reason(&decision), expected);
            assert_eq!(fake.calls.load(Ordering::SeqCst), 0, "{expected}");
        }
    }

    #[tokio::test]
    async fn bad_model_output_falls_back_to_direct() {
        let nine = format!(
            r#"{{"decision":"decompose","confidence":0.9,"nodes":[{}]}}"#,
            (0..9)
                .map(|i| format!(
                    r#"{{"id":"n{i}","objective":"x","task_kind":"research","output":"text"}}"#
                ))
                .collect::<Vec<_>>()
                .join(",")
        );
        let cycle = r#"{"decision":"decompose","confidence":0.9,"nodes":[
            {"id":"a","objective":"x","task_kind":"research","dependencies":["b"],"output":"text"},
            {"id":"b","objective":"y","task_kind":"research","dependencies":["a"],"output":"text"}]}"#;
        let low = TWO_NODES.replace("0.9", "0.5");
        let unknown_kind = TWO_NODES.replace("code_review", "world_domination");
        let extra_field =
            TWO_NODES.replace(r#""confidence":0.9,"#, r#""confidence":0.9,"route":"x","#);
        let cases = [
            ("not json", "bad_json"),
            (nine.as_str(), "graph_too_many_nodes"),
            (cycle, "graph_cycle"),
            (low.as_str(), "low_confidence"),
            (unknown_kind.as_str(), "unknown_task_kind"),
            (extra_field.as_str(), "bad_json"),
        ];
        for (reply, expected) in cases {
            let decision = run_with(
                &Fake::text(reply),
                &plain("hi there"),
                DataPolicy::OnlineAllowed,
            )
            .await;
            assert_eq!(reason(&decision), expected, "{reply}");
        }
        let oversize = "x".repeat(MAX_RESPONSE_BYTES + 1);
        let decision = run_with(
            &Fake::text(&oversize),
            &plain("hi there"),
            DataPolicy::OnlineAllowed,
        )
        .await;
        assert_eq!(reason(&decision), "response_too_large");
    }

    #[tokio::test]
    async fn timeout_and_model_errors_fall_back_to_direct() {
        let slow = Fake {
            delay: Duration::from_millis(500),
            ..Fake::text(TWO_NODES)
        };
        let decision = run_with(&slow, &plain("hi there"), DataPolicy::OnlineAllowed).await;
        assert_eq!(reason(&decision), "model_timeout");
        for (failure, expected) in [
            (ModelFailure::Failed, "model_error"),
            (ModelFailure::NoModel, "no_eligible_model"),
        ] {
            let fake = Fake {
                reply: Err(failure),
                ..Fake::text("")
            };
            let decision = run_with(&fake, &plain("hi there"), DataPolicy::OnlineAllowed).await;
            assert_eq!(reason(&decision), expected);
        }
    }

    #[test]
    fn local_only_can_be_tightened_but_never_loosened() {
        let id = Uuid::nil();
        let ceiling_local = parse_decision(TWO_NODES.as_bytes(), id, DataPolicy::LocalOnly);
        let DecompositionDecision::Decomposed(graph) = ceiling_local else {
            panic!("expected a graph");
        };
        assert!(
            graph
                .nodes
                .iter()
                .all(|node| node.data_policy == DataPolicy::LocalOnly)
        );

        let first_local = TWO_NODES.replacen(
            r#""output":"text""#,
            r#""local_only":true,"output":"text""#,
            1,
        );
        let DecompositionDecision::Decomposed(graph) =
            parse_decision(first_local.as_bytes(), id, DataPolicy::OnlineAllowed)
        else {
            panic!("expected a graph");
        };
        assert!(
            graph
                .nodes
                .iter()
                .all(|node| node.data_policy == DataPolicy::LocalOnly),
            "dependent of a local node must stay local"
        );

        let online_ceiling = parse_decision(TWO_NODES.as_bytes(), id, DataPolicy::OnlineAllowed);
        let DecompositionDecision::Decomposed(graph) = online_ceiling else {
            panic!("expected a graph");
        };
        assert!(
            graph
                .nodes
                .iter()
                .all(|node| node.data_policy == DataPolicy::OnlineAllowed)
        );
    }

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Capture;
        fn make_writer(&'a self) -> Capture {
            self.clone()
        }
    }

    fn captured(capture: &Capture) -> String {
        String::from_utf8(capture.0.lock().unwrap().clone()).unwrap()
    }

    fn subscriber(capture: &Capture) -> impl tracing::Subscriber + Send + Sync {
        tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_ansi(false)
            .finish()
    }

    #[tokio::test]
    async fn logs_never_contain_the_prompt_or_model_output() {
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(subscriber(&capture));
        let prompt = plain(&format!("{SENTINEL} please review this"));
        let echoing = Fake::text(&format!("{SENTINEL} not json"));
        run_with(&echoing, &prompt, DataPolicy::OnlineAllowed).await;
        let tools = request(
            GatewayOperation::OpenAiResponses,
            &json!({"model":"public","input":SENTINEL,"tool_choice":"required"}).to_string(),
        );
        run_with(&echoing, &tools, DataPolicy::OnlineAllowed).await;
        let log = captured(&capture);
        assert!(
            log.contains("orchestration.decompose.direct"),
            "no log emitted: {log}"
        );
        assert!(!log.contains(SENTINEL), "prompt leaked into logs");
    }

    #[test]
    fn log_capture_detects_a_leak() {
        let capture = Capture::default();
        let _guard = tracing::subscriber::set_default(subscriber(&capture));
        tracing::info!(prompt = SENTINEL, "deliberate leak");
        assert!(captured(&capture).contains(SENTINEL));
    }

    fn for_tenant(mut request: CanonicalRequest, tenant_id: Uuid) -> CanonicalRequest {
        request.principal.tenant_id = tenant_id;
        request
    }

    async fn shadow_rows(pool: &PgPool, tenant_id: Uuid) -> Vec<(String, String, String)> {
        sqlx::query_as(
            "SELECT d.decision, d.reason, e.status
             FROM task_decompositions d
             JOIN orchestration_executions e
               ON e.tenant_id = d.tenant_id AND e.orchestration_id = d.orchestration_id
              AND e.execution_role = 'decomposer' AND e.task_id = '__decomposer__' AND e.attempt = 1
             WHERE d.tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn observe_once(pool: &PgPool, fake: &Fake, tenant_id: Uuid) {
        let input =
            DecompositionInput::from_request(&for_tenant(plain("review then test"), tenant_id));
        observe(
            pool,
            fake,
            &input,
            Uuid::new_v4(),
            Duration::from_millis(100),
        )
        .await;
    }

    #[sqlx::test]
    async fn tenants_are_disabled_unless_configured(pool: PgPool) {
        let tenant = Uuid::from_u128(901);
        let fake = Fake::text(TWO_NODES);
        observe_once(&pool, &fake, tenant).await;
        sqlx::query("INSERT INTO orchestration_configs (tenant_id) VALUES ($1)")
            .bind(tenant)
            .execute(&pool)
            .await
            .unwrap();
        observe_once(&pool, &fake, tenant).await;
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
        assert!(shadow_rows(&pool, tenant).await.is_empty());
    }

    #[sqlx::test]
    async fn shadow_records_metadata_only_and_isolates_tenants(pool: PgPool) {
        let (tenant, other) = (Uuid::from_u128(902), Uuid::from_u128(903));
        sqlx::query("INSERT INTO orchestration_configs (tenant_id, mode) VALUES ($1, 'shadow')")
            .bind(tenant)
            .execute(&pool)
            .await
            .unwrap();
        let fake = Fake::text(TWO_NODES);
        observe_once(&pool, &fake, tenant).await;
        assert_eq!(*fake.ceilings.lock().unwrap(), [DataPolicy::LocalOnly]);
        assert_eq!(
            shadow_rows(&pool, tenant).await,
            [(
                "decomposed".to_owned(),
                "decomposed".to_owned(),
                "completed".to_owned()
            )]
        );
        assert!(shadow_rows(&pool, other).await.is_empty());
        let stored: String = sqlx::query_scalar(
            "SELECT string_agg(to_jsonb(d)::text, ' ') FROM task_decompositions d",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            !stored.contains("review the code"),
            "objective stored: {stored}"
        );
        let columns: Vec<String> = sqlx::query_scalar(
            "SELECT column_name FROM information_schema.columns
             WHERE table_name IN ('task_decompositions', 'orchestration_executions')",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(
            columns
                .iter()
                .all(|name| !["prompt", "objective", "content", "output", "text"]
                    .iter()
                    .any(|banned| name.contains(banned))),
            "{columns:?}"
        );
    }

    #[sqlx::test]
    async fn failures_are_recorded_as_direct_with_a_reason_code(pool: PgPool) {
        let tenant = Uuid::from_u128(904);
        sqlx::query(
            "INSERT INTO orchestration_configs (tenant_id, mode, data_policy)
             VALUES ($1, 'shadow', 'online_allowed')",
        )
        .bind(tenant)
        .execute(&pool)
        .await
        .unwrap();
        let fake = Fake::text("not json");
        observe_once(&pool, &fake, tenant).await;
        assert_eq!(*fake.ceilings.lock().unwrap(), [DataPolicy::OnlineAllowed]);
        assert_eq!(
            shadow_rows(&pool, tenant).await,
            [(
                "direct".to_owned(),
                "bad_json".to_owned(),
                "failed".to_owned()
            )]
        );
    }

    #[test]
    fn local_only_data_is_limited_to_local_providers() {
        for kind in [
            ProviderKind::Anthropic,
            ProviderKind::OpenAi,
            ProviderKind::Gemini,
            ProviderKind::Xai,
        ] {
            assert!(!may_serve(DataPolicy::LocalOnly, kind));
            assert!(may_serve(DataPolicy::OnlineAllowed, kind));
        }
        for kind in [ProviderKind::Vllm, ProviderKind::Ollama] {
            assert!(may_serve(DataPolicy::LocalOnly, kind));
        }
    }
}
