//! HTTP request totals, latency, and process-wide in-flight pressure.

use std::time::Instant;

use axum::extract::MatchedPath;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use super::counter::Counter;
use super::gauge::Gauge;
use super::histogram::{EdgeHistogram, HistogramSnapshot, REQUEST_DURATION_BUCKETS_SECONDS};

/// Stable status classes keep alert cardinality bounded while preserving useful failure classes.
#[derive(Clone, Copy, Debug)]
pub enum StatusClass {
    /// A successful response.
    Success,
    /// A redirection response.
    Redirection,
    /// A client-error response.
    ClientError,
    /// A server-error response.
    ServerError,
}

/// Status labels in append-only discriminant order.
pub const STATUS_CLASS_LABELS: &[&str] = &["2xx", "3xx", "4xx", "5xx"];

impl StatusClass {
    /// Classify an HTTP status code.
    #[must_use]
    pub const fn from_status(status: StatusCode) -> Self {
        match status.as_u16() {
            200..=299 => Self::Success,
            300..=399 => Self::Redirection,
            400..=499 => Self::ClientError,
            _ => Self::ServerError,
        }
    }

    /// Return the append-only label-axis index.
    #[must_use]
    pub const fn idx(self) -> usize {
        match self {
            Self::Success => 0,
            Self::Redirection => 1,
            Self::ClientError => 2,
            Self::ServerError => 3,
        }
    }
}

/// Route patterns registered by the data plane and the process-owned operational routes.
/// The axis is bounded by this table rather than user input, which makes per-route labels
/// affordable; `other` keeps a newly added route countable instead of dropping or panicking.
pub const ROUTE_LABELS: &[&str] = &[
    "/v1/llm-credential",
    "/v1/llm-credential/test",
    "/v1/llm-credential/verify",
    "/v1/whoami",
    "/v1/capabilities",
    "/v1/memories",
    "/v1/memories/{id}/inspect",
    "/v1/recall",
    "/v1/plan",
    "/v1/reason",
    "/v1/links",
    "/v1/graph",
    "/v1/entities",
    "/v1/entity-types",
    "/v1/entities/resolve",
    "/v1/entities/{id}",
    "/v1/entities/{id}/rename",
    "/v1/entities/{id}/merge",
    "/v1/entities/{id}/unmerge",
    "/v1/entities/{id}/traverse",
    "/v1/entities/{id}/relations",
    "/v1/statements",
    "/v1/statements/{id}",
    "/v1/statements/{id}/supersede",
    "/v1/statements/{id}/retract",
    "/v1/statements/{id}/history",
    "/v1/relations",
    "/v1/relations/{id}",
    "/v1/relations/{id}/supersede",
    "/v1/schema",
    "/v1/schema/validate",
    "/v1/schema/{kind}/{name}",
    "/v1/query/explain",
    "/v1/query/trace",
    "/v1/schemas",
    "/v1/extractors",
    "/v1/memories/vector",
    "/v1/procedural/materialize",
    "/v1/spaces",
    "/v1/sessions",
    "/v1/sessions/{id}",
    "/v1/subscribe",
    "/health/live",
    "/health/ready",
    "/metrics",
    "other",
];

/// Resolve a matched route to its bounded label index; user-controlled concrete URIs never enter it.
#[must_use]
pub fn route_index(path: &str) -> usize {
    match path {
        "/v1/llm-credential" => 0,
        "/v1/llm-credential/test" => 1,
        "/v1/llm-credential/verify" => 2,
        "/v1/whoami" => 3,
        "/v1/capabilities" => 4,
        "/v1/memories" => 5,
        "/v1/memories/{id}/inspect" => 6,
        "/v1/recall" => 7,
        "/v1/plan" => 8,
        "/v1/reason" => 9,
        "/v1/links" => 10,
        "/v1/graph" => 11,
        "/v1/entities" => 12,
        "/v1/entity-types" => 13,
        "/v1/entities/resolve" => 14,
        "/v1/entities/{id}" => 15,
        "/v1/entities/{id}/rename" => 16,
        "/v1/entities/{id}/merge" => 17,
        "/v1/entities/{id}/unmerge" => 18,
        "/v1/entities/{id}/traverse" => 19,
        "/v1/entities/{id}/relations" => 20,
        "/v1/statements" => 21,
        "/v1/statements/{id}" => 22,
        "/v1/statements/{id}/supersede" => 23,
        "/v1/statements/{id}/retract" => 24,
        "/v1/statements/{id}/history" => 25,
        "/v1/relations" => 26,
        "/v1/relations/{id}" => 27,
        "/v1/relations/{id}/supersede" => 28,
        "/v1/schema" => 29,
        "/v1/schema/validate" => 30,
        "/v1/schema/{kind}/{name}" => 31,
        "/v1/query/explain" => 32,
        "/v1/query/trace" => 33,
        "/v1/schemas" => 34,
        "/v1/extractors" => 35,
        "/v1/memories/vector" => 36,
        "/v1/procedural/materialize" => 37,
        "/v1/spaces" => 38,
        "/v1/sessions" => 39,
        "/v1/sessions/{id}" => 40,
        "/v1/subscribe" => 41,
        "/health/live" => 42,
        "/health/ready" => 43,
        "/metrics" => 44,
        _ => 45,
    }
}

