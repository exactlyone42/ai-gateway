use std::sync::Arc;

use axum::{
    Extension,
    extract::{OriginalUri, State},
    http::HeaderMap,
    response::Response,
};

use crate::{
    error::GatewayError,
    identity::PrincipalContext,
    middleware::RequestReceivedAt,
    routes::{BodyBytes, StreamingResponse, body_bytes},
    services::openai::{self, Operation, RequestContext},
    state::AppState,
};

pub async fn handle(
    State(state): State<Arc<AppState>>,
    Extension(principal): Extension<PrincipalContext>,
    Extension(received_at): Extension<RequestReceivedAt>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: BodyBytes,
) -> Result<Response, GatewayError> {
    let body = body_bytes(body)?;
    let reply = openai::execute(
        &state,
        RequestContext {
            principal,
            received_at: received_at.0,
            path_and_query: uri
                .path_and_query()
                .map(|value| value.as_str())
                .unwrap_or(uri.path())
                .to_owned(),
            headers,
            body,
            operation: Operation::Responses,
        },
    )
    .await?;
    build_response(reply)
}

fn build_response(reply: openai::Reply) -> Result<Response, GatewayError> {
    let mut builder = Response::builder().status(reply.status);
    for (name, value) in &reply.headers {
        builder = builder.header(name, value);
    }
    let mut response = builder.body(reply.body)?;
    if reply.streaming {
        response.extensions_mut().insert(StreamingResponse);
    }
    Ok(response)
}
