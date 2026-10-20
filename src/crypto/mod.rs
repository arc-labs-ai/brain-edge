//! Envelope encryption for tenant LLM provider keys (BYOK).
//!
//! # Why this lives in the edge
//!
//! A tenant's OpenAI/Anthropic key is the most sensitive value the platform
//! handles: it bills to their account and it is useful to an attacker on its
//! own. The design goal is that **plaintext exists only in this process**.
//!
//! - **Brain never sees it.** The engine holds a namespace-scoped *grant* and
//!   calls the edge, which swaps the grant for the real key on the way
//!   upstream. A Brain compromise — WAL, snapshots, backups — yields no
//!   provider keys, because none were ever written there.
//! - **The store never sees it.** [`SealedSecret`] is the only shape that
//!   crosses the [`crate::port::LlmCredentialStore`] boundary. Whatever
//!   persists it — Postgres in the hosted gateway, a file in self-host — holds
//!   ciphertext and nothing else, so a database compromise yields nothing
//!   usable without the key-encryption key, which lives in this process's
//!   configuration and never in its data.
//!
//! # The construction
//!
//! AEAD (ChaCha20-Poly1305) under a key-encryption key, with a fresh random
//! 96-bit nonce per seal and the binding context as additional authenticated
//! data.
//!
//! **The AAD is load-bearing, not decoration.** It binds the ciphertext to the
//! namespace and provider it was sealed for, so a stolen row cannot be replayed
//! under a different tenant: moving `acme`'s ciphertext into `globex`'s row
//! makes the tag fail and the open returns an error rather than another
//! tenant's key. Without it, a store with row-level write access would be a
//! cross-tenant key-swap primitive.
//!
//! `kek_id` is carried in the clear beside the ciphertext so several KEKs can
//! coexist. That is what makes rotation possible without a flag day: a new KEK
//! is added, new seals use it, and old rows stay readable until re-sealed.
//!
//! # Why `ring`
//!
//! `ring` is already in this binary's dependency tree (via rustls), so using it
//! adds no new cryptographic implementation to audit. The alternative —
//! pulling a second AEAD crate — would widen the trusted surface to save
//! nothing.

use std::collections::HashMap;
use std::fmt;

use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use zeroize::Zeroizing;

/// Length of a raw key-encryption key, in bytes.
pub const KEK_LEN: usize = 32;

/// What can go wrong sealing or opening a secret.
///
/// Deliberately coarse: a caller must not be able to tell *why* an open failed
/// (wrong key, tampered ciphertext, wrong tenant), because that distinction is
/// an oracle. The operator gets detail through logs, not through this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    /// The configured key material was not a valid KEK.
    InvalidKek,
    /// No KEK is configured for the `kek_id` this ciphertext was sealed under.
    UnknownKekId,
    /// Sealing failed (RNG or AEAD failure).
    SealFailed,
    /// Opening failed: wrong key, tampered ciphertext, or a binding mismatch
    /// (the ciphertext belongs to a different namespace/provider).
    OpenFailed,
}

/// Hand-written rather than derived: this crate carries no `thiserror`
/// dependency, and these messages are deliberately uniform about *why* an open
/// failed.
impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidKek => {
                write!(f, "invalid key-encryption key: must be {KEK_LEN} raw bytes")
            }
            Self::UnknownKekId => {
                write!(f, "no key-encryption key configured for this ciphertext")
            }
            Self::SealFailed => write!(f, "could not seal secret"),
            Self::OpenFailed => write!(f, "could not open secret"),
        }
    }
}

impl std::error::Error for CryptoError {}

/// The context a ciphertext is bound to.
///
/// Both fields enter the AEAD's additional authenticated data, so a ciphertext
/// only opens under the exact pair it was sealed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretBinding<'a> {
    /// Owning namespace (the tenant).
    pub namespace: &'a str,
    /// Provider the key is for (`"openai"`, `"anthropic"`).
    pub provider: &'a str,
}

