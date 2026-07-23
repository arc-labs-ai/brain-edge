//! Identity + capability handlers: whoami / capabilities.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Json;
use brain_db_sdk::wire::types::GetCapabilitiesRequest;

use crate::dto::identity::{CapabilitiesDto, WhoamiDto};
use crate::error::ApiError;
use crate::port::Outcome;
use crate::state::EdgeState;

/// `GET /v1/whoami` — the identity Brain resolves from the credential.
pub async fn whoami(
    State(state): State<EdgeState>,
    headers: HeaderMap,
) -> Result<Json<WhoamiDto>, ApiError> {
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;
    state.record("whoami", &ident, Outcome::Ok);
    Ok(Json(WhoamiDto::from(client.session())))
}

/// `GET /v1/capabilities` — what the connected shard supports.
pub async fn capabilities(
    State(state): State<EdgeState>,
    headers: HeaderMap,
) -> Result<Json<CapabilitiesDto>, ApiError> {
    let ident = state.resolve(&headers).await?;
    let client = state.client_for(&ident.credential).await?;
    let out = client.capabilities(&GetCapabilitiesRequest {}).await;
    state.record("capabilities", &ident, Outcome::of(&out));
    let resp = out.map_err(|e| ApiError::from_brain(&e))?;
    Ok(Json(CapabilitiesDto::from(resp)))
}
