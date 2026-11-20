//! Deterministic Prometheus text exposition for the edge metric families.

use std::fmt::Write as _;

use super::EdgeMetricsSnapshot;
use super::counter::Counter;
use super::engine::{ENGINE_CALL_KIND_LABELS, ENGINE_CALL_OUTCOME_LABELS};
use super::gauge::Gauge;
use super::histogram::HistogramSnapshot;
use super::pool::{ACQUIRE_OUTCOME_LABELS, EVICTION_REASON_LABELS};
use super::request::{ROUTE_LABELS, STATUS_CLASS_LABELS};

/// Emit a HELP/TYPE header.
pub fn emit_header(out: &mut String, name: &str, help: &str, kind: &str) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {kind}");
}

/// Emit a labelled counter using a complete brace-delimited label string.
pub fn emit_counter_labeled(out: &mut String, name: &str, labels: &str, counter: &Counter) {
    emit_counter_value(out, name, labels, counter.get());
}

/// Emit a labelless gauge.
pub fn emit_gauge(out: &mut String, name: &str, gauge: &Gauge) {
    emit_scalar(out, name, gauge.get());
}

/// Emit a labelled gauge using a complete brace-delimited label string.
pub fn emit_gauge_labeled(out: &mut String, name: &str, labels: &str, gauge: &Gauge) {
    let _ = writeln!(out, "{name}{labels} {}", gauge.get());
}

/// Emit an info family whose payload is carried in labels and whose value is one.
pub fn emit_info(out: &mut String, name: &str, labels: &str) {
    let _ = writeln!(out, "{name}{labels} 1");
}

/// Emit a labelless scalar.
pub fn emit_scalar(out: &mut String, name: &str, value: u64) {
    let _ = writeln!(out, "{name} {value}");
}

fn emit_counter_value(out: &mut String, name: &str, labels: &str, value: u64) {
    let _ = writeln!(out, "{name}{labels} {value}");
}

/// Emit cumulative histogram buckets, sum, and count.
pub fn emit_histogram(out: &mut String, name: &str, labels: &str, histogram: &HistogramSnapshot) {
    for bucket in &histogram.buckets {
        let bound = bucket
            .le
            .map_or_else(|| "+Inf".to_string(), |value| value.to_string());
        let separator = if labels.is_empty() { "" } else { "," };
        let _ = writeln!(
            out,
            "{name}_bucket{{{labels}{separator}le=\"{bound}\"}} {}",
            bucket.cumulative_count
        );
    }
    if labels.is_empty() {
        let _ = writeln!(out, "{name}_sum {}", histogram.sum);
        let _ = writeln!(out, "{name}_count {}", histogram.count);
    } else {
        let _ = writeln!(out, "{name}_sum{{{labels}}} {}", histogram.sum);
        let _ = writeln!(out, "{name}_count{{{labels}}} {}", histogram.count);
    }
}

/// Render a complete edge metrics snapshot in stable dashboard order.
#[must_use]
pub fn format(snapshot: &EdgeMetricsSnapshot) -> String {
    let mut out = String::with_capacity(16_384);
    emit_header(
        &mut out,
        "brain_edge_build_info",
        "Build identity for correlating fleet behaviour.",
        "gauge",
    );
    emit_info(
        &mut out,
        "brain_edge_build_info",
        &format!("{{version=\"{}\"}}", env!("CARGO_PKG_VERSION")),
    );
    emit_header(
        &mut out,
        "brain_edge_up",
        "Whether this process is alive and able to serve HTTP.",
        "gauge",
    );
    emit_scalar(&mut out, "brain_edge_up", 1);
    emit_header(
        &mut out,
        "brain_edge_requests_in_flight",
        "Requests currently occupying this edge process.",
        "gauge",
    );
    emit_scalar(
        &mut out,
        "brain_edge_requests_in_flight",
        snapshot.request.in_flight,
    );
    emit_request_metrics(&mut out, snapshot);
    emit_engine_metrics(&mut out, snapshot);
    emit_pool_metrics(&mut out, snapshot);
    out
}

fn emit_request_metrics(out: &mut String, snapshot: &EdgeMetricsSnapshot) {
    emit_header(
        out,
        "brain_edge_requests_total",
        "Completed requests grouped by route and outcome class.",
        "counter",
    );
    for (route_index, route) in ROUTE_LABELS.iter().enumerate() {
        for (status_index, status) in STATUS_CLASS_LABELS.iter().enumerate() {
            emit_counter_value(
                out,
                "brain_edge_requests_total",
                &format!("{{route=\"{route}\",status=\"{status}\"}}"),
                snapshot.request.totals[route_index * STATUS_CLASS_LABELS.len() + status_index],
            );
        }
    }
    emit_header(
        out,
        "brain_edge_request_duration_seconds",
        "Request latency, including middleware-generated responses.",
        "histogram",
    );
    for (index, route) in ROUTE_LABELS.iter().enumerate() {
        emit_histogram(
            out,
            "brain_edge_request_duration_seconds",
            &format!("route=\"{route}\""),
            &snapshot.request.durations[index],
        );
    }
}

