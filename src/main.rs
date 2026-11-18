//! brain-edge — the self-host binary.
//!
//! Thin wrapper: load config from the environment and run the edge with the
//! self-host defaults (bearer passthrough, no metering). The reusable engine —
//! router, ports, DTOs, pool — lives in the crate library (`brain_edge`), which
//! the Arc cloud gateway embeds with its own auth + metering ports.

use brain_edge::EdgeConfig;
use tracing::info;

/// Env var selecting human-readable or structured startup logging.
const LOG_FORMAT_ENV: &str = "BRAIN_EDGE_LOG_FORMAT";

/// Output encoding for the process log stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LogFormat {
    /// For a self-host operator reading logs by eye.
    #[default]
    Text,
    /// For an aggregator that indexes one JSON object per line.
    Json,
}

impl LogFormat {
    /// Parse a [`LOG_FORMAT_ENV`] value.
    ///
    /// # Errors
    /// An unrecognised value is a hard startup error rather than a fallback to
    /// text: a configured `jsonl` value followed by human-readable output gives
    /// an aggregator no parseable events and removes the only immediate signal
    /// that the operator's setting was wrong.
    fn from_name(name: &str) -> Result<Self, String> {
        match name.trim().to_ascii_lowercase().as_str() {
            "" | "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            other => Err(format!(
                "unknown log format `{other}`: expected one of text, json"
            )),
        }
    }

    /// Read the log format from the environment, using text when it is absent.
    ///
    /// # Errors
    /// As [`Self::from_name`].
    fn from_env() -> Result<Self, String> {
        std::env::var(LOG_FORMAT_ENV).map_or(Ok(Self::Text), |value| Self::from_name(&value))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log_format = LogFormat::from_env().map_err(|e| format!("{LOG_FORMAT_ENV}: {e}"))?;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "brain_edge=info,tower_http=info".into());
    match log_format {
        LogFormat::Text => tracing_subscriber::fmt().with_env_filter(filter).init(),
        LogFormat::Json => {
            // Flattening puts fields at the top level, where aggregators can
            // index them directly instead of requiring a JSON path through
            // each event's nested `fields` object.
            tracing_subscriber::fmt()
                .json()
                .flatten_event(true)
                .with_current_span(true)
                .with_span_list(false)
                .with_env_filter(filter)
                .init();
        }
    }

    let config = EdgeConfig::from_env().map_err(|e| format!("config: {e}"))?;
    info!(
        listen = %config.listen_addr,
        brain = %config.brain_addr,
        pool_size = config.pool_size,
        "brain-edge starting",
    );
    brain_edge::run(config).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_format_defaults_to_text() {
        assert_eq!(LogFormat::from_name(""), Ok(LogFormat::Text));
    }

    #[test]
    fn log_format_parses_json() {
        assert_eq!(LogFormat::from_name("json"), Ok(LogFormat::Json));
    }

    #[test]
    fn log_format_is_case_and_whitespace_tolerant() {
        assert_eq!(LogFormat::from_name("  JsOn  "), Ok(LogFormat::Json));
    }

    #[test]
    fn log_format_rejects_unknown_values_with_accepted_set() {
        let error = LogFormat::from_name("jsonl").expect_err("unknown format");
        assert!(error.contains("text, json"));
    }
}
