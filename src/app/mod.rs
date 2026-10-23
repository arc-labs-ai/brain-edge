//! The HTTP surface: the data-plane router.

pub mod handlers;

use axum::Router;
use axum::routing::get;
use axum::routing::{delete, post};

use crate::state::EdgeState;

/// Build the data-plane router (`/v1/*`), with [`EdgeState`] baked in.
///
/// This is the reusable half: the self-host binary adds its own health routes
/// around it (see [`crate::run`]); the hosted gateway merges it under its auth +
/// metering middleware alongside its control-plane routes. It deliberately does
/// NOT register `/health/*`, so a host app can own health without colliding.
///
/// # What this router does NOT include
///
/// No transport-level protection is attached here — not the request-body cap,
/// the request timeout, the credential-header redaction, or tracing. An
/// embedder must apply them, and [`crate::harden`] does exactly that.
///
/// The body cap is the one most easily missed: [`crate::EdgeConfig`] exposes
/// `max_body_bytes` as a validated knob, which makes it reasonable to assume
/// the router already honours it. It does not.
/// Which planes a given process serves.
///
/// One binary, deployed twice. The data plane carries every tenant's memory
/// traffic; the credential plane is the only place a plaintext provider key
/// exists in memory, and it sits on the LLM path with a completely different
/// latency profile.
///
/// Running both in one process is supported, and is the default — standing up
/// two deployments just to try BYOK would be absurd. Production should split
/// them: it keeps every tenant's keys out of the busiest process and lets the
/// two scale on their own shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Plane {
    /// Memory verbs plus credentials and the broker. The default.
    #[default]
    Both,
    /// Memory verbs only. No key material is reachable in this process.
    Data,
    /// BYOK credential management and the broker only.
    Credential,
}

/// Env var selecting the plane this process serves.
pub const PLANE_ENV: &str = "BRAIN_EDGE_PLANE";

impl Plane {
    /// Parse a [`PLANE_ENV`] value.
    ///
    /// # Errors
    /// Names the accepted set. An unrecognised plane is a hard error rather
    /// than a silent fallback to `Both`: quietly serving credentials from a
    /// process an operator meant to be data-only is the exact mistake this
    /// flag exists to prevent.
    pub fn from_name(name: &str) -> Result<Self, String> {
        match name.trim().to_ascii_lowercase().as_str() {
            "both" | "" => Ok(Self::Both),
            "data" => Ok(Self::Data),
            "credential" => Ok(Self::Credential),
            other => Err(format!(
                "unknown plane `{other}`: expected one of both, data, credential"
            )),
        }
    }

    /// Read the plane from the environment.
    ///
    /// # Errors
    /// As [`Self::from_name`].
    pub fn from_env() -> Result<Self, String> {
        std::env::var(PLANE_ENV).map_or(Ok(Self::Both), |v| Self::from_name(&v))
    }

    /// Whether this plane serves the memory verbs.
    #[must_use]
    pub const fn serves_data(self) -> bool {
        matches!(self, Self::Both | Self::Data)
    }

    /// Whether this plane serves credentials and the broker.
    #[must_use]
    pub const fn serves_credentials(self) -> bool {
        matches!(self, Self::Both | Self::Credential)
    }
}

/// Build the router for the default (`Both`) plane.
pub fn router(state: EdgeState) -> Router {
    router_for(state, Plane::Both)
}

/// Build the router for one plane.
pub fn router_for(state: EdgeState, plane: Plane) -> Router {
    let mut r = Router::new();
    if plane.serves_credentials() {
        r = r.merge(credential_router());
    }
    if plane.serves_data() {
        r = r.merge(data_router());
    }
    r.with_state(state)
}

/// BYOK credential management plus the broker.
///
/// Scoped to the caller's own namespace, which is read from Brain rather than
/// from the request — see the handler module. Registered even without a KEK,
/// where they return `501`: that tells a tenant "this deployment does not do
/// BYOK" instead of a 404 that looks like a client-side URL mistake.
fn credential_router() -> Router<EdgeState> {
    Router::new()
        .route(
            "/v1/llm-credential",
            axum::routing::put(handlers::llm_credential::put)
                .get(handlers::llm_credential::get)
                .delete(handlers::llm_credential::delete),
        )
        // The broker. Paths mirror each provider's native shape so Brain's
        // existing provider clients work by pointing `base_url` here.
        .route(
            "/llm/ns/{namespace}/openai/v1/chat/completions",
            post(handlers::llm_proxy::openai_chat_completions),
        )
        .route(
            "/llm/ns/{namespace}/anthropic/v1/messages",
            post(handlers::llm_proxy::anthropic_messages),
        )
        .route(
            "/llm/ns/{namespace}/resolve",
            get(handlers::llm_proxy::resolve),
        )
}

