//! Session-lifecycle request/response DTOs — the public JSON contract for the
//! `/v1/sessions` verbs (provision, list, erase).
//!
//! A session is a conversation-scoped grouping inside a space
//! (`SESSION_CREATE` / `SESSION_LIST` / `SESSION_DELETE`, opcodes `0x0073`–
//! `0x0075`). Like the space verbs, these run against the caller's effective
//! space, resolved from `act_as`; unlike them, a session has an id of its own,
//! so create and delete name one.
//!
//! A session id is a `u64` on the wire and a JSON number here — NOT a UUID.
//! It is the same id `EncodeBody.session` already carries, so a memory and the
//! session it was written into share one representation.

// Field names are the API; see `dto/space.rs` for the manifest note.
#![allow(missing_docs)]

use brain_db_sdk::wire::types::{
    SessionCreateRequest, SessionCreateResponse, SessionDeleteRequest, SessionDeleteResponse,
    SessionListRequest, SessionListResponse,
};
use serde::{Deserialize, Serialize};

/// Default page size for `GET /v1/sessions` when the client omits `limit`.
const DEFAULT_LIST_LIMIT: u32 = 50;
/// Hard ceiling on the page size.
const MAX_LIST_LIMIT: u32 = 500;

#[derive(Debug, Deserialize)]
pub struct CreateSessionBody {
    /// The session id to provision. Client-chosen so a caller can make its
    /// own conversation id the session id rather than keeping a mapping; the
    /// op is idempotent on it.
    pub session_id: u64,
    /// Optional human label.
    #[serde(default)]
    pub title: Option<String>,
}

impl CreateSessionBody {
    /// Build the wire request.
    #[must_use]
    pub fn to_request(&self) -> SessionCreateRequest {
        SessionCreateRequest {
            session_id: self.session_id,
            title: self.title.clone(),
            request_id: brain_db_sdk::new_id(),
            act_as: None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ListSessionsQuery {
    /// Page size. Clamped rather than rejected.
    #[serde(default)]
    pub limit: Option<u32>,
}

impl ListSessionsQuery {
    #[must_use]
    pub fn to_request(&self) -> SessionListRequest {
        SessionListRequest {
            limit: self
                .limit
                .filter(|n| *n > 0)
                .map_or(DEFAULT_LIST_LIMIT, |n| n.min(MAX_LIST_LIMIT)),
            act_as: None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct DeleteSessionQuery {
    /// Hard erasure rather than a soft tombstone. Defaults to soft, so the
    /// destructive form has to be asked for by name.
    #[serde(default)]
    pub hard: bool,
}

/// Build the wire delete request for `id`.
#[must_use]
pub fn delete_session_request(id: u64, query: &DeleteSessionQuery) -> SessionDeleteRequest {
    SessionDeleteRequest {
        session_id: id,
        hard: query.hard,
        request_id: brain_db_sdk::new_id(),
        act_as: None,
    }
}

#[derive(Debug, Serialize)]
pub struct SessionProvisionDto {
    pub space_id: String,
    pub session_id: u64,
    /// Whether THIS call created it; the op is idempotent.
    pub created: bool,
    pub created_at_unix_nanos: u64,
    pub last_active_unix_nanos: u64,
    pub memory_count: u32,
}

impl From<SessionCreateResponse> for SessionProvisionDto {
    fn from(r: SessionCreateResponse) -> Self {
        Self {
            space_id: crate::dto::uuid_string(&r.space_id),
            session_id: r.session_id,
            created: r.created,
            created_at_unix_nanos: r.created_at_unix_nanos,
            last_active_unix_nanos: r.last_active_unix_nanos,
            memory_count: r.memory_count,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SessionSummaryDto {
    pub session_id: u64,
    pub created_at_unix_nanos: u64,
    pub last_active_unix_nanos: u64,
    pub title: Option<String>,
    pub memory_count: u32,
}

#[derive(Debug, Serialize)]
pub struct ListSessionsResponseDto {
    pub space_id: String,
    pub sessions: Vec<SessionSummaryDto>,
}

impl From<SessionListResponse> for ListSessionsResponseDto {
    fn from(r: SessionListResponse) -> Self {
        Self {
            space_id: crate::dto::uuid_string(&r.space_id),
            sessions: r
                .sessions
                .into_iter()
                .map(|s| SessionSummaryDto {
                    session_id: s.session_id,
                    created_at_unix_nanos: s.created_at_unix_nanos,
                    last_active_unix_nanos: s.last_active_unix_nanos,
                    title: s.title,
                    memory_count: s.memory_count,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SessionErasedDto {
    pub space_id: String,
    pub session_id: u64,
    /// `false` when the session had no row — already erased, or never written
    /// to.
    pub existed: bool,
    pub memories_forgotten: u64,
}

impl From<SessionDeleteResponse> for SessionErasedDto {
    fn from(r: SessionDeleteResponse) -> Self {
        Self {
            space_id: crate::dto::uuid_string(&r.space_id),
            session_id: r.session_id,
            existed: r.existed,
            memories_forgotten: r.memories_forgotten,
        }
    }
}