impl SecretBinding<'_> {
    /// Serialise the binding into AAD bytes.
    ///
    /// Length-prefixed rather than delimiter-joined: with a separator, the
    /// pairs `("a:b", "c")` and `("a", "b:c")` would produce identical AAD and
    /// a ciphertext sealed for one would open for the other. Namespaces are
    /// tenant-supplied, so that ambiguity is reachable.
    fn to_aad_bytes(self) -> Vec<u8> {
        let ns = self.namespace.as_bytes();
        let pr = self.provider.as_bytes();
        let mut out = Vec::with_capacity(16 + ns.len() + pr.len());
        // `u64`, not `u32`: a truncating length prefix would let two distinct
        // bindings produce identical AAD, which is the collision this encoding
        // exists to prevent. `usize -> u64` is lossless on every target this
        // builds for.
        out.extend_from_slice(&(ns.len() as u64).to_be_bytes());
        out.extend_from_slice(ns);
        out.extend_from_slice(&(pr.len() as u64).to_be_bytes());
        out.extend_from_slice(pr);
        out
    }
}

/// An encrypted secret, exactly as it is persisted.
///
/// This is the only shape that crosses the storage boundary. It carries no
/// plaintext and no key material — `kek_id` names which KEK to open it with,
/// it is not itself secret.
///
/// Serialisable so a durable store can persist it verbatim. The byte fields are
/// hex rather than JSON number arrays: a credential file is something an
/// operator will open during an incident, and `"a3f1…"` is inspectable where
/// `[163,241,…]` is not. Hex also keeps the file a third of the size.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SealedSecret {
    /// Which key-encryption key sealed this. Clear-text by design (rotation).
    pub kek_id: String,
    /// Random per-seal nonce.
    #[serde(with = "hex_array")]
    pub nonce: [u8; NONCE_LEN],
    /// Ciphertext with the AEAD tag appended.
    #[serde(with = "hex_vec")]
    pub ciphertext: Vec<u8>,
}

/// Hex serde for the fixed-size nonce.
mod hex_array {
    use super::NONCE_LEN;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(v: &[u8; NONCE_LEN], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::to_hex(v))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<[u8; NONCE_LEN], D::Error> {
        let raw = String::deserialize(d)?;
        let bytes = super::decode_hex(&raw).map_err(serde::de::Error::custom)?;
        <[u8; NONCE_LEN]>::try_from(bytes.as_slice())
            .map_err(|_| serde::de::Error::custom(format!("nonce must be {NONCE_LEN} bytes")))
    }
}

/// Hex serde for the variable-length ciphertext.
mod hex_vec {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::to_hex(v))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let raw = String::deserialize(d)?;
        super::decode_hex(&raw)
            .map(|z| z.to_vec())
            .map_err(serde::de::Error::custom)
    }
}

/// Lowercase-hex encode. Dependency-free; these buffers are small.
fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut acc, b| {
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

/// Never print ciphertext or nonce: a `Debug` on a request-scoped struct is
/// how secrets reach logs. The id and a length are enough to debug with.
impl fmt::Debug for SealedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SealedSecret")
            .field("kek_id", &self.kek_id)
            .field("ciphertext_len", &self.ciphertext.len())
            .finish_non_exhaustive()
    }
}

/// A set of key-encryption keys, one of them current.
///
/// Holds every KEK that can still *open* data, plus which one new seals use.
/// Rotation is therefore additive: add a KEK, point `current` at it, and old
/// ciphertext keeps opening under its own `kek_id` until it is re-sealed.
pub struct KeyRing {
    keys: HashMap<String, LessSafeKey>,
    current: String,
    rng: SystemRandom,
}

/// Keys must never surface in a `Debug`, and `LessSafeKey` has no `Debug`
/// anyway — report the ids only.
impl fmt::Debug for KeyRing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut ids: Vec<&str> = self.keys.keys().map(String::as_str).collect();
        ids.sort_unstable();
        // `finish_non_exhaustive`: the omission is deliberate — key material
        // must never reach a formatter.
        f.debug_struct("KeyRing")
            .field("kek_ids", &ids)
            .field("current", &self.current)
            .finish_non_exhaustive()
    }
}

