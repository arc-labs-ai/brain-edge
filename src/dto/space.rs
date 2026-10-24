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
    /// Opaque caller metadata (quota hints, labels), sent as a UTF-8 string
    /// here and carried as bytes on the wire.
    ///
    /// WRITE-ONLY. Brain accepts and stores it, but no op returns it —
    /// neither `SPACE_CREATE`'s response nor `SPACE_LIST`'s view carries the
    /// blob — so a caller that needs to read its own metadata back has to
    /// keep it elsewhere. Said plainly because the obvious assumption is that
    /// something you can write is something you can read.
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
    /// The human-readable structured space string (e.g.
    /// `"support-bot:user123"`), NOT the derived 16-byte id.
    ///
    /// Named `space`, not `space_id`: `space_id` on this surface already means
    /// the derived id rendered as a UUID (see `WhoamiDto`), and one name for
    /// both values is what made the wire protocol's version of this field need
    /// a doc comment to say which one it was.
    pub space: String,
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
            space: r.space,
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
    /// The human-readable structured space string (e.g.
    /// `"support-bot:user123"`), NOT the derived 16-byte id.
    ///
    /// Named `space`, not `space_id`: `space_id` on this surface already means
    /// the derived id rendered as a UUID (see `WhoamiDto`), and one name for
    /// both values is what made the wire protocol's version of this field need
    /// a doc comment to say which one it was.
    pub space: String,
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
                    space: s.space,
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
    /// The human-readable structured space string (e.g.
    /// `"support-bot:user123"`), NOT the derived 16-byte id.
    ///
    /// Named `space`, not `space_id`: `space_id` on this surface already means
    /// the derived id rendered as a UUID (see `WhoamiDto`), and one name for
    /// both values is what made the wire protocol's version of this field need
    /// a doc comment to say which one it was.
    pub space: String,
    /// `false` when the space had no registry row — already erased, or never
    /// written to.
    pub existed: bool,
    pub memories_forgotten: u64,
}

impl From<SpaceDeleteResponse> for SpaceErasedDto {
    fn from(r: SpaceDeleteResponse) -> Self {
        Self {
            space: r.space,
            existed: r.existed,
            memories_forgotten: r.memories_forgotten,
        }
    }
}

#[cfg(test)]
mod space_tests {
    use super::*;

    fn view(id: &str, memories: u64) -> brain_db_sdk::wire::types::SpaceView {
        brain_db_sdk::wire::types::SpaceView {
            space: id.into(),
            created_at_unix_nanos: 1,
            last_active_unix_nanos: 2,
            memory_count: memories,
            session_count: 3,
        }
    }

    #[test]
    fn create_carries_metadata_as_bytes_and_leaves_act_as_to_the_pool() {
        let req = CreateSpaceBody {
            metadata: Some("tier=free".into()),
        }
        .to_request();
        assert_eq!(req.metadata.as_deref(), Some(b"tier=free".as_slice()));
        // The pool fills `act_as`; a DTO that set it would pin the wrong
        // tenant on the shared-service path.
        assert!(req.act_as.is_none());

        let bare = CreateSpaceBody::default().to_request();
        assert!(bare.metadata.is_none(), "absent, not an empty blob");
    }

    #[test]
    fn list_limit_defaults_and_clamps_rather_than_failing() {
        assert_eq!(
            ListSpacesQuery { limit: None }.to_request().limit,
            DEFAULT_LIST_LIMIT
        );
        // Zero is a caller saying nothing useful, not a request for no rows.
        assert_eq!(
            ListSpacesQuery { limit: Some(0) }.to_request().limit,
            DEFAULT_LIST_LIMIT
        );
        assert_eq!(ListSpacesQuery { limit: Some(7) }.to_request().limit, 7);
        assert_eq!(
            ListSpacesQuery {
                limit: Some(100_000)
            }
            .to_request()
            .limit,
            MAX_LIST_LIMIT,
            "an over-large ask is clamped, not rejected"
        );
    }

    #[test]
    fn provision_reports_which_path_it_took() {
        let made = SpaceProvisionDto::from(SpaceCreateResponse {
            space: "s".into(),
            created: true,
            created_at_unix_nanos: 1,
            last_active_unix_nanos: 2,
            memory_count: 0,
            session_count: 0,
        });
        assert!(made.created);

        let existed = SpaceProvisionDto::from(SpaceCreateResponse {
            space: "s".into(),
            created: false,
            created_at_unix_nanos: 1,
            last_active_unix_nanos: 2,
            memory_count: 4,
            session_count: 1,
        });
        assert!(
            !existed.created,
            "the create is idempotent; the flag is the only thing that \
             distinguishes made-it from already-there"
        );
    }

    #[test]
    fn a_partial_listing_says_so() {
        let page = ListSpacesResponseDto::from(SpaceListResponse {
            spaces: vec![view("a", 1), view("b", 2)],
            cross_shard_complete: false,
        });
        assert_eq!(page.spaces.len(), 2);
        assert_eq!(page.spaces[1].memory_count, 2);
        assert!(
            !page.cross_shard_complete,
            "passed through, not smoothed over: a partial listing presented \
             as complete is how a caller concludes a space is gone"
        );
    }

    #[test]
    fn an_erasure_of_nothing_is_reported_as_such() {
        let gone = SpaceErasedDto::from(SpaceDeleteResponse {
            space: "s".into(),
            existed: false,
            memories_forgotten: 0,
        });
        assert!(!gone.existed);
        assert_eq!(gone.memories_forgotten, 0);
    }

    #[test]
    fn the_json_field_names_are_the_contract() {
        let json = serde_json::to_value(SpaceProvisionDto::from(SpaceCreateResponse {
            space: "sp".into(),
            created: true,
            created_at_unix_nanos: 11,
            last_active_unix_nanos: 22,
            memory_count: 5,
            session_count: 2,
        }))
        .unwrap();
        for key in [
            // , not : the JSON name changed with the wire
            // name, so this guard is the thing that had to be updated on
            // purpose rather than a break discovered by a caller.
            "space",
            "created",
            "created_at_unix_nanos",
            "last_active_unix_nanos",
            "memory_count",
            "session_count",
        ] {
            assert!(json.get(key).is_some(), "missing `{key}` from the wire");
        }
    }
}
