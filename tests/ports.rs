//! The two ports that turn this edge into a product: [`CredentialResolver`]
//! and [`MeteringSink`].
//!
//! The self-host default forwards the caller's bearer to Brain and meters
//! nothing, so neither port does anything visible in that deployment. The
//! hosted gateway swaps both — an API key resolves to a tenant, and every
//! operation is recorded as usage. That means these two traits carry the
//! access control and the billing signal for the managed product, and until
//! this file existed they had no tests at all.
//!
//! What that leaves unguarded, absent these tests:
//!
//!   * a route that forgets to consult the resolver is an auth bypass in the
//!     hosted deployment and completely invisible in self-host, where the
//!     default forwards the token to Brain regardless;
//!   * a route that forgets to record is silently unbilled, and one that
//!     records twice overbills a customer.
//!
//! Both are correct today. Nothing was holding them there.
//!
//! The router is driven in-process with `ServiceExt::oneshot`, so these
//! exercise the real routing and extractor stack. No Brain is required: the
//! resolver runs before any connection is opened, so a rejecting resolver
//! short-circuits every route without one.

use std::time::Duration;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use brain_edge::crypto::KeyRing;
use brain_edge::port::{
    CredentialResolver, InMemoryLlmCredentialStore, MeterEvent, MeteringSink, Outcome,
    ResolvedCredential,
};
use brain_edge::{ApiError, EdgeConfig, EdgeState};
use tower::ServiceExt;

// --- test doubles ----------------------------------------------------------

/// A resolver that counts calls and answers however the test tells it to.
struct SpyResolver {
    calls: Mutex<usize>,
    answer: Result<ResolvedCredential, ()>,
}

impl SpyResolver {
    fn rejecting() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(0),
            answer: Err(()),
        })
    }

    fn calls(&self) -> usize {
        *self.calls.lock().expect("calls lock")
    }
}

#[async_trait::async_trait]
impl CredentialResolver for SpyResolver {
    async fn resolve(&self, _headers: &HeaderMap) -> Result<ResolvedCredential, ApiError> {
        *self.calls.lock().expect("calls lock") += 1;
        self.answer
            .clone()
            .map_err(|()| ApiError::unauthorized("spy resolver rejects everything"))
    }
}

/// A sink that keeps every event, so a test can assert on count and content
/// rather than only on "something was recorded".
#[derive(Default)]
struct RecordingMeter {
    events: Mutex<Vec<(Option<String>, String, Outcome)>>,
}

impl RecordingMeter {
    fn events(&self) -> Vec<(Option<String>, String, Outcome)> {
        self.events.lock().expect("events lock").clone()
    }
}

impl MeteringSink for RecordingMeter {
    fn record(&self, event: &MeterEvent<'_>) {
        self.events.lock().expect("events lock").push((
            event.tenant.map(str::to_owned),
            event.op.to_owned(),
            event.outcome,
        ));
    }
}

// --- the route table -------------------------------------------------------