impl KeyRing {
    /// Build a key ring from `(id, raw 32-byte key)` pairs, with `current`
    /// naming the KEK new seals use.
    ///
    /// # Errors
    /// [`CryptoError::InvalidKek`] if any key is not [`KEK_LEN`] bytes, or if
    /// `current` is not among the supplied ids — a ring that cannot seal is a
    /// configuration error worth failing at startup rather than at first write.
    pub fn new<I>(keys: I, current: impl Into<String>) -> Result<Self, CryptoError>
    where
        I: IntoIterator<Item = (String, Zeroizing<Vec<u8>>)>,
    {
        let mut map = HashMap::new();
        for (id, raw) in keys {
            if raw.len() != KEK_LEN {
                return Err(CryptoError::InvalidKek);
            }
            let unbound =
                UnboundKey::new(&CHACHA20_POLY1305, &raw).map_err(|_| CryptoError::InvalidKek)?;
            map.insert(id, LessSafeKey::new(unbound));
        }
        let current = current.into();
        if !map.contains_key(&current) {
            return Err(CryptoError::InvalidKek);
        }
        Ok(Self {
            keys: map,
            current,
            rng: SystemRandom::new(),
        })
    }

    /// The id new seals are written under.
    #[must_use]
    pub fn current_kek_id(&self) -> &str {
        &self.current
    }

    /// Encrypt `plaintext` under the current KEK, bound to `binding`.
    ///
    /// # Errors
    /// [`CryptoError::SealFailed`] if the RNG or the AEAD fails.
    pub fn seal(
        &self,
        plaintext: &[u8],
        binding: SecretBinding<'_>,
    ) -> Result<SealedSecret, CryptoError> {
        let key = self
            .keys
            .get(&self.current)
            .ok_or(CryptoError::UnknownKekId)?;

        let mut nonce_bytes = [0u8; NONCE_LEN];
        self.rng
            .fill(&mut nonce_bytes)
            .map_err(|_| CryptoError::SealFailed)?;

        let aad = binding.to_aad_bytes();
        let mut buf = plaintext.to_vec();
        key.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce_bytes),
            Aad::from(&aad),
            &mut buf,
        )
        .map_err(|_| CryptoError::SealFailed)?;

        Ok(SealedSecret {
            kek_id: self.current.clone(),
            nonce: nonce_bytes,
            ciphertext: buf,
        })
    }

    /// Decrypt `sealed`, which must have been sealed under the same `binding`.
    ///
    /// The plaintext comes back in a [`Zeroizing`] buffer so it is wiped when
    /// the caller drops it rather than lingering in freed memory.
    ///
    /// # Errors
    /// [`CryptoError::UnknownKekId`] when no configured KEK matches — the
    /// operator removed a key that still has data under it.
    /// [`CryptoError::OpenFailed`] for a wrong key, tampered ciphertext, or a
    /// binding that does not match the one it was sealed with.
    pub fn open(
        &self,
        sealed: &SealedSecret,
        binding: SecretBinding<'_>,
    ) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
        let key = self
            .keys
            .get(&sealed.kek_id)
            .ok_or(CryptoError::UnknownKekId)?;

        let aad = binding.to_aad_bytes();
        let mut buf = sealed.ciphertext.clone();
        let plaintext = key
            .open_in_place(
                Nonce::assume_unique_for_key(sealed.nonce),
                Aad::from(&aad),
                &mut buf,
            )
            .map_err(|_| CryptoError::OpenFailed)?;

        Ok(Zeroizing::new(plaintext.to_vec()))
    }

    /// Re-seal `sealed` under the current KEK. Used by rotation: open with the
    /// old key, seal with the new, same binding.
    ///
    /// # Errors
    /// As [`Self::open`] and [`Self::seal`].
    pub fn reseal(
        &self,
        sealed: &SealedSecret,
        binding: SecretBinding<'_>,
    ) -> Result<SealedSecret, CryptoError> {
        let plaintext = self.open(sealed, binding)?;
        self.seal(&plaintext, binding)
    }
}

