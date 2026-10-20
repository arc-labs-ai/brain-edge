//! BYOK provider-credential verbs: register / inspect / remove a tenant's own
//! OpenAI or Anthropic key.
//!
//! The asymmetry here is deliberate and is the whole contract: a plaintext key
//! goes **in** exactly once, and nothing that comes **out** can reconstruct it.
//! [`LlmCredentialDto`] carries a fingerprint, never the key, because once
//! sealed the edge is the only thing that can open it and there is no reason to
//! hand it back.

// See `dto/mod.rs`: JSON field names are the API contract and are emitted into
// `contract/http-routes.json` for the SDK conformance check.
#![allow(missing_docs)]

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::port::StoredLlmCredential;

/// The providers a tenant may register a key for.
///
/// Derived from [`crate::app::handlers::llm_proxy::Upstream`] rather than
/// listed separately: the value selects which upstream the proxy talks to and
/// enters the sealing AAD, so a provider accepted here but unroutable there
/// would let a caller mint ciphertext nothing can ever serve.
pub fn supported_providers() -> Vec<&'static str> {
    use crate::app::handlers::llm_proxy::Upstream;
    Upstream::ALL.into_iter().map(Upstream::provider).collect()
}

/// `PUT /v1/llm-credential` request — the only shape carrying a plaintext key.
#[derive(Deserialize)]
pub struct PutLlmCredentialRequest {
    /// `"openai"` or `"anthropic"`.
    pub provider: String,
    /// Model this key should serve (e.g. `gpt-4o-mini`, `claude-haiku-4-5`).
    pub model: String,
    /// The provider key. Sealed immediately and never stored or returned.
    pub api_key: String,
}

/// Redacting `Debug`, written by hand rather than derived.
///
/// A derived `Debug` on a request body is one `tracing` call away from putting
/// a live provider key in a log file, and request-logging middleware is exactly
/// the kind of thing that gets added later by someone who has not read this
/// file.
impl fmt::Debug for PutLlmCredentialRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PutLlmCredentialRequest")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("api_key", &"[redacted]")
            .finish()
    }
}

impl PutLlmCredentialRequest {
    /// Validate the request, returning the normalised provider.
    ///
    /// # Errors
    /// A human-readable reason. **Never quotes the key**, including on
    /// failure — an error string is the most likely thing to be logged.
    pub fn validate(&self) -> Result<&'static str, String> {
        let provider = self.provider.trim().to_ascii_lowercase();
        let Some(known) = crate::app::handlers::llm_proxy::Upstream::from_name(&provider) else {
            return Err(format!(
                "unsupported provider `{}`: expected one of {}",
                self.provider,
                supported_providers().join(", ")
            ));
        };
        let known = known.provider();
        if self.model.trim().is_empty() {
            return Err("`model` must not be empty".into());
        }
        if self.api_key.trim().is_empty() {
            return Err("`api_key` must not be empty".into());
        }
        // A key with surrounding whitespace is almost always a copy-paste
        // artifact; it would be sealed verbatim and then fail upstream with an
        // opaque 401 that looks like a wrong key rather than a stray space.
        if self.api_key != self.api_key.trim() {
            return Err("`api_key` has leading or trailing whitespace".into());
        }
        Ok(known)
    }
}

/// `GET /v1/llm-credential` response — what is installed, never the key.
#[derive(Debug, Serialize)]
pub struct LlmCredentialDto {
    pub provider: String,
    pub model: String,
    /// Display-safe tail (e.g. `…4f2a`) so an operator can tell which key is
    /// installed. Not reversible into the key.
    pub fingerprint: String,
    pub created_at_unix_secs: u64,
}

impl From<StoredLlmCredential> for LlmCredentialDto {
    fn from(r: StoredLlmCredential) -> Self {
        Self {
            provider: r.provider,
            model: r.model,
            fingerprint: r.fingerprint,
            created_at_unix_secs: r.created_at_unix_secs,
        }
    }
}

/// `GET /llm/ns/{namespace}/resolve` response — what Brain needs to route.
///
/// Deliberately carries **no key material**: Brain learns which provider and
/// model to build a client for and nothing else, which is what lets it hold
/// this in memory without that constituting a key at rest.
#[derive(Debug, Serialize, Deserialize)]
pub struct ResolvedLlmDto {
    pub provider: String,
    pub model: String,
}

/// `DELETE /v1/llm-credential` response.
#[derive(Debug, Serialize)]
pub struct DeleteLlmCredentialDto {
    /// Whether a credential existed to remove. Reported rather than always
    /// 204 so a tenant can tell "removed" from "there was nothing there".
    pub deleted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(provider: &str, model: &str, key: &str) -> PutLlmCredentialRequest {
        PutLlmCredentialRequest {
            provider: provider.into(),
            model: model.into(),
            api_key: key.into(),
        }
    }

    #[test]
    fn accepts_both_supported_providers_case_insensitively() {
        assert_eq!(
            req("openai", "gpt-4o-mini", "sk-x").validate().unwrap(),
            "openai"
        );
        assert_eq!(
            req("ANTHROPIC", "claude-haiku-4-5", "sk-x")
                .validate()
                .unwrap(),
            "anthropic"
        );
        assert_eq!(
            req(" OpenAI ", "gpt-4o", "sk-x").validate().unwrap(),
            "openai"
        );
    }

    #[test]
    fn rejects_an_unknown_provider() {
        let err = req("cohere", "m", "sk-x").validate().unwrap_err();
        assert!(err.contains("cohere"), "{err}");
        assert!(err.contains("openai"), "names the accepted set: {err}");
    }

    #[test]
    fn rejects_empty_model_and_key() {
        assert!(req("openai", "", "sk-x").validate().is_err());
        assert!(req("openai", "m", "").validate().is_err());
        assert!(req("openai", "m", "   ").validate().is_err());
    }

    #[test]
    fn rejects_a_key_with_stray_whitespace() {
        // Sealed verbatim, this would fail upstream as an opaque 401 that looks
        // like a wrong key rather than a copy-paste artifact.
        let err = req("openai", "m", " sk-x\n").validate().unwrap_err();
        assert!(err.contains("whitespace"), "{err}");
    }

    #[test]
    fn no_validation_error_ever_quotes_the_key() {
        // Error strings are the most likely thing to reach a log.
        let secret = "sk-live-must-never-appear";
        for r in [
            req("cohere", "m", secret),
            req("openai", "", secret),
            req("openai", "m", &format!(" {secret} ")),
        ] {
            let err = r.validate().unwrap_err();
            assert!(!err.contains(secret), "key leaked into error: {err}");
        }
    }

    #[test]
    fn debug_redacts_the_key() {
        let d = format!("{:?}", req("openai", "gpt-4o-mini", "sk-live-secret"));
        assert!(!d.contains("sk-live-secret"), "{d}");
        assert!(d.contains("[redacted]"), "{d}");
        assert!(
            d.contains("gpt-4o-mini"),
            "non-secret fields stay useful: {d}"
        );
    }

    #[test]
    fn the_response_shape_cannot_carry_a_key() {
        // Structural, not incidental: there is no field to put one in.
        let dto = LlmCredentialDto {
            provider: "openai".into(),
            model: "gpt-4o-mini".into(),
            fingerprint: "…4f2a".into(),
            created_at_unix_secs: 7,
        };
        let json = serde_json::to_string(&dto).unwrap();
        assert!(json.contains("…4f2a"));
        assert!(!json.contains("api_key"), "{json}");
    }
}
