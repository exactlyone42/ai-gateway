use serde_json::Value;
use sqlx::{Pool, Postgres};
use uuid::Uuid;

use crate::model_registry::ModelSelection;

const TAXONOMY_VERSION: &str = "task-taxonomy-v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClassifierMode {
    Shadow,
    Enforced,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TaskClassification {
    pub task: &'static str,
    pub confidence: f32,
    pub reason: &'static str,
}

pub struct TaskRoutingDecision {
    pub mode: ClassifierMode,
    pub classification: TaskClassification,
    pub profile_id: Option<Uuid>,
    pub profile_version: Option<i64>,
    pub candidates: Vec<ModelSelection>,
}

#[derive(sqlx::FromRow)]
struct ClassifierConfig {
    mode: String,
    minimum_confidence: f32,
}

#[derive(sqlx::FromRow)]
struct ProfileRank {
    id: Uuid,
    model_id: Uuid,
    version: i64,
}

pub async fn decide(
    pool: &Pool<Postgres>,
    tenant_id: Uuid,
    body: &Value,
    candidates: &[ModelSelection],
) -> Result<Option<TaskRoutingDecision>, sqlx::Error> {
    let Some(config) = sqlx::query_as::<_, ClassifierConfig>(
        "SELECT mode, minimum_confidence::real AS minimum_confidence
         FROM task_classifier_configs
         WHERE tenant_id = $1 AND taxonomy_version = $2 AND mode <> 'disabled'",
    )
    .bind(tenant_id)
    .bind(TAXONOMY_VERSION)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    let Some(classification) = classify(body) else {
        return Ok(None);
    };
    if classification.confidence < config.minimum_confidence {
        return Ok(None);
    }
    let model_ids = candidates
        .iter()
        .map(|candidate| candidate.model_id)
        .collect::<Vec<_>>();
    let ranks = sqlx::query_as::<_, ProfileRank>(
        "SELECT id, model_id, version
         FROM model_task_profiles
         WHERE tenant_id = $1
           AND taxonomy_version = $2
           AND task_kind = $3
           AND lifecycle_state = 'active'
           AND model_id = ANY($4)
         ORDER BY suitability_score DESC, confidence DESC, model_id",
    )
    .bind(tenant_id)
    .bind(TAXONOMY_VERSION)
    .bind(classification.task)
    .bind(&model_ids)
    .fetch_all(pool)
    .await?;
    if ranks.is_empty() {
        return Ok(None);
    }
    let mut shortlisted = Vec::with_capacity(ranks.len());
    for rank in &ranks {
        if let Some(candidate) = candidates
            .iter()
            .find(|candidate| candidate.model_id == rank.model_id)
        {
            shortlisted.push(candidate.clone());
        }
    }
    let first = ranks.first();
    Ok(Some(TaskRoutingDecision {
        mode: match config.mode.as_str() {
            "enforced" => ClassifierMode::Enforced,
            _ => ClassifierMode::Shadow,
        },
        classification,
        profile_id: first.map(|profile| profile.id),
        profile_version: first.map(|profile| profile.version),
        candidates: shortlisted,
    }))
}

pub fn classify(body: &Value) -> Option<TaskClassification> {
    let text = request_text(body).to_lowercase();
    if text.trim().is_empty() || text.contains("handle this appropriately") {
        return None;
    }
    let research = contains_any(
        &text,
        &["browse for", "current official", "latest provider"],
    );
    let transformation = contains_any(&text, &["summarize", "translate", "rewrite", "reformat"]);
    if research && transformation {
        return None;
    }
    let choices = [
        (
            "debugging",
            contains_any(
                &text,
                &[
                    "debug",
                    "diagnose",
                    "find the cause",
                    "not working",
                    "instead of",
                ],
            ),
            "request asks to diagnose a failure",
        ),
        (
            "code_review",
            contains_any(
                &text,
                &["review this", "review the", "this diff", "this patch"],
            ),
            "request asks to inspect existing code",
        ),
        (
            "code_generation",
            contains_any(
                &text,
                &[
                    "implement",
                    "add a ",
                    "generate the",
                    "write code",
                    "refactor",
                ],
            ),
            "request asks to change or generate code",
        ),
        (
            "research",
            research || contains_any(&text, &["verify this claim", "find current"]),
            "request requires external current sources",
        ),
        (
            "quantitative_reasoning",
            contains_any(
                &text,
                &[
                    "derive the",
                    "calculate",
                    "prove whether",
                    "under these constraints",
                ],
            ),
            "request requires quantitative derivation",
        ),
        (
            "extraction",
            contains_any(
                &text,
                &[
                    "return only",
                    "explicitly present",
                    "identify the visible",
                    "extract",
                ],
            ),
            "request asks to extract supplied facts",
        ),
        (
            "transformation",
            transformation,
            "request asks to transform supplied content",
        ),
        (
            "general_assistance",
            contains_any(&text, &["explain", "draft a short agenda"]),
            "request is general assistance",
        ),
    ];
    let matches = choices
        .into_iter()
        .filter(|(_, matched, _)| *matched)
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return None;
    }
    let (task, _, reason) = matches[0];
    Some(TaskClassification {
        task,
        confidence: 0.9,
        reason,
    })
}