impl KeyRing {
    /// Build a ring from a deploy-time spec: `id:hex,id:hex,…`, with `current`
    /// naming which id new seals use.
    ///
    /// Hex rather than base64 so no encoding dependency is needed for 32 bytes,
    /// and so a malformed value fails loudly instead of decoding to the wrong
    /// length. Generate one with `openssl rand -hex 32`.
    ///
    /// # Errors
    /// [`CryptoError::InvalidKek`] on a malformed entry, a key that is not
    /// [`KEK_LEN`] bytes, or a `current` id absent from the spec.
    pub fn from_spec(spec: &str, current: &str) -> Result<Self, CryptoError> {
        let mut keys: Vec<(String, Zeroizing<Vec<u8>>)> = Vec::new();
        for entry in spec.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            // `split_once`, not `split`: a hex body contains no colon, so an
            // extra one means a malformed entry rather than something to
            // silently take the first field of.
            let (id, hex) = entry.split_once(':').ok_or(CryptoError::InvalidKek)?;
            let id = id.trim();
            if id.is_empty() || hex.contains(':') {
                return Err(CryptoError::InvalidKek);
            }
            keys.push((id.to_string(), decode_hex(hex.trim())?));
        }
        if keys.is_empty() {
            return Err(CryptoError::InvalidKek);
        }
        Self::new(keys, current)
    }
}

/// Env var holding the key-encryption keys, as `id:hex,id:hex,…`.
pub const KEK_SPEC_ENV: &str = "BRAIN_EDGE_LLM_KEKS";
/// Env var naming which KEK id new seals use.
pub const KEK_CURRENT_ENV: &str = "BRAIN_EDGE_LLM_KEK_CURRENT";

/// Build the key ring from the environment, or `None` when BYOK is not
/// configured.
///
/// Deliberately **not** a field on `EdgeConfig`: key material must never sit in
/// a struct that something might `Debug`-print or serialise into a startup log.
/// It is read here, moved into the ring, and the spec string is dropped.
///
/// When only one key is configured, `BRAIN_EDGE_LLM_KEK_CURRENT` may be
/// omitted — its id is unambiguous. With several, it is required: guessing
/// which key to seal under is not a safe default.
///
/// # Errors
/// A description of what is malformed. **Never includes the key material** —
/// an error string is the most likely thing to reach a log.
pub fn keyring_from_env() -> Result<Option<KeyRing>, String> {
    let Ok(spec) = std::env::var(KEK_SPEC_ENV) else {
        return Ok(None);
    };
    if spec.trim().is_empty() {
        return Ok(None);
    }

    let ids: Vec<String> = spec
        .split(',')
        .filter_map(|e| e.split_once(':').map(|(id, _)| id.trim().to_string()))
        .filter(|id| !id.is_empty())
        .collect();

    let current = match std::env::var(KEK_CURRENT_ENV) {
        Ok(c) if !c.trim().is_empty() => c.trim().to_string(),
        _ => {
            if ids.len() == 1 {
                ids[0].clone()
            } else {
                return Err(format!(
                    "{KEK_CURRENT_ENV} must name which key id to seal under when \
                     {KEK_SPEC_ENV} carries more than one key"
                ));
            }
        }
    };

    // The error is deliberately generic: `CryptoError` carries no key bytes,
    // and neither does this message.
    KeyRing::from_spec(&spec, &current)
        .map(Some)
        .map_err(|e| format!("{KEK_SPEC_ENV}: {e}"))
}