/// Every route the edge serves, with a body that parses.
///
/// The bodies matter: `Json<T>` is an extractor, so it runs *before* the
/// handler and therefore before the resolver. A route given an unparseable
/// body would be rejected without the resolver ever being consulted, and the
/// coverage assertion below would pass while proving nothing. Required fields
/// are taken from the DTOs — see `tools/http_manifest.py`.
///
/// Kept as an explicit list rather than derived from the router, because the
/// point is to state independently what the surface is.
///
/// This list covers the routes that authenticate through the resolver. The
/// broker routes authenticate against a deployment token instead and are held
/// in `BROKER_ROUTES` below; `the_route_table_covers_the_whole_surface` counts
/// the router's registrations so neither table can fall behind it unnoticed.
const ROUTES: &[(&str, &str, Option<&str>)] = &[
    ("GET", "/v1/whoami", None),
    ("GET", "/v1/capabilities", None),
    ("POST", "/v1/memories", Some(r#"{"text":"x"}"#)),
    ("GET", "/v1/memories", None),
    ("DELETE", "/v1/memories", Some(r#"{"memory_id":"1"}"#)),
    ("GET", "/v1/memories/1/inspect", None),
    ("POST", "/v1/recall", Some(r#"{"query":"x"}"#)),
    (
        "POST",
        "/v1/links",
        Some(r#"{"source":"1","target":"2","kind":"caused"}"#),
    ),
    (
        "DELETE",
        "/v1/links",
        Some(r#"{"source":"1","target":"2","kind":"caused"}"#),
    ),
    (
        "POST",
        "/v1/plan",
        Some(r#"{"start":{"text":"a"},"goal":{"text":"b"}}"#),
    ),
    (
        "POST",
        "/v1/reason",
        Some(r#"{"observation":{"text":"a"}}"#),
    ),
    ("GET", "/v1/graph", None),
    (
        "POST",
        "/v1/entities",
        Some(r#"{"entity_type_id":1,"canonical_name":"Ada"}"#),
    ),
    ("GET", "/v1/entities", None),
    (
        "POST",
        "/v1/entities/resolve",
        Some(r#"{"candidate_name":"Ada"}"#),
    ),
    (
        "GET",
        "/v1/entities/019faf38-50c7-7820-9048-980d79a1aa27",
        None,
    ),
    (
        "POST",
        "/v1/entities/019faf38-50c7-7820-9048-980d79a1aa27/traverse",
        Some("{}"),
    ),
    (
        "GET",
        "/v1/entities/019faf38-50c7-7820-9048-980d79a1aa27/relations",
        None,
    ),
    ("GET", "/v1/statements", None),
    (
        "GET",
        "/v1/statements/019faf38-50f1-7202-a06f-fc1813453d61",
        None,
    ),
    (
        "GET",
        "/v1/relations/019faf38-50f1-7202-a06f-fc0f4a810aad",
        None,
    ),
    ("GET", "/v1/schema", None),
    (
        "POST",
        "/v1/schema",
        Some(r#"{"schema_document":"entity P {}"}"#),
    ),
    (
        "PUT",
        "/v1/schema",
        Some(r#"{"schema_document":"entity P {}","force_drop_existing":true}"#),
    ),
    (
        "POST",
        "/v1/schema/validate",
        Some(r#"{"schema_document":"entity P {}"}"#),
    ),
    // Space lifecycle. No path id: all three act on the caller's effective
    // space, which Brain resolves from `act_as`.
    ("POST", "/v1/spaces", Some("{}")),
    ("GET", "/v1/spaces", None),
    ("DELETE", "/v1/spaces", None),
    // Session lifecycle. A session has an id, so create and delete name one.
    ("POST", "/v1/sessions", Some(r#"{"session_id":7}"#)),
    ("GET", "/v1/sessions", None),
    ("DELETE", "/v1/sessions/7", None),
    // Surgical schema drop.
    ("DELETE", "/v1/schema/predicate/works_at", None),
    // Query-plan introspection.
    (
        "POST",
        "/v1/query/explain",
        Some(r#"{"text":"who works at Arc"}"#),
    ),
    (
        "POST",
        "/v1/query/trace",
        Some(r#"{"text":"who works at Arc"}"#),
    ),
    // Introspection and the memory extras.
    ("GET", "/v1/schemas", None),
    ("GET", "/v1/extractors", None),
    (
        "POST",
        "/v1/memories/vector",
        Some(
            r#"{"text":"x","vector":[0.1,0.2],"model_fingerprint":"000102030405060708090a0b0c0d0e0f"}"#,
        ),
    ),
    ("POST", "/v1/procedural/materialize", Some("{}")),
    // Relation authoring.
    (
        "POST",
        "/v1/relations",
        Some(
            r#"{"relation_type":"works_at","from_entity":"019faf38-50c7-7820-9048-980d79a1aa27","to_entity":"019faf38-50c7-7820-9048-980d79a1aa28"}"#,
        ),
    ),
    (
        "DELETE",
        "/v1/relations/019faf38-50f1-7202-a06f-fc0f4a810aad",
        None,
    ),
    (
        "POST",
        "/v1/relations/019faf38-50f1-7202-a06f-fc0f4a810aad/supersede",
        Some(
            r#"{"relation_type":"works_at","from_entity":"019faf38-50c7-7820-9048-980d79a1aa27","to_entity":"019faf38-50c7-7820-9048-980d79a1aa29"}"#,
        ),
    ),
    // Statement authoring.
    (
        "POST",
        "/v1/statements",
        Some(
            r#"{"kind":"fact","subject":"019faf38-50c7-7820-9048-980d79a1aa27","predicate":"works_at","object":{"kind":"value","value":{"type":"text","value":"Arc"}}}"#,
        ),
    ),
    (
        "DELETE",
        "/v1/statements/019faf38-50f1-7202-a06f-fc1813453d61",
        None,
    ),
    (
        "POST",
        "/v1/statements/019faf38-50f1-7202-a06f-fc1813453d61/supersede",
        Some(
            r#"{"kind":"fact","subject":"019faf38-50c7-7820-9048-980d79a1aa27","predicate":"works_at","object":{"kind":"value","value":{"type":"text","value":"Arc Labs"}}}"#,
        ),
    ),
    (
        "POST",
        "/v1/statements/019faf38-50f1-7202-a06f-fc1813453d61/retract",
        None,
    ),
    (
        "GET",
        "/v1/statements/019faf38-50f1-7202-a06f-fc1813453d61/history",
        None,
    ),
    // Entity authoring.
    (
        "PUT",
        "/v1/entities/019faf38-50c7-7820-9048-980d79a1aa27",
        Some(r#"{"canonical_name":"Ada Lovelace"}"#),
    ),
    (
        "DELETE",
        "/v1/entities/019faf38-50c7-7820-9048-980d79a1aa27",
        None,
    ),
    (
        "POST",
        "/v1/entities/019faf38-50c7-7820-9048-980d79a1aa27/rename",
        Some(r#"{"new_canonical_name":"Ada King"}"#),
    ),
    (
        "POST",
        "/v1/entities/019faf38-50c7-7820-9048-980d79a1aa27/merge",
        Some(r#"{"merged":"019faf38-50c7-7820-9048-980d79a1aa28"}"#),
    ),
    (
        "POST",
        "/v1/entities/019faf38-50c7-7820-9048-980d79a1aa27/unmerge",
        None,
    ),
    // The change feed. SSE, so the response body never completes on its own;
    // the resolver guarantee this table asserts is about the request, which
    // is an ordinary one.
    ("GET", "/v1/subscribe", None),
    // BYOK. These reach the resolver only once a keyring is installed: without
    // one, `store_of` answers 501 before authenticating, so the states built
    // below install one. That is the hosted shape, and the only shape where
    // there is a credential to protect.
    (
        "PUT",
        "/v1/llm-credential",
        Some(r#"{"provider":"openai","model":"gpt-4o-mini","api_key":"sk-not-a-real-key"}"#),
    ),
    ("GET", "/v1/llm-credential", None),
    ("DELETE", "/v1/llm-credential", None),
    (
        "POST",
        "/v1/llm-credential/test",
        Some(r#"{"provider":"openai","model":"gpt-4o-mini","api_key":"sk-not-a-real-key"}"#),
    ),
    ("POST", "/v1/llm-credential/verify", None),
];

/// The broker routes, which do **not** authenticate through the resolver.
///
/// Brain calls these with a deployment-wide token (see `llm_proxy`), not with a
/// tenant credential, so the resolver guarantee above does not apply to them
/// and asserting it would be wrong. They are listed so the surface count stays
/// honest, and `the_broker_routes_never_reach_the_resolver` pins what does hold
/// for them: an unauthenticated call is refused, consults no resolver, and
/// bills nothing.
const BROKER_ROUTES: &[(&str, &str, Option<&str>)] = &[
    (
        "POST",
        "/llm/ns/acme/openai/v1/chat/completions",
        Some(r#"{"model":"gpt-4o-mini","messages":[]}"#),
    ),
    (
        "POST",
        "/llm/ns/acme/anthropic/v1/messages",
        Some(r#"{"model":"claude-haiku-4-5","messages":[]}"#),
    ),
    ("GET", "/llm/ns/acme/resolve", None),
];

fn config() -> EdgeConfig {
    // Never connected to: the resolver rejects before any pool call.
    EdgeConfig::from_env().unwrap_or_else(|_| unreachable!("defaults are always valid"))
}

/// A 32-byte KEK, hex, under the id `test`. Not a secret: it seals nothing that
/// outlives the test process, and the store it seals into is in-memory.
const TEST_KEK_SPEC: &str = "test:1111111111111111111111111111111111111111111111111111111111111111";

/// State with BYOK installed.
///
/// The credential routes consult `store_of` before the resolver, so on an edge
/// without a keyring they answer 501 without authenticating. Installing one
/// here is what lets those routes participate in the resolver guarantee rather
/// than being exempted from it.
fn byok_state(resolver: Arc<dyn CredentialResolver>, meter: Arc<dyn MeteringSink>) -> EdgeState {
    let keyring = KeyRing::from_spec(TEST_KEK_SPEC, "test").expect("test keyring is well-formed");
    EdgeState::with_ports(config(), resolver, meter).with_llm_credentials(
        Arc::new(InMemoryLlmCredentialStore::default()),
        Arc::new(keyring),
    )
}

async fn send(
    state: EdgeState,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> axum::http::Response<Body> {
    let mut req = Request::builder().method(method).uri(path);
    if body.is_some() {
        req = req.header("content-type", "application/json");
    }
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_owned())))
        .expect("build request");

    brain_edge::app::router(state)
        .oneshot(req)
        .await
        .expect("router response")
}

// --- the guarantees --------------------------------------------------------

#[tokio::test]
async fn every_route_consults_the_resolver() {
    // The auth-bypass gate. A handler that skips `state.resolve()` would serve
    // its route to anyone in the hosted deployment, while behaving identically
    // in self-host — so this cannot be caught by using the thing.
    let resolver = SpyResolver::rejecting();
    let meter = Arc::new(RecordingMeter::default());

    for (i, (method, path, body)) in ROUTES.iter().enumerate() {
        let state = byok_state(resolver.clone(), meter.clone());
        let resp = send(state, method, path, *body).await;

        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path}: a rejecting resolver must make this 401. A different \
             status means the request reached past authentication."
        );
        assert_eq!(
            resolver.calls(),
            i + 1,
            "{method} {path}: the resolver was not consulted exactly once"
        );
    }
}

#[tokio::test]
async fn an_unauthenticated_request_is_never_metered() {
    // Usage is billable, so a rejected request must not appear as usage. This
    // also pins the ordering: `record` sits after `resolve` in every handler,
    // and moving it earlier would start billing for 401s.
    let resolver = SpyResolver::rejecting();
    let meter = Arc::new(RecordingMeter::default());

    for (method, path, body) in ROUTES.iter().chain(BROKER_ROUTES) {
        let state = byok_state(resolver.clone(), meter.clone());
        let _ = send(state, method, path, *body).await;
    }

    assert!(
        meter.events().is_empty(),
        "rejected requests were metered as usage: {:?}",
        meter.events()
    );
}

#[tokio::test]
async fn the_route_table_covers_the_whole_surface() {
    // This assertion used to compare `ROUTES.len()` against a constant written
    // directly above it. Both were hand-maintained, so adding a route to the
    // router changed neither and the test stayed green: the BYOK work added six
    // method+path combos and this never noticed, which left the resolver and
    // metering guarantees with a six-route hole. The comment claimed "adding a
    // route forces a deliberate update" — it did not.
    //
    // So count the router's own registrations. `include_str!` resolves at
    // compile time relative to this file, and the count is a plain substring
    // tally rather than a parse, which is enough: every registration in
    // `app::router` is a literal `.route(` call.
    //
    // What this catches: a new path reaching the router without reaching either
    // table. What it does not: a new METHOD chained onto a path already
    // registered (`.post(...)` added beside an existing `.get(...)`), since the
    // `.route(` count is unchanged. The combo assertion below is still
    // hand-stated for that reason — it is a second pair of eyes, not a tripwire.
    // Both counts up front: clippy's `items_after_statements` (denied here) will
    // not have a `const` introduced partway down a function body.
    const REGISTERED_PATHS: usize = 44;
    const SERVED_COMBOS: usize = 60;

    let router_src = include_str!("../src/app/mod.rs");
    let registrations = router_src.matches(".route(").count();
    assert_eq!(
        registrations, REGISTERED_PATHS,
        "app::router registers {registrations} paths but this test expects \
         {REGISTERED_PATHS}. A path was added or removed: list it in ROUTES (if it \
         authenticates through the resolver) or in BROKER_ROUTES (if it \
         authenticates against the deployment token), then update this count."
    );

    assert_eq!(
        ROUTES.len() + BROKER_ROUTES.len(),
        SERVED_COMBOS,
        "the router serves {SERVED_COMBOS} method+path combos; update the tables so \
         the resolver and metering guarantees still cover all of them"
    );
}

#[tokio::test]
async fn a_keyless_edge_refuses_byok_before_authenticating() {
    // Pinning a real property of the design, not endorsing it — the same
    // treatment `validation_runs_before_authentication` gets.
    //
    // Every credential handler asks for the store before resolving the
    // caller, so an edge with no keyring answers 501 to anyone. The consequence
    // is that an unauthenticated caller can tell a BYOK-capable edge from one
    // without a KEK.
    //
    // `test` is held to the same line even though it stores nothing: an edge
    // that cannot hold a key has no business probing providers on a caller's
    // behalf, and answering there would imply a registration that will 501.
    //
    // Not an auth bypass, and not reachable where it would matter: with no
    // keyring there is no credential to disclose or modify, and on an edge that
    // has one these routes resolve the caller first — which is what
    // `every_route_consults_the_resolver` proves, since the states it builds
    // install a keyring. Recorded here so reordering those two lines is a
    // visible change rather than a silent one.
    /// How many trailing `ROUTES` entries are the credential verbs. They are
    /// kept last so this slice can reach them; adding one means bumping this.
    const CREDENTIAL_VERBS: usize = 5;

    let resolver = SpyResolver::rejecting();
    let meter = Arc::new(RecordingMeter::default());

    for (method, path, body) in &ROUTES[ROUTES.len() - CREDENTIAL_VERBS..] {
        assert!(
            path.starts_with("/v1/llm-credential"),
            "this test slices the last {CREDENTIAL_VERBS} ROUTES entries, which \
             must be the credential verbs; the table was reordered (found {path})"
        );
        let state = EdgeState::with_ports(config(), resolver.clone(), meter.clone());
        let resp = send(state, method, path, *body).await;
        assert_eq!(
            resp.status(),
            StatusCode::NOT_IMPLEMENTED,
            "{method} {path}: a keyless edge should answer 501, not {}",
            resp.status()
        );
    }

    assert_eq!(
        resolver.calls(),
        0,
        "the credential routes authenticated on a keyless edge; that is an \
         improvement, so move these routes' expectation rather than deleting it"
    );
    assert!(meter.events().is_empty(), "a 501 was metered as usage");
}

#[tokio::test]
async fn the_broker_routes_never_reach_the_resolver() {
    // The companion to `every_route_consults_the_resolver`, for the routes that
    // deliberately do not. Brain reaches the broker with a deployment token, so
    // these must be refused without a tenant credential ever being resolved —
    // a broker route that fell through to the resolver would make Brain's
    // service identity look like a tenant's.
    //
    // The exact status is not pinned here: it depends on whether a broker token
    // is configured in the ambient environment (501 when absent, 401 when
    // present and unmatched). `llm_proxy`'s own tests pin both against
    // `authenticate_with`, which takes the token as an argument instead of
    // reading the environment. What matters here is that the request is refused
    // and that neither port is touched.
    let resolver = SpyResolver::rejecting();
    let meter = Arc::new(RecordingMeter::default());

    for (method, path, body) in BROKER_ROUTES {
        let state = byok_state(resolver.clone(), meter.clone());
        let resp = send(state, method, path, *body).await;

        assert!(
            resp.status().is_client_error() || resp.status().is_server_error(),
            "{method} {path}: an unauthenticated broker call must be refused, got {}",
            resp.status()
        );
    }

    assert_eq!(
        resolver.calls(),
        0,
        "a broker route consulted the tenant resolver; Brain authenticates with \
         the deployment token, so resolving a tenant here would conflate the two"
    );
    assert!(
        meter.events().is_empty(),
        "a refused broker call was metered as tenant usage: {:?}",
        meter.events()
    );
}

#[tokio::test]
async fn validation_runs_before_authentication() {
    // Pinning a real property of the design, not endorsing it.
    //
    // `Json<T>` is an extractor, so an unparseable body is rejected before the
    // handler runs — and several handlers validate their input before calling
    // `resolve`. The consequence is that an anonymous caller can tell a
    // well-formed request from a malformed one: valid body -> 401, invalid
    // body -> 400. That is enough to map field names and validation rules
    // without a credential, and those probes never appear in auth logs.
    //
    // Not an auth bypass: no data is returned and no operation runs. Fixing it
    // would mean authenticating in a middleware layer ahead of the extractors,
    // which the library router deliberately does not have — `crate::run` is
    // where the self-host binary layers its own. Recorded here so the choice is
    // visible and cannot change silently.
    let resolver = SpyResolver::rejecting();
    let meter = Arc::new(RecordingMeter::default());

    let state = EdgeState::with_ports(config(), resolver.clone(), meter.clone());
    let valid = send(state, "POST", "/v1/memories", Some(r#"{"text":"x"}"#)).await;
    assert_eq!(valid.status(), StatusCode::UNAUTHORIZED);

    let before = resolver.calls();
    let state = EdgeState::with_ports(config(), resolver.clone(), meter.clone());
    let invalid = send(state, "POST", "/v1/memories", Some(r#"{"text":""}"#)).await;
    assert_eq!(
        invalid.status(),
        StatusCode::BAD_REQUEST,
        "a blank text is rejected by the handler's own validation"
    );
    assert_eq!(
        resolver.calls(),
        before,
        "the resolver is not consulted when validation rejects first"
    );

    let state = EdgeState::with_ports(config(), resolver.clone(), meter.clone());
    let unparseable = send(state, "POST", "/v1/memories", Some(r#"{"nope":1}"#)).await;
    assert!(
        unparseable.status().is_client_error(),
        "an unparseable body is rejected by the Json extractor"
    );

    // The same holds for path parameters, which is the broader case: every
    // `{id}` route parses its id before authenticating, so a malformed id is a
    // 400 to an anonymous caller while a well-formed one is a 401.
    let state = EdgeState::with_ports(config(), resolver.clone(), meter.clone());
    let bad_id = send(state, "GET", "/v1/entities/not-a-uuid", None).await;
    assert_eq!(
        bad_id.status(),
        StatusCode::BAD_REQUEST,
        "a malformed path id is rejected before authentication"
    );

    assert!(
        meter.events().is_empty(),
        "requests rejected before authentication must not be metered"
    );
}

#[tokio::test]
async fn a_resolved_tenant_reaches_the_sink() {
    // The gateway fills `tenant` from the API key, and every metered event has
    // to carry it — an event without one cannot be attributed to a customer,
    // which makes it useless for billing. Asserted directly on the state rather
    // than through a route, so it holds without a reachable Brain.
    let meter = Arc::new(RecordingMeter::default());
    let state = EdgeState::with_ports(
        config(),
        Arc::new(SpyResolver {
            calls: Mutex::new(0),
            answer: Ok(ResolvedCredential {
                credential: "brain_key".into(),
                tenant: Some("acme/prod".into()),
                act_as: None,
            }),
        }),
        meter.clone(),
    );

    let ident = state
        .resolve(&HeaderMap::new())
        .await
        .expect("resolver accepts");
    state.record("encode", &ident, Outcome::Ok, Duration::from_millis(7));
    state.record("recall", &ident, Outcome::Err, Duration::from_millis(3));

    assert_eq!(
        meter.events(),
        vec![
            (Some("acme/prod".into()), "encode".into(), Outcome::Ok),
            (Some("acme/prod".into()), "recall".into(), Outcome::Err),
        ],
        "the tenant label, verb name and outcome must all reach the sink"
    );
}

#[tokio::test]
async fn the_self_host_default_meters_nothing_and_forwards_the_bearer() {
    // The OSS deployment must stay zero-overhead and must not need a key store:
    // `NoopMeter` records nothing, and `BearerResolver` passes the caller's
    // token through with no tenant, because Brain resolves identity there.
    let state = EdgeState::new(config());

    let mut headers = HeaderMap::new();
    headers.insert("authorization", "Bearer brain_abc".parse().expect("header"));
    let ident = state.resolve(&headers).await.expect("bearer accepted");

    assert_eq!(ident.credential, "brain_abc");
    assert!(
        ident.tenant.is_none(),
        "the self-host default must not invent a tenant; Brain owns identity there"
    );

    // No panic, no state: the default sink is a no-op by construction.
    state.record("encode", &ident, Outcome::Ok, Duration::from_millis(1));
}