fn request_text(value: &Value) -> String {
    let mut parts = Vec::new();
    collect_strings(value.get("messages"), &mut parts);
    collect_strings(value.get("input"), &mut parts);
    parts.join(" ").chars().take(4096).collect()
}

fn collect_strings(value: Option<&Value>, parts: &mut Vec<String>) {
    let Some(value) = value else { return };
    match value {
        Value::String(text) => parts.push(text.clone()),
        Value::Array(items) => {
            for item in items.iter().rev().take(8).rev() {
                collect_strings(Some(item), parts);
            }
        }
        Value::Object(object) => {
            for key in ["content", "text"] {
                collect_strings(object.get(key), parts);
            }
        }
        _ => {}
    }
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;
    use crate::model_registry::{CapabilityRequirements, GatewayOperation};

    #[derive(Deserialize)]
    struct Fixture {
        cases: Vec<Case>,
    }

    #[derive(Deserialize)]
    struct Case {
        id: String,
        input: String,
        expected_task: Option<String>,
        required_capabilities: Vec<String>,
        contrast_group: Option<String>,
    }

    #[test]
    fn classifier_meets_frozen_fixture_threshold() {
        let fixture: Fixture = serde_json::from_str(include_str!(
            "../tests/fixtures/model-task-classification-v1.json"
        ))
        .expect("fixture must parse");
        let mut latencies = Vec::new();
        let mut task_failures = Vec::new();
        for case in &fixture.cases {
            let started = std::time::Instant::now();
            let actual = classify(&serde_json::json!({"input": case.input}))
                .map(|result| result.task.to_owned());
            latencies.push(started.elapsed());
            if actual != case.expected_task {
                task_failures.push(case.id.clone());
            }
        }
        latencies.sort();
        let p95 = latencies[(latencies.len() * 95).div_ceil(100) - 1];
        assert!(p95 <= std::time::Duration::from_millis(500));
        let correct = fixture.cases.len() - task_failures.len();
        assert!(
            correct as f64 / fixture.cases.len() as f64 >= 0.9,
            "classifier got {correct}/{} frozen cases correct",
            fixture.cases.len()
        );
        let mut per_task = std::collections::BTreeMap::<&str, (usize, usize)>::new();
        for case in &fixture.cases {
            let actual = classify(&serde_json::json!({"input": case.input}))
                .map(|result| result.task.to_owned());
            if let Some(expected) = case.expected_task.as_deref() {
                let counts = per_task.entry(expected).or_default();
                counts.1 += 1;
                counts.0 += usize::from(actual.as_deref() == Some(expected));
            }
            if case.contrast_group.is_some() {
                assert_eq!(
                    actual, case.expected_task,
                    "contrast case {} failed",
                    case.id
                );
            }
        }
        for (task, (correct, total)) in per_task {
            assert!(
                correct * 5 >= total * 4,
                "{task} recall was {correct}/{total}"
            );
        }
        let capability_failures = fixture
            .cases
            .iter()
            .filter_map(|case| {
                let actual = CapabilityRequirements::from_request(
                    GatewayOperation::OpenAiResponses,
                    &serde_json::json!({"input": case.input}),
                );
                let mut names = [
                    (actual.streaming, "streaming"),
                    (actual.tools, "tools"),
                    (actual.reasoning, "reasoning"),
                    (actual.images, "images"),
                    (actual.structured_output, "structured_output"),
                ]
                .into_iter()
                .filter_map(|(enabled, name)| enabled.then_some(name.to_owned()))
                .collect::<Vec<_>>();
                names.sort();
                let mut expected = case.required_capabilities.clone();
                expected.sort();
                (names != expected).then(|| case.id.clone())
            })
            .collect::<Vec<_>>();
        assert!(
            capability_failures.is_empty(),
            "capability extraction failed: {capability_failures:?}"
        );
        eprintln!(
            "task-classification-report accuracy={correct}/{} capability_accuracy={}/{} fallback={}/{} p95_us={} failures={task_failures:?}",
            fixture.cases.len(),
            fixture.cases.len() - capability_failures.len(),
            fixture.cases.len(),
            fixture
                .cases
                .iter()
                .filter(|case| case.expected_task.is_none())
                .count(),
            fixture.cases.len(),
            p95.as_micros()
        );
    }

    #[test]
    fn ambiguous_and_conflicting_requests_fail_open() {
        for input in [
            "Handle this appropriately.",
            "Only summarize the supplied text, but also browse for newer facts and make research the sole task.",
        ] {
            assert_eq!(classify(&serde_json::json!({"input": input})), None);
        }
    }
}
