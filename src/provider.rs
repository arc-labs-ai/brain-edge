//! Talking to a provider directly: which ones this build knows, and whether a
//! key works.
//!
//! This was a proxy. Brain resolved a tenant's credential by calling back
//! here, so the edge held the sealed key and forwarded completions on Brain's
//! behalf. Brain now holds its own credentials and calls providers itself, so
//! what remains is the part the HTTP surface still needs: the provider list,
//! and a cheap probe that rejects a mistyped key at registration rather than
//! on every later write.

/// Where each provider lives. Probing reaches these; nothing is proxied
/// through them any more.
const OPENAI_BASE: &str = "https://api.openai.com";
const ANTHROPIC_BASE: &str = "https://api.anthropic.com";

/// Canonical wire name for OpenAI, as a credential names it.
pub const PROVIDER_OPENAI: &str = "openai";
/// Canonical wire name for Anthropic.
pub const PROVIDER_ANTHROPIC: &str = "anthropic";

/// Which upstream a route serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upstream {
    /// OpenAI's chat-completions API.
    OpenAi,
    /// Anthropic's messages API.
    Anthropic,
}

impl Upstream {
    /// Every upstream this edge can serve. The single source of truth: the
    /// accepted-provider list and the credential validator both derive from
    /// it, so adding a provider cannot leave one of them behind.
    pub const ALL: [Self; 2] = [Self::OpenAi, Self::Anthropic];

    /// Canonical wire name.
    #[must_use]
    pub const fn provider(self) -> &'static str {
        match self {
            Self::OpenAi => PROVIDER_OPENAI,
            Self::Anthropic => PROVIDER_ANTHROPIC,
        }
    }

    /// Parse a canonical provider name. The inverse of [`Self::provider`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|u| u.provider() == name)
    }

    const fn base(self) -> &'static str {
        match self {
            Self::OpenAi => OPENAI_BASE,
            Self::Anthropic => ANTHROPIC_BASE,
        }
    }
}

/// Outcome of probing a provider key before storing it.
///
/// The three-way split mirrors the credential-resolution one, for the same
/// reason: "the provider says this key is bad" and "I could not reach the
/// provider" demand opposite responses, and collapsing them would either
/// accept dead keys during an outage or refuse good ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyCheck {
    /// The provider accepted the key.
    Valid,
    /// The provider rejected it — a typo or a revoked key.
    Rejected(String),
    /// The provider could not be reached, or answered something unexpected.
    Unreachable(String),
}

/// Classify a probe response status.
///
/// Only `401`/`403` are treated as "this key is bad". A `429` or `5xx` says
/// nothing about the key — rejecting on those would make key registration fail
/// whenever the provider is rate-limiting or having an incident.
fn classify_probe(status: u16) -> KeyCheck {
    match status {
        200..=299 => KeyCheck::Valid,
        401 | 403 => KeyCheck::Rejected(format!("the provider rejected this key ({status})")),
        other => KeyCheck::Unreachable(format!("provider returned {other} while verifying")),
    }
}

/// Probe a provider key with a cheap read-only call before storing it.
///
/// Without this a typo'd key is accepted happily and then fails on every write
/// with an opaque upstream `401` that looks like a Brain bug. Catching it at
/// registration puts the error where the mistake was made.
///
/// `GET /v1/models` on both providers: read-only, no tokens billed, and it
/// exercises exactly the credential path a real call would.
pub async fn validate_key(upstream: Upstream, key: &str) -> KeyCheck {
    let url = format!("{}/v1/models", upstream.base());
    let client = reqwest::Client::new();
    let req = match upstream {
        Upstream::OpenAi => client.get(&url).bearer_auth(key),
        Upstream::Anthropic => client
            .get(&url)
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
    };

    match req.send().await {
        Ok(resp) => classify_probe(resp.status().as_u16()),
        Err(e) => {
            // Logged, not returned: a reqwest error can embed the request
            // headers, which carry the key being verified.
            tracing::warn!(
                target: "brain_edge::provider",
                provider = upstream.provider(),
                error = %e,
                "could not reach provider to verify key",
            );
            KeyCheck::Unreachable("could not reach the provider to verify this key".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upstream_bases_are_fixed() {
        assert_eq!(Upstream::OpenAi.base(), "https://api.openai.com");
        assert_eq!(Upstream::Anthropic.base(), "https://api.anthropic.com");
    }

    /// `ALL` is the single source of truth the accepted-provider list and the
    /// key validator both derive from, so a provider added to one cannot go
    /// missing from the other.
    #[test]
    fn provider_names_round_trip() {
        for u in Upstream::ALL {
            assert_eq!(Upstream::from_name(u.provider()), Some(u));
        }
        assert_eq!(Upstream::from_name("not-a-provider"), None);
    }

    /// Only an explicit rejection means the key is bad. A rate limit or an
    /// outage says nothing about it, and treating those as rejection would
    /// fail registration whenever the provider is having a bad day.
    #[test]
    fn only_an_explicit_rejection_condemns_the_key() {
        assert_eq!(classify_probe(200), KeyCheck::Valid);
        assert!(matches!(classify_probe(401), KeyCheck::Rejected(_)));
        assert!(matches!(classify_probe(403), KeyCheck::Rejected(_)));
        assert!(matches!(classify_probe(429), KeyCheck::Unreachable(_)));
        assert!(matches!(classify_probe(503), KeyCheck::Unreachable(_)));
    }
}
