//! Identity + capability verbs: whoami / capabilities.

use brain_db_sdk::wire::types::GetCapabilitiesResponse;
use brain_db_sdk::ConnectionInfo;
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