/// HTTP request metric family.
#[derive(Debug)]
pub struct RequestMetrics {
    totals: Vec<Counter>,
    durations: Vec<EdgeHistogram>,
    in_flight: Gauge,
}

impl RequestMetrics {
    /// Construct dense route-by-status storage.
    #[must_use]
    pub fn new() -> Self {
        Self {
            totals: (0..ROUTE_LABELS.len() * STATUS_CLASS_LABELS.len())
                .map(|_| Counter::new())
                .collect(),
            durations: (0..ROUTE_LABELS.len())
                .map(|_| EdgeHistogram::new(REQUEST_DURATION_BUCKETS_SECONDS))
                .collect(),
            in_flight: Gauge::new(),
        }
    }

    /// Access one route/status counter.
    #[must_use]
    pub fn total(&self, route: usize, status: usize) -> &Counter {
        &self.totals[route * STATUS_CLASS_LABELS.len() + status]
    }

    /// Access one route latency histogram.
    #[must_use]
    pub fn duration(&self, route: usize) -> &EdgeHistogram {
        &self.durations[route]
    }

    /// Access process-wide in-flight occupancy.
    #[must_use]
    pub fn in_flight(&self) -> &Gauge {
        &self.in_flight
    }

    /// Copy all request metrics into plain data.
    #[must_use]
    pub fn snapshot(&self) -> RequestMetricsSnapshot {
        RequestMetricsSnapshot {
            totals: self.totals.iter().map(Counter::get).collect(),
            durations: self.durations.iter().map(EdgeHistogram::snapshot).collect(),
            in_flight: self.in_flight.get(),
        }
    }
}

impl Default for RequestMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Plain-data request snapshot.
#[derive(Clone, Debug)]
pub struct RequestMetricsSnapshot {
    /// Dense route-by-status totals.
    pub totals: Vec<u64>,
    /// One duration histogram per route.
    pub durations: Vec<HistogramSnapshot>,
    /// Current process-wide in-flight count.
    pub in_flight: u64,
}

/// Track every response, including responses generated by middleware before a handler runs.
pub async fn track(req: Request<axum::body::Body>, next: Next) -> Response {
    // MatchedPath is a router pattern, not a concrete URI: labeling by user input creates
    // unbounded series and is the single worst way to overload a Prometheus server.
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or(ROUTE_LABELS.len() - 1, |path| route_index(path.as_str()));
    let metrics = &super::global().request;
    metrics.in_flight().inc();
    let started = Instant::now();
    let response = next.run(req).await;
    metrics
        .duration(route)
        .observe(started.elapsed().as_secs_f64());
    metrics
        .total(route, StatusClass::from_status(response.status()).idx())
        .inc();
    metrics.in_flight().dec();
    response
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::{ROUTE_LABELS, RequestMetrics, STATUS_CLASS_LABELS, StatusClass, route_index};

    #[test]
    fn status_classes_and_routes_are_bounded() {
        assert_eq!(StatusClass::from_status(StatusCode::OK).idx(), 0);
        assert_eq!(
            StatusClass::from_status(StatusCode::MOVED_PERMANENTLY).idx(),
            1
        );
        assert_eq!(StatusClass::from_status(StatusCode::NOT_FOUND).idx(), 2);
        assert_eq!(
            StatusClass::from_status(StatusCode::INTERNAL_SERVER_ERROR).idx(),
            3
        );
        assert_ne!(route_index("/v1/whoami"), ROUTE_LABELS.len() - 1);
        assert_ne!(route_index("/v1/entities/{id}"), route_index("/v1/whoami"));
        assert_eq!(route_index("/not-registered"), ROUTE_LABELS.len() - 1);
        assert_eq!(ROUTE_LABELS.last(), Some(&"other"));
        for (index, label) in ROUTE_LABELS.iter().enumerate() {
            assert!(!ROUTE_LABELS[..index].contains(label));
        }
    }

    #[test]
    fn recording_selects_one_route_status_cell() {
        let metrics = RequestMetrics::new();
        let route = route_index("/v1/whoami");
        metrics.total(route, StatusClass::Success.idx()).inc();
        metrics.duration(route).observe(0.01);
        assert_eq!(metrics.total(route, StatusClass::Success.idx()).get(), 1);
        assert_eq!(
            metrics.total(route, StatusClass::ClientError.idx()).get(),
            0
        );
        assert_eq!(
            metrics.total(route + 1, StatusClass::Success.idx()).get(),
            0
        );
        assert_eq!(metrics.duration(route).snapshot().count, 1);
        assert_eq!(metrics.in_flight().get(), 0);
        assert_eq!(STATUS_CLASS_LABELS.len(), 4);
    }
}
