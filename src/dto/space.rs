//! Space-lifecycle request/response DTOs — the public JSON contract for the
//! `/v1/spaces` verbs (provision, list, erase).
//!
//! A space is Brain's own first-class container (`SPACE_CREATE` / `SPACE_LIST`
//! / `SPACE_DELETE`, opcodes `0x0070`–`0x0072`). All three act on the CALLER's
//! effective space: Brain resolves `act_as` into `caller_namespace` /
//! `caller_space` before its handler runs and never trusts a client-supplied
//! scope. So none of these carry a target — the credential is the target, and
//! there is nothing for a path parameter to name.
//!
//! Timestamps cross the wire as `*_unix_nanos`, exactly as Brain reports them:
//! a caller comparing an edge answer against a native-wire one should see the
//! same numbers rather than a reformatting of them.

// The DTOs below mirror the HTTP contract one-for-one: the JSON field names
// are the API, and `tools/http_manifest.py` emits every one into
// `contract/http-routes.json`, which the three SDK clients are checked
// against.
#![allow(missing_docs)]

use brain_db_sdk::wire::types::{
    SpaceCreateRequest, SpaceCreateResponse, SpaceDeleteRequest, SpaceDeleteResponse,
    SpaceListRequest, SpaceListResponse,
};
use serde::{Deserialize, Serialize};

/// Default page size for `GET /v1/spaces` when the client omits `limit`.
const DEFAULT_LIST_LIMIT: u32 = 50;
/// Hard ceiling on the page size, so one request cannot pull an unbounded
/// registry scan.
const MAX_LIST_LIMIT: u32 = 500;

#[derive(Debug, Default, Deserialize)]
pub struct CreateSpaceBody {
    /// Opaque caller metadata (quota hints, labels) that Brain stores and
    /// echoes back. Sent as a UTF-8 string here and carried as bytes on the
    /// wire.
    #[serde(default)]
    pub metadata: Option<String>,
}

impl CreateSpaceBody {
    /// Build the wire request. `act_as` is filled in by the caller's pool.
    #[must_use]
    pub fn to_request(&self) -> SpaceCreateRequest {
        SpaceCreateRequest {
            metadata: self.metadata.as_ref().map(|m| m.as_bytes().to_vec()),
            request_id: brain_db_sdk::new_id(),
            act_as: None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct ListSpacesQuery {
    /// Page size. Clamped rather than rejected: a caller asking for more than
    /// the ceiling means "as many as you'll give me".
    #[serde(default)]
    pub limit: Option<u32>,
}

impl ListSpacesQuery {
    #[must_use]
    pub fn to_request(&self) -> SpaceListRequest {
        SpaceListRequest {
            limit: self
                .limit
                .filter(|n| *n > 0)
                .map_or(DEFAULT_LIST_LIMIT, |n| n.min(MAX_LIST_LIMIT)),
            act_as: None,
        }
    }
}

/// The wire request for an erasure. Carries no target for the reason in the
/// module docs.
#[must_use]
pub fn delete_space_request() -> SpaceDeleteRequest {
    SpaceDeleteRequest {
        request_id: brain_db_sdk::new_id(),
        act_as: None,
    }
}

#[derive(Debug, Serialize)]
pub struct SpaceProvisionDto {
    pub space_id: String,
    /// Whether THIS call created it. The op is idempotent, so a create for an
    /// existing space succeeds with `false` rather than failing — and a caller
    /// cannot tell the two apart from the rest of the body.
    pub created: bool,
    pub created_at_unix_nanos: u64,
    pub last_active_unix_nanos: u64,
    pub memory_count: u64,
    pub session_count: u32,
}

impl From<SpaceCreateResponse> for SpaceProvisionDto {
    fn from(r: SpaceCreateResponse) -> Self {
        Self {
            space_id: r.space_id,
            created: r.created,
            created_at_unix_nanos: r.created_at_unix_nanos,
            last_active_unix_nanos: r.last_active_unix_nanos,
            memory_count: r.memory_count,
            session_count: r.session_count,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SpaceSummaryDto {
    pub space_id: String,
    pub created_at_unix_nanos: u64,
    pub last_active_unix_nanos: u64,
    pub memory_count: u64,
    pub session_count: u32,
}

#[derive(Debug, Serialize)]
pub struct ListSpacesResponseDto {
    pub spaces: Vec<SpaceSummaryDto>,
    /// `false` when the listing covered only the caller's shard.
    ///
    /// Surfaced rather than smoothed over: v1 lists one shard, and a partial
    /// listing presented as complete is how a caller concludes a space is gone
    /// and acts on it.
    pub cross_shard_complete: bool,
}

impl From<SpaceListResponse> for ListSpacesResponseDto {
    fn from(r: SpaceListResponse) -> Self {
        Self {
            spaces: r
                .spaces
                .into_iter()
                .map(|s| SpaceSummaryDto {
                    space_id: s.space_id,
                    created_at_unix_nanos: s.created_at_unix_nanos,
                    last_active_unix_nanos: s.last_active_unix_nanos,
                    memory_count: s.memory_count,
                    session_count: s.session_count,
                })
                .collect(),
            cross_shard_complete: r.cross_shard_complete,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SpaceErasedDto {
    pub space_id: String,
    /// `false` when the space had no registry row — already erased, or never
    /// written to.
    pub existed: bool,
    pub memories_forgotten: u64,
}

impl From<SpaceDeleteResponse> for SpaceErasedDto {
    fn from(r: SpaceDeleteResponse) -> Self {
        Self {
            space_id: r.space_id,
            existed: r.existed,
            memories_forgotten: r.memories_forgotten,
        }
    }
}