/// The memory verbs.
fn data_router() -> Router<EdgeState> {
    Router::new()
        // identity
        .route("/v1/whoami", get(handlers::identity::whoami))
        .route("/v1/capabilities", get(handlers::identity::capabilities))
        // memory
        .route(
            "/v1/memories",
            post(handlers::memory::encode)
                .get(handlers::memory::list)
                .delete(handlers::memory::forget),
        )
        .route("/v1/memories/{id}/inspect", get(handlers::memory::inspect))
        .route("/v1/recall", post(handlers::memory::recall))
        // reasoning
        .route("/v1/plan", post(handlers::reasoning::plan))
        .route("/v1/reason", post(handlers::reasoning::reason))
        // memory graph
        .route(
            "/v1/links",
            post(handlers::graph::link).delete(handlers::graph::unlink),
        )
        // typed-graph export
        .route("/v1/graph", get(handlers::graph::fetch))
        // typed-graph entities
        .route(
            "/v1/entities",
            post(handlers::entity::create).get(handlers::entity::list),
        )
        .route("/v1/entities/resolve", post(handlers::entity::resolve))
        .route(
            "/v1/entities/{id}",
            get(handlers::entity::get)
                // PUT, not PATCH: ENTITY_UPDATE replaces wholesale.
                .put(handlers::entity::update)
                .delete(handlers::entity::tombstone),
        )
        .route("/v1/entities/{id}/rename", post(handlers::entity::rename))
        .route("/v1/entities/{id}/merge", post(handlers::entity::merge))
        .route("/v1/entities/{id}/unmerge", post(handlers::entity::unmerge))
        .route(
            "/v1/entities/{id}/traverse",
            post(handlers::entity::traverse),
        )
        .route("/v1/entities/{id}/relations", get(handlers::relation::list))
        // typed-graph statements
        .route(
            "/v1/statements",
            get(handlers::statement::list).post(handlers::statement::create),
        )
        .route(
            "/v1/statements/{id}",
            get(handlers::statement::get).delete(handlers::statement::tombstone),
        )
        .route(
            "/v1/statements/{id}/supersede",
            post(handlers::statement::supersede),
        )
        // A separate verb from DELETE: the two differ in whether the content
        // survives, which is not a difference to hide in a query parameter.
        .route(
            "/v1/statements/{id}/retract",
            post(handlers::statement::retract),
        )
        .route(
            "/v1/statements/{id}/history",
            get(handlers::statement::history),
        )
        // typed-graph relations
        .route("/v1/relations", post(handlers::relation::create))
        .route(
            "/v1/relations/{id}",
            get(handlers::relation::get).delete(handlers::relation::tombstone),
        )
        .route(
            "/v1/relations/{id}/supersede",
            post(handlers::relation::supersede),
        )
        // schema. GET reads, POST merges, PUT replaces destructively — the
        // method carries the difference, which is the clearest signal
        // available that one of these three is not like the others.
        .route(
            "/v1/schema",
            get(handlers::schema::get)
                .post(handlers::schema::upload)
                .put(handlers::schema::replace),
        )
        .route("/v1/schema/validate", post(handlers::schema::validate))
        // Schema version history — distinct from `/v1/schema`, which is one
        // document.
        .route("/v1/schemas", get(handlers::schema::list))
        // Read-only: extraction is always-on and cannot be toggled.
        .route("/v1/extractors", get(handlers::schema::extractors))
        // A memory whose embedding the caller computed.
        .route("/v1/memories/vector", post(handlers::memory::encode_vector))
        // Directives rendered into a system-prompt block.
        .route(
            "/v1/procedural/materialize",
            post(handlers::memory::materialize_procedural),
        )
        // space lifecycle — SPACE_CREATE / SPACE_LIST / SPACE_DELETE
        // (0x0070-0x0072). No path parameter: all three act on the caller's
        // effective space, which Brain resolves from `act_as`.
        .route(
            "/v1/spaces",
            post(handlers::space::create)
                .get(handlers::space::list)
                .delete(handlers::space::delete),
        )
        // session lifecycle — SESSION_CREATE / SESSION_LIST / SESSION_DELETE
        // (0x0073-0x0075). A session has an id, so create and delete name one.
        .route(
            "/v1/sessions",
            post(handlers::session::create).get(handlers::session::list),
        )
        .route("/v1/sessions/{id}", delete(handlers::session::delete))
        // The change feed — SUBSCRIBE (0x0030) as Server-Sent Events.
        // Unsubscribe is the client closing the stream; see the handler.
        .route("/v1/subscribe", get(handlers::subscribe::subscribe))
}

#[cfg(test)]
mod plane_tests {
    use super::*;

    #[test]
    fn the_default_serves_everything() {
        // Self-host must work without setting anything.
        assert_eq!(Plane::default(), Plane::Both);
        assert!(Plane::Both.serves_data());
        assert!(Plane::Both.serves_credentials());
        assert_eq!(Plane::from_name("").unwrap(), Plane::Both);
    }

    #[test]
    fn the_data_plane_cannot_reach_key_material() {
        // The whole point of the split: a data-plane process serves no route
        // that can read or use a provider key.
        assert!(Plane::Data.serves_data());
        assert!(!Plane::Data.serves_credentials());
    }

    #[test]
    fn the_credential_plane_serves_no_memory_verbs() {
        assert!(Plane::Credential.serves_credentials());
        assert!(!Plane::Credential.serves_data());
    }

    #[test]
    fn parsing_is_case_and_whitespace_tolerant() {
        assert_eq!(Plane::from_name(" DATA ").unwrap(), Plane::Data);
        assert_eq!(Plane::from_name("Credential").unwrap(), Plane::Credential);
    }

    #[test]
    fn an_unknown_plane_is_an_error_not_a_silent_both() {
        // Falling back to `Both` would quietly serve credentials from a
        // process the operator meant to be data-only — the exact mistake this
        // flag exists to prevent.
        let err = Plane::from_name("dataplane").unwrap_err();
        assert!(err.contains("dataplane"), "{err}");
        assert!(err.contains("data"), "names the accepted set: {err}");
    }
}
