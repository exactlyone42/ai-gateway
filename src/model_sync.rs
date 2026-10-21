use std::{collections::HashSet, time::Duration};

use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use tokio::task::JoinHandle;
use uuid::Uuid;

const REFRESH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(FromRow)]
struct ProviderRow {
    id: Uuid,
    tenant_id: Uuid,
    provider_key: String,
    base_url: String,
    supported_operations: Vec<String>,
}

#[derive(Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TagModel>,
}

#[derive(Deserialize)]
struct TagModel {
    name: String,
}

#[derive(Deserialize)]
struct ShowResponse {
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    model_info: serde_json::Map<String, Value>,
}

struct DiscoveredModel {
    name: String,
    capabilities: Vec<String>,
    context_window: Option<i64>,
}

pub async fn refresh(pool: &PgPool, client: &Client) -> Result<usize, sqlx::Error> {
    let providers = sqlx::query_as::<_, ProviderRow>(
        "SELECT id, tenant_id, provider_key, base_url, supported_operations FROM providers WHERE lifecycle_state = 'active' AND provider_type = 'local' AND provider_key LIKE 'ollama%'",
    )
    .fetch_all(pool)
    .await?;
    let mut total = 0;
    for provider in providers {
        match discover(client, &provider.base_url).await {
            Ok(models) => {
                total += sync_provider(pool, &provider, &models).await?;
            }
            Err(error) => tracing::warn!(
                event.name = "model_catalog.refresh_failed",
                provider = %provider.provider_key,
                exception.message = %error,
                "model catalog refresh failed"
            ),
        }
    }
    sqlx::query("SELECT seed_model_task_profiles()")
        .execute(pool)
        .await?;
    tracing::info!(
        event.name = "model_catalog.refreshed",
        models = total,
        "model catalog refreshed"
    );
    Ok(total)
}

pub fn spawn(pool: PgPool, client: Client) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(REFRESH_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(error) = refresh(&pool, &client).await {
                tracing::warn!(event.name = "model_catalog.refresh_failed", exception.message = %error, "model catalog refresh failed");
            }
        }
    })
}

async fn discover(client: &Client, base_url: &str) -> Result<Vec<DiscoveredModel>, reqwest::Error> {
    let tags = client
        .get(format!("{}/api/tags", base_url.trim_end_matches('/')))
        .send()
        .await?
        .error_for_status()?
        .json::<TagsResponse>()
        .await?;
    let mut models = Vec::with_capacity(tags.models.len());
    for tag in tags.models.into_iter().take(256) {
        let show = client
            .post(format!("{}/api/show", base_url.trim_end_matches('/')))
            .json(&json!({"model": tag.name}))
            .send()
            .await?
            .error_for_status()?
            .json::<ShowResponse>()
            .await?;
        let context_window = show
            .model_info
            .iter()
            .find(|(key, _)| key.ends_with(".context_length"))
            .and_then(|(_, value)| value.as_i64())
            .filter(|value| *value > 0);
        models.push(DiscoveredModel {
            name: tag.name,
            capabilities: show.capabilities,
            context_window,
        });
    }
    Ok(models)
}

async fn sync_provider(
    pool: &PgPool,
    provider: &ProviderRow,
    models: &[DiscoveredModel],
) -> Result<usize, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let mut current = HashSet::new();
    for model in models {
        current.insert(model.name.clone());
        let public_model_id = format!("{}/{}", provider.provider_key, model.name);
        let mut capability_values = model.capabilities.clone();
        capability_values.push("auto_discovered".to_owned());
        let capabilities = json!(capability_values);
        let has = |capability: &str| model.capabilities.iter().any(|item| item == capability);
        sqlx::query(
            r#"
            INSERT INTO models (
                id, tenant_id, provider_id, public_model_id, provider_model_id,
                display_name, description, quality_tier, context_window,
                max_output_tokens, supported_operations, capabilities,
                supports_streaming, supports_tools, supports_reasoning,
                supports_images, supports_structured_output, cost_metadata,
                lifecycle_state
            ) VALUES (
                $1, $2, $3, $4, $5, $5, $6, $7, $8, 8192, $9, $10,
                true, $11, $12, $13, false, '{}'::jsonb, 'active'
            )
            ON CONFLICT (tenant_id, public_model_id) DO UPDATE SET
                provider_model_id = EXCLUDED.provider_model_id,
                context_window = EXCLUDED.context_window,
                supported_operations = EXCLUDED.supported_operations,
                capabilities = EXCLUDED.capabilities,
                supports_streaming = EXCLUDED.supports_streaming,
                supports_tools = EXCLUDED.supports_tools,
                supports_reasoning = EXCLUDED.supports_reasoning,
                supports_images = EXCLUDED.supports_images,
                lifecycle_state = 'active', version = models.version + 1,
                updated_at = now()
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(provider.tenant_id)
        .bind(provider.id)
        .bind(public_model_id)
        .bind(&model.name)
        .bind(format!(
            "Ollama model {}; capabilities: {}",
            model.name,
            model.capabilities.join(", ")
        ))
        .bind(Option::<String>::None)
        .bind(model.context_window)
        .bind(&provider.supported_operations)
        .bind(capabilities)
        .bind(has("tools"))
        .bind(has("thinking"))
        .bind(has("vision"))
        .execute(&mut *transaction)
        .await?;
    }
    let current = current.into_iter().collect::<Vec<_>>();
    sqlx::query(
        r#"
        UPDATE models SET lifecycle_state = 'disabled', version = version + 1, updated_at = now()
        WHERE tenant_id = $1 AND provider_id = $2
          AND capabilities ? 'auto_discovered'
          AND NOT (provider_model_id = ANY($3))
        "#,
    )
    .bind(provider.tenant_id)
    .bind(provider.id)
    .bind(&current)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(models.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ollama_show_capabilities_are_typed() {
        let show: ShowResponse = serde_json::from_value(json!({
            "capabilities": ["completion", "vision", "tools", "thinking"],
            "model_info": {"qwen.context_length": 32768}
        }))
        .unwrap();
        assert!(show.capabilities.iter().any(|item| item == "vision"));
        assert_eq!(show.model_info["qwen.context_length"], 32768);
    }
}
