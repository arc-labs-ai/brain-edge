//! Deploy-time configuration, read from the environment.

use std::net::{SocketAddr, ToSocketAddrs};

/// brain-edge configuration.
#[derive(Clone, Debug)]
pub struct EdgeConfig {
    /// Address to serve the HTTP API on.
    pub listen_addr: SocketAddr,
    /// Address of the Brain database to proxy to.
    pub brain_addr: SocketAddr,
    /// Per-credential connection-pool width to Brain.
    pub pool_size: usize,
    /// Hard cap on distinct cached credentials (LRU-evicted over this).
    pub max_credentials: usize,
    /// Seconds of idle before a cached credential's pool is swept.
    pub idle_ttl_secs: u64,
    /// Per-request wall-clock timeout, in seconds. A request that outlives this
    /// (e.g. a stalled Brain connection) is cut with `408 Request Timeout`
    /// rather than hanging a worker indefinitely.
    pub request_timeout_secs: u64,
    /// Hard cap on request body size, in bytes. Bodies over this are rejected
    /// with `413 Payload Too Large` before the handler runs.
    pub max_body_bytes: usize,
    /// Optional address for the transparent wire-protocol proxy listener. When
    /// `Some`, the edge serves the Brain binary wire protocol here (in addition
    /// to the HTTP data plane) — a customer SDK points at this address with its
    /// own Brain key and the edge splices frames to Brain untouched. `None`
    /// (the default) leaves the proxy off; the edge is HTTP-only.
    pub wire_listen_addr: Option<SocketAddr>,
    /// Wire-proxy per-credential rate-limit burst ceiling. `0` (the default)
    /// disables rate limiting on the wire path.
    pub wire_rate_capacity: u32,
    /// Wire-proxy per-credential sustained rate, in ops per second, refilled into
    /// the burst bucket. Ignored when `wire_rate_capacity` is `0`.
    pub wire_rate_refill_per_sec: u32,
}

/// Parse an env var as `T`, or fail loudly. Unlike a silent `unwrap_or(default)`,
/// a typo'd value (`BRAIN_EDGE_POOL_SIZE=four`) is a hard startup error rather
/// than a silent fall-back to the default — a value that was set was meant.
fn parse_env<T>(key: &str, default: T) -> Result<T, String>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match std::env::var(key) {
        Err(_) => Ok(default),
        Ok(raw) => raw
            .trim()
            .parse::<T>()
            .map_err(|e| format!("{key} `{raw}`: {e}")),
    }
}

/// Resolve a `host:port` string to a `SocketAddr`, accepting both a literal
/// `ip:port` and a DNS name (`brain:9090`) so the edge works unchanged under
/// docker-compose / Kubernetes service DNS, not only with hard-coded IPs.
/// Resolution happens once at startup; a Service name resolves to its stable
/// cluster IP, so a single boot-time lookup is sufficient.
fn resolve_addr(key: &str, raw: &str) -> Result<SocketAddr, String> {
    if let Ok(addr) = raw.parse::<SocketAddr>() {
        return Ok(addr);
    }
    raw.to_socket_addrs()
        .map_err(|e| format!("{key} `{raw}`: {e}"))?
        .next()
        .ok_or_else(|| format!("{key} `{raw}`: resolved to no addresses"))
}