/// Decode a hex string into a zeroizing buffer.
///
/// The output is [`Zeroizing`] so key material is wiped if construction fails
/// part-way rather than being left in a dropped `Vec`.
fn decode_hex(hex: &str) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    if !hex.len().is_multiple_of(2) {
        return Err(CryptoError::InvalidKek);
    }
    let (pairs, rest) = hex.as_bytes().as_chunks::<2>();
    debug_assert!(rest.is_empty(), "length checked above");
    let mut out = Zeroizing::new(Vec::with_capacity(hex.len() / 2));
    for pair in pairs {
        let hi = hex_nibble(pair[0]).ok_or(CryptoError::InvalidKek)?;
        let lo = hex_nibble(pair[1]).ok_or(CryptoError::InvalidKek)?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

const fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kek(byte: u8) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(vec![byte; KEK_LEN])
    }

    fn ring_with(id: &str, byte: u8) -> KeyRing {
        KeyRing::new([(id.to_string(), kek(byte))], id).expect("valid ring")
    }

    fn binding<'a>(ns: &'a str, provider: &'a str) -> SecretBinding<'a> {
        SecretBinding {
            namespace: ns,
            provider,
        }
    }

    #[test]
    fn seals_and_opens_round_trip() {
        let r = ring_with("k1", 7);
        let b = binding("acme", "openai");
        let sealed = r.seal(b"sk-secret-value", b).unwrap();
        assert_eq!(&*r.open(&sealed, b).unwrap(), b"sk-secret-value");
    }

    #[test]
    fn ciphertext_does_not_contain_the_plaintext() {
        // The property the whole module exists for. If this ever fails, the
        // store is holding readable keys.
        let r = ring_with("k1", 7);
        let secret = b"sk-live-0123456789";
        let sealed = r.seal(secret, binding("acme", "openai")).unwrap();
        assert!(
            !sealed.ciphertext.windows(secret.len()).any(|w| w == secret),
            "plaintext leaked into ciphertext"
        );
    }

    #[test]
    fn a_ciphertext_cannot_be_replayed_under_another_namespace() {
        // The cross-tenant key-swap this binding exists to prevent: an attacker
        // with row-level write access moves acme's ciphertext into globex's row.
        let r = ring_with("k1", 7);
        let sealed = r.seal(b"acme-key", binding("acme", "openai")).unwrap();
        assert_eq!(
            r.open(&sealed, binding("globex", "openai")).unwrap_err(),
            CryptoError::OpenFailed
        );
    }

    #[test]
    fn a_ciphertext_cannot_be_replayed_under_another_provider() {
        let r = ring_with("k1", 7);
        let sealed = r.seal(b"oa-key", binding("acme", "openai")).unwrap();
        assert_eq!(
            r.open(&sealed, binding("acme", "anthropic")).unwrap_err(),
            CryptoError::OpenFailed
        );
    }

    #[test]
    fn binding_is_unambiguous_across_separator_lookalikes() {
        // Length-prefixed AAD: with a `:` join, ("a:b","c") and ("a","b:c")
        // would collide and a ciphertext sealed for one would open for the
        // other. Namespaces are tenant-supplied, so this is reachable.
        let r = ring_with("k1", 7);
        let sealed = r.seal(b"v", binding("a:b", "c")).unwrap();
        assert_eq!(
            r.open(&sealed, binding("a", "b:c")).unwrap_err(),
            CryptoError::OpenFailed
        );
    }

    #[test]
    fn a_tampered_ciphertext_does_not_open() {
        let r = ring_with("k1", 7);
        let b = binding("acme", "openai");
        let mut sealed = r.seal(b"sk-secret", b).unwrap();
        sealed.ciphertext[0] ^= 0xFF;
        assert_eq!(r.open(&sealed, b).unwrap_err(), CryptoError::OpenFailed);
    }

    #[test]
    fn a_wrong_key_does_not_open() {
        let a = ring_with("k1", 1);
        let bogus = ring_with("k1", 2); // same id, different material
        let b = binding("acme", "openai");
        let sealed = a.seal(b"sk-secret", b).unwrap();
        assert_eq!(bogus.open(&sealed, b).unwrap_err(), CryptoError::OpenFailed);
    }

    #[test]
    fn nonces_differ_across_seals_of_identical_input() {
        // Nonce reuse under one key breaks ChaCha20-Poly1305 catastrophically,
        // so identical plaintext must not produce identical ciphertext.
        let r = ring_with("k1", 7);
        let b = binding("acme", "openai");
        let one = r.seal(b"same", b).unwrap();
        let two = r.seal(b"same", b).unwrap();
        assert_ne!(one.nonce, two.nonce);
        assert_ne!(one.ciphertext, two.ciphertext);
    }

    #[test]
    fn rotation_keeps_old_ciphertext_readable() {
        // The point of carrying kek_id: adding a KEK must not strand data.
        let r = KeyRing::new(
            [("old".to_string(), kek(1)), ("new".to_string(), kek(2))],
            "new",
        )
        .unwrap();
        let b = binding("acme", "openai");

        // Simulate a row sealed before rotation.
        let old_ring = ring_with("old", 1);
        let old_sealed = old_ring.seal(b"legacy", b).unwrap();
        assert_eq!(old_sealed.kek_id, "old");

        assert_eq!(&*r.open(&old_sealed, b).unwrap(), b"legacy");

        let resealed = r.reseal(&old_sealed, b).unwrap();
        assert_eq!(resealed.kek_id, "new");
        assert_eq!(&*r.open(&resealed, b).unwrap(), b"legacy");
    }

    #[test]
    fn an_unknown_kek_id_is_distinguishable_from_a_failed_open() {
        // The operator needs to tell "I removed a key that still has data"
        // from "this ciphertext is bad"; a caller gets neither (both are 5xx).
        let r = ring_with("k1", 7);
        let b = binding("acme", "openai");
        let mut sealed = r.seal(b"v", b).unwrap();
        sealed.kek_id = "retired".to_string();
        assert_eq!(r.open(&sealed, b).unwrap_err(), CryptoError::UnknownKekId);
    }

    #[test]
    fn a_short_key_is_rejected_at_construction() {
        let short = Zeroizing::new(vec![0u8; KEK_LEN - 1]);
        assert_eq!(
            KeyRing::new([("k1".to_string(), short)], "k1").unwrap_err(),
            CryptoError::InvalidKek
        );
    }

    #[test]
    fn a_current_id_not_in_the_ring_is_rejected() {
        // A ring that cannot seal fails at startup, not at first write.
        assert_eq!(
            KeyRing::new([("k1".to_string(), kek(1))], "k2").unwrap_err(),
            CryptoError::InvalidKek
        );
    }

    #[test]
    fn debug_impls_do_not_leak_secrets() {
        let r = ring_with("k1", 7);
        let sealed = r
            .seal(b"sk-super-secret", binding("acme", "openai"))
            .unwrap();
        let d = format!("{sealed:?}");
        assert!(!d.contains("sk-super-secret"));
        assert!(!d.contains("nonce"), "nonce must not be printed: {d}");
        assert!(d.contains("k1"), "kek_id is safe and useful: {d}");
        // The ring prints ids, never material.
        let rd = format!("{r:?}");
        assert!(rd.contains("k1"));
        assert!(!rd.contains('\u{7}'));
    }
}

