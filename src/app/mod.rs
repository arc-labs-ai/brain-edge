//! The HTTP surface: the data-plane router.

pub mod handlers;

use axum::routing::post;
use axum::routing::get;
use axum::Router;

use crate::state::EdgeState;

/// Build the data-plane router (`/v1/*`), with [`EdgeState`] baked in.
///
/// This is the reusable half: the self-host binary adds its own health routes
/// around it (see [`crate::run`]); the hosted gateway merges it under its auth +
/// metering middleware alongside its control-plane routes. It deliberately does
/// NOT register `/health/*`, so a host app can own health without colliding.
#[must_use]
pub fn router(state: EdgeState) -> Router {
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
        .route(
            "/v1/memories/{id}/inspect",
            get(handlers::memory::inspect),
        )
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
        .route("/v1/entities/{id}", get(handlers::entity::get))
        .route(
            "/v1/entities/{id}/traverse",
            post(handlers::entity::traverse),
        )
        .route(
            "/v1/entities/{id}/relations",
            get(handlers::relation::list),
        )
        // typed-graph statements
        .route("/v1/statements", get(handlers::statement::list))
        .route("/v1/statements/{id}", get(handlers::statement::get))
        // typed-graph relations
        .route("/v1/relations/{id}", get(handlers::relation::get))
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
        .with_state(state)
}