impl EdgeConfig {
    /// Load from the environment with sane self-host defaults.
    ///
    /// - `BRAIN_EDGE_LISTEN`          — HTTP listen addr (default `0.0.0.0:8080`).
    /// - `BRAIN_ADDR`                 — Brain address, `ip:port` or `host:port`
    ///   (default `127.0.0.1:7878`).
    /// - `BRAIN_EDGE_POOL_SIZE`       — pool width per credential (default `4`).
    /// - `BRAIN_EDGE_MAX_CREDENTIALS` — cap on cached credentials (default `256`).
    /// - `BRAIN_EDGE_IDLE_TTL_SECS`   — idle-sweep TTL in seconds (default `900`).
    /// - `BRAIN_EDGE_REQUEST_TIMEOUT_SECS` — per-request timeout (default `30`).
    /// - `BRAIN_EDGE_MAX_BODY_BYTES`  — request body cap (default `1048576` = 1 MiB).
    /// - `BRAIN_EDGE_WIRE_LISTEN`     — wire-proxy listen addr, `ip:port` or
    ///   `host:port` (unset = wire proxy off).
    /// - `BRAIN_EDGE_WIRE_RATE_CAPACITY`      — per-credential burst (default `0` = off).
    /// - `BRAIN_EDGE_WIRE_RATE_REFILL_PER_SEC`— per-credential ops/sec refill (default `0`).
    ///
    /// # Errors
    /// Returns a message if an address fails to parse/resolve, a numeric var is
    /// malformed, or a required-positive value is zero.
    pub fn from_env() -> Result<Self, String> {
        let listen = std::env::var("BRAIN_EDGE_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".into());
        let brain = std::env::var("BRAIN_ADDR").unwrap_or_else(|_| "127.0.0.1:7878".into());

        let pool_size = parse_env::<usize>("BRAIN_EDGE_POOL_SIZE", 4)?;
        let max_credentials = parse_env::<usize>("BRAIN_EDGE_MAX_CREDENTIALS", 256)?;
        let idle_ttl_secs = parse_env::<u64>("BRAIN_EDGE_IDLE_TTL_SECS", 900)?;
        let request_timeout_secs = parse_env::<u64>("BRAIN_EDGE_REQUEST_TIMEOUT_SECS", 30)?;
        let max_body_bytes = parse_env::<usize>("BRAIN_EDGE_MAX_BODY_BYTES", 1024 * 1024)?;

        // The wire proxy is opt-in: absent env var → HTTP-only, unchanged.
        let wire_listen_addr = match std::env::var("BRAIN_EDGE_WIRE_LISTEN") {
            Err(_) => None,
            Ok(raw) => Some(resolve_addr("BRAIN_EDGE_WIRE_LISTEN", &raw)?),
        };
        let wire_rate_capacity = parse_env::<u32>("BRAIN_EDGE_WIRE_RATE_CAPACITY", 0)?;
        let wire_rate_refill_per_sec =
            parse_env::<u32>("BRAIN_EDGE_WIRE_RATE_REFILL_PER_SEC", 0)?;

        if pool_size == 0 {
            return Err("BRAIN_EDGE_POOL_SIZE must be >= 1".into());
        }
        if max_credentials == 0 {
            return Err("BRAIN_EDGE_MAX_CREDENTIALS must be >= 1".into());
        }
        if request_timeout_secs == 0 {
            return Err("BRAIN_EDGE_REQUEST_TIMEOUT_SECS must be >= 1".into());
        }
        if max_body_bytes == 0 {
            return Err("BRAIN_EDGE_MAX_BODY_BYTES must be >= 1".into());
        }

        Ok(Self {
            listen_addr: listen
                .parse()
                .map_err(|e| format!("BRAIN_EDGE_LISTEN `{listen}`: {e}"))?,
            brain_addr: resolve_addr("BRAIN_ADDR", &brain)?,
            pool_size,
            max_credentials,
            idle_ttl_secs,
            request_timeout_secs,
            max_body_bytes,
            wire_listen_addr,
            wire_rate_capacity,
            wire_rate_refill_per_sec,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_addr_accepts_literal_ip_port() {
        let addr = resolve_addr("BRAIN_ADDR", "127.0.0.1:7878").expect("literal ip:port");
        assert_eq!(addr.port(), 7878);
        assert!(addr.ip().is_loopback());
    }

    #[test]
    fn resolve_addr_resolves_a_hostname() {
        // `localhost` resolves offline to a loopback address; this proves a
        // DNS name (not just a literal IP) is accepted — the docker/k8s case.
        let addr = resolve_addr("BRAIN_ADDR", "localhost:9090").expect("hostname:port");
        assert_eq!(addr.port(), 9090);
        assert!(addr.ip().is_loopback());
    }

    #[test]
    fn resolve_addr_rejects_garbage() {
        assert!(resolve_addr("BRAIN_ADDR", "not-an-address").is_err());
        assert!(resolve_addr("BRAIN_ADDR", "").is_err());
    }

    #[test]
    fn parse_env_uses_default_when_unset() {
        // A key we never set falls back to the default rather than erroring.
        let v = parse_env::<usize>("BRAIN_EDGE_TEST_DEFINITELY_UNSET_KEY", 42).expect("default");
        assert_eq!(v, 42);
    }
}