#[cfg(test)]
mod spec_tests {
    use super::*;

    const K1: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const K2: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    #[test]
    fn parses_a_single_key_spec() {
        let r = KeyRing::from_spec(&format!("k1:{K1}"), "k1").unwrap();
        assert_eq!(r.current_kek_id(), "k1");
    }

    #[test]
    fn parses_a_rotation_spec_and_seals_under_current() {
        let r = KeyRing::from_spec(&format!("old:{K1},new:{K2}"), "new").unwrap();
        assert_eq!(r.current_kek_id(), "new");
        let sealed = r
            .seal(
                b"v",
                SecretBinding {
                    namespace: "acme",
                    provider: "openai",
                },
            )
            .unwrap();
        assert_eq!(sealed.kek_id, "new");
    }

    #[test]
    fn rejects_malformed_specs_rather_than_guessing() {
        for bad in [
            "",                           // empty
            "k1",                         // no colon
            &format!("k1:{K1}:extra"),    // stray colon
            &format!(":{K1}"),            // empty id
            "k1:zz",                      // non-hex
            "k1:111",                     // odd length
            &format!("k1:{}", &K1[..62]), // 31 bytes, not 32
        ] {
            assert_eq!(
                KeyRing::from_spec(bad, "k1").unwrap_err(),
                CryptoError::InvalidKek,
                "should reject {bad:?}"
            );
        }
    }

    #[test]
    fn rejects_a_current_id_absent_from_the_spec() {
        assert_eq!(
            KeyRing::from_spec(&format!("k1:{K1}"), "k2").unwrap_err(),
            CryptoError::InvalidKek
        );
    }

    #[test]
    fn accepts_uppercase_hex_and_surrounding_whitespace() {
        let spec = format!(" k1 : {} ", K1.to_uppercase());
        assert!(KeyRing::from_spec(&spec, "k1").is_ok());
    }
}
