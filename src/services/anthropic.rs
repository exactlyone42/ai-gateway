use axum::http::{HeaderMap, HeaderName, HeaderValue, header};

use crate::{error::GatewayError, logging, model_registry::ProviderProfile, state::AppState};

pub struct UpstreamResponse {
    pub response: reqwest::Response,
}

/// Sends one native Anthropic request after the router has replaced only the
/// public model selector with the provider model id.
#[tracing::instrument(
    name = "provider.attempt",
    skip_all,
    fields(gen_ai.system = %profile.provider_key, gen_ai.request.model = %provider_model_id)
)]
pub async fn forward(
    state: &AppState,
    path_and_query: &str,
    client_headers: &HeaderMap,
    body: axum::body::Bytes,
    profile: &ProviderProfile,
    provider_model_id: &str,
) -> Result<UpstreamResponse, GatewayError> {
    let mut headers = provider_headers(client_headers);
    logging::inject_context(&mut headers);
    headers.insert(
        HeaderName::from_static("x-api-key"),
        HeaderValue::from_str(
            profile
                .api_key_secret()
                .ok_or(GatewayError::InvalidConfiguration)?,
        )
        .map_err(|_| GatewayError::InvalidConfiguration)?,
    );

    let response = state
        .client
        .post(format!("{}{}", profile.base_url, path_and_query))
        .headers(headers)
        .body(body)
        .send()
        .await?;
    Ok(UpstreamResponse { response })
}

fn provider_headers(client: &HeaderMap) -> HeaderMap {
    let mut outgoing = HeaderMap::new();
    for (name, value) in client {
        let allowed = name == header::CONTENT_TYPE
            || name == header::ACCEPT
            || name.as_str().starts_with("anthropic-");
        if allowed && name != header::AUTHORIZATION && name.as_str() != "x-api-key" {
            outgoing.append(name.clone(), value.clone());
        }
    }
    outgoing
}

#[cfg(test)]
mod tests {
    use super::provider_headers;
    use axum::http::HeaderMap;

    #[test]
    fn forwards_open_anthropic_headers_but_never_client_credentials() {
        let mut incoming = HeaderMap::new();
        incoming.insert("authorization", "Bearer client-secret".parse().unwrap());
        incoming.insert("x-api-key", "client-secret".parse().unwrap());
        incoming.insert("anthropic-version", "2023-06-01".parse().unwrap());
        incoming.insert("anthropic-beta", "one,two".parse().unwrap());
        incoming.insert("content-type", "application/json".parse().unwrap());

        let outgoing = provider_headers(&incoming);
        assert!(outgoing.get("authorization").is_none());
        assert!(outgoing.get("x-api-key").is_none());
        assert_eq!(outgoing["anthropic-version"], "2023-06-01");
        assert_eq!(outgoing["anthropic-beta"], "one,two");
    }
}
