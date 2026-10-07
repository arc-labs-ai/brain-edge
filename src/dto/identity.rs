//! Identity + capability verbs: whoami / capabilities.

// The DTOs below mirror the HTTP contract one-for-one: the JSON field names are
// the API, and `tools/http_manifest.py` emits every one of them — with its type
// and serde attributes — into `contract/http-routes.json`, which the three SDK
// clients are checked against. A doc comment on each of ~312 fields would
// restate the field name; the ones that carry meaning beyond their name have
// one written below.
#![allow(missing_docs)]

use brain_db_sdk::ConnectionInfo;
use brain_db_sdk::wire::types::GetCapabilitiesResponse;
use serde::Serialize;

use super::uuid_string;

/// `GET /v1/whoami` response — the identity Brain resolved from the credential.
#[derive(Debug, Serialize)]
pub struct WhoamiDto {
    pub namespace: String,
    pub space_id: String,
    pub permissions: PermissionsDto,
}

/// The resolved permission flags.
#[derive(Debug, Serialize)]
pub struct PermissionsDto {
    pub can_encode: bool,
    pub can_recall: bool,
    pub can_plan: bool,
    pub can_reason: bool,
    pub can_forget: bool,
    pub can_admin: bool,
}

impl From<&ConnectionInfo> for WhoamiDto {
    fn from(s: &ConnectionInfo) -> Self {
        let p = &s.permissions;
        Self {
            namespace: s.namespace.clone(),
            space_id: uuid_string(&s.space_id),
            permissions: PermissionsDto {
                can_encode: p.can_encode,
                can_recall: p.can_recall,
                can_plan: p.can_plan,
                can_reason: p.can_reason,
                can_forget: p.can_forget,
                can_admin: p.can_admin,
            },
        }
    }
}

/// `GET /v1/capabilities` response — what the connected shard supports.
#[derive(Debug, Serialize)]
pub struct CapabilitiesDto {
    pub rerank: bool,
    pub llm_extractor: bool,
    pub classifier_extractor: bool,
    pub pattern_extractor: bool,
    pub schema_namespaces: Vec<String>,
    pub vector_dim: u16,
}

impl From<GetCapabilitiesResponse> for CapabilitiesDto {
    fn from(r: GetCapabilitiesResponse) -> Self {
        let c = r.capabilities;
        Self {
            rerank: c.rerank,
            llm_extractor: c.llm_extractor,
            classifier_extractor: c.classifier_extractor,
            pattern_extractor: c.pattern_extractor,
            schema_namespaces: c.schema_namespaces,
            vector_dim: c.vector_dim,
        }
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    use brain_db_sdk::wire::types::{AuthMethod, Capabilities, ServerFeatures, SpacePermissions};

    fn sample_connection(space_id: [u8; 16]) -> ConnectionInfo {
        ConnectionInfo {
            space_id,
            server_id: "mock-brain".into(),
            chosen_version: 1,
            connection_id: [0xAB; 16],
            bound_shard_id: 0,
            permissions: SpacePermissions {
                can_encode: true,
                can_recall: true,
                can_plan: false,
                can_reason: false,
                can_forget: true,
                can_admin: false,
                can_act_as: false,
            },
            namespace: "acme".into(),
            server_features: ServerFeatures {
                max_payload_size: 1 << 20,
                max_concurrent_streams: 64,
                idle_timeout_seconds: 300,
                auth_methods: vec![AuthMethod::Token],
            },
        }
    }

    #[test]
    fn whoami_renders_space_id_as_uuid_and_maps_permissions() {
        let mut space_id = [0u8; 16];
        space_id[0] = 0x11;
        space_id[15] = 0x2a;
        let dto = WhoamiDto::from(&sample_connection(space_id));

        assert_eq!(dto.namespace, "acme");
        // The connection's 16-byte `space_id` is surfaced as a UUID string under
        // the `space_id` JSON field (the historical agent_id -> space_id rename).
        assert_eq!(dto.space_id, uuid_string(&space_id));
        assert_eq!(dto.space_id, "11000000-0000-0000-0000-00000000002a");

        assert!(dto.permissions.can_encode);
        assert!(dto.permissions.can_recall);
        assert!(!dto.permissions.can_plan);
        assert!(!dto.permissions.can_reason);
        assert!(dto.permissions.can_forget);
        assert!(!dto.permissions.can_admin);
    }

    #[test]
    fn capabilities_dto_maps_every_flag() {
        let dto = CapabilitiesDto::from(GetCapabilitiesResponse {
            capabilities: Capabilities {
                rerank: true,
                llm_extractor: false,
                classifier_extractor: true,
                pattern_extractor: true,
                schema_namespaces: vec!["brain".into(), "people".into()],
                vector_dim: 384,
            },
        });
        assert!(dto.rerank);
        assert!(!dto.llm_extractor);
        assert!(dto.classifier_extractor);
        assert!(dto.pattern_extractor);
        assert_eq!(dto.schema_namespaces, vec!["brain", "people"]);
        assert_eq!(dto.vector_dim, 384);
    }
}
