//! Identity + capability handlers: whoami / capabilities.

use std::time::Instant;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Json;
use brain_db_sdk::wire::types::GetCapabilitiesRequest;

use crate::dto::identity::{CapabilitiesDto, WhoamiDto};
use crate::error::ApiError;
use crate::port::{Op, Outcome};
use crate::state::EdgeState;

/// `GET /v1/whoami` — the identity Brain resolves from the credential.
pub async fn whoami(
    State(state): State<EdgeState>,
    headers: HeaderMap,
) -> Result<Json<WhoamiDto>, ApiError> {
    let ident = state.resolve(&headers).await?;
    state.admit(Op::Whoami, &ident).await?;
    let client = state.client_for(&ident.credential).await?;
    state.record_untimed(Op::Whoami, &ident, Outcome::Ok);
    Ok(Json(WhoamiDto::for_identity(&ident, client.connection())))
}

/// `GET /v1/capabilities` — what the connected shard supports.
pub async fn capabilities(
    State(state): State<EdgeState>,
    headers: HeaderMap,
) -> Result<Json<CapabilitiesDto>, ApiError> {
    let ident = state.resolve(&headers).await?;
    state.admit(Op::Capabilities, &ident).await?;
    let client = state.client_for(&ident.credential).await?;
    let mut req = GetCapabilitiesRequest::default();
    ident.scope(&mut req);
    let started = Instant::now();
    let out = client.capabilities(&req).await;
    state.record(
        Op::Capabilities,
        &ident,
        Outcome::of(&out),
        started.elapsed(),
    );
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(CapabilitiesDto::from(resp)))
}