fn emit_engine_metrics(out: &mut String, snapshot: &EdgeMetricsSnapshot) {
    emit_header(
        out,
        "brain_edge_engine_calls_total",
        "Brain round trips grouped by operation and failure mode.",
        "counter",
    );
    for (kind_index, kind) in ENGINE_CALL_KIND_LABELS.iter().enumerate() {
        for (outcome_index, outcome) in ENGINE_CALL_OUTCOME_LABELS.iter().enumerate() {
            emit_counter_value(
                out,
                "brain_edge_engine_calls_total",
                &format!("{{kind=\"{kind}\",outcome=\"{outcome}\"}}"),
                snapshot.engine.calls
                    [kind_index * ENGINE_CALL_OUTCOME_LABELS.len() + outcome_index],
            );
        }
    }
    emit_header(
        out,
        "brain_edge_engine_call_duration_seconds",
        "Time spent waiting for Brain handshakes, reconnects, and probes.",
        "histogram",
    );
    for (index, kind) in ENGINE_CALL_KIND_LABELS.iter().enumerate() {
        emit_histogram(
            out,
            "brain_edge_engine_call_duration_seconds",
            &format!("kind=\"{kind}\""),
            &snapshot.engine.durations[index],
        );
    }
}

fn emit_pool_metrics(out: &mut String, snapshot: &EdgeMetricsSnapshot) {
    emit_header(
        out,
        "brain_edge_pool_acquisitions_total",
        "Pool acquisition outcomes; rising failures indicate Brain or credential trouble.",
        "counter",
    );
    for (index, outcome) in ACQUIRE_OUTCOME_LABELS.iter().enumerate() {
        emit_counter_value(
            out,
            "brain_edge_pool_acquisitions_total",
            &format!("{{outcome=\"{outcome}\"}}"),
            snapshot.pool.acquisitions[index],
        );
    }
    emit_header(
        out,
        "brain_edge_pool_acquire_duration_seconds",
        "Whole pool acquisition latency, including collapsed cold-start waits.",
        "histogram",
    );
    emit_histogram(
        out,
        "brain_edge_pool_acquire_duration_seconds",
        "",
        &snapshot.pool.acquire_duration,
    );
    emit_header(
        out,
        "brain_edge_pool_init_collapsed_total",
        "Requests that joined an in-flight per-credential initializer.",
        "counter",
    );
    emit_scalar(
        out,
        "brain_edge_pool_init_collapsed_total",
        snapshot.pool.init_collapsed,
    );
    emit_header(
        out,
        "brain_edge_credential_cache_size",
        "Distinct credentials currently retained by the cache.",
        "gauge",
    );
    emit_scalar(
        out,
        "brain_edge_credential_cache_size",
        snapshot.pool.cache_size,
    );
    emit_header(
        out,
        "brain_edge_credential_cache_evictions_total",
        "Credential evictions separated into capacity pressure and healthy idle cleanup.",
        "counter",
    );
    for (index, reason) in EVICTION_REASON_LABELS.iter().enumerate() {
        emit_counter_value(
            out,
            "brain_edge_credential_cache_evictions_total",
            &format!("{{reason=\"{reason}\"}}"),
            snapshot.pool.evictions[index],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{emit_counter_labeled, emit_header, emit_histogram, format};
    use crate::metrics::{Counter, EdgeMetrics, EdgeMetricsSnapshot};

    #[test]
    fn helpers_have_stable_bytes() {
        let mut output = String::new();
        emit_header(&mut output, "brain_test_total", "a test.", "counter");
        let counter = Counter::new();
        counter.add(7);
        emit_counter_labeled(&mut output, "brain_test_total", "{kind=\"x\"}", &counter);
        assert_eq!(
            output,
            "# HELP brain_test_total a test.\n# TYPE brain_test_total counter\nbrain_test_total{kind=\"x\"} 7\n"
        );
    }

    #[test]
    fn format_contains_every_family_and_histogram_parts() {
        let snapshot: EdgeMetricsSnapshot = EdgeMetrics::new().snapshot();
        let body = format(&snapshot);
        for name in [
            "brain_edge_build_info",
            "brain_edge_up",
            "brain_edge_requests_in_flight",
            "brain_edge_requests_total",
            "brain_edge_request_duration_seconds",
            "brain_edge_engine_calls_total",
            "brain_edge_engine_call_duration_seconds",
            "brain_edge_pool_acquisitions_total",
            "brain_edge_pool_acquire_duration_seconds",
            "brain_edge_pool_init_collapsed_total",
            "brain_edge_credential_cache_size",
            "brain_edge_credential_cache_evictions_total",
        ] {
            assert!(body.contains(&format!("# TYPE {name} ")));
        }
        assert!(body.contains("_bucket"));
        assert!(body.contains("_sum"));
        assert!(body.contains("_count"));
        assert!(body.contains("le=\"+Inf\""));
        let mut histogram = String::new();
        emit_histogram(
            &mut histogram,
            "test_seconds",
            "",
            &snapshot.request.durations[0],
        );
        assert!(histogram.contains("test_seconds_bucket{le=\"+Inf\"}"));
    }
}
