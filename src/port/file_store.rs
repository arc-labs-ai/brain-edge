//! A durable, file-backed [`LlmCredentialStore`] for self-hosted edges.
//!
//! The in-memory default loses every registered provider key on restart, which
//! then fails the whole write path until each tenant re-registers. That is a
//! reasonable default for a first look at BYOK and a bad one for anything left
//! running, so this is the self-host answer that does not require a database.
//!
//! # Why a plain file is safe here
//!
//! The file holds [`SealedSecret`]s and nothing else. The key-encryption key
//! lives in the process environment and never in the file, so the file on its
//! own is inert — copying it off the host yields ciphertext bound by AAD to a
//! namespace and provider, openable only with a KEK that was never written
//! beside it.
//!
//! That is the same property the hosted gateway gets from its database. The
//! storage medium was never the control.
//!
//! # Durability
//!
//! Writes are **atomic**: serialise, write a sibling temp file, fsync it,
//! rename over the target. A rename within a directory is atomic on POSIX, so
//! a crash leaves either the old file or the new one — never a half-written
//! one. Without that, a crash mid-write truncates the file and every tenant's
//! key is gone, which is precisely the failure this module exists to remove.

use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use std::sync::RwLock;

use super::{LlmCredentialStore, StoredLlmCredential};
use crate::crypto::SealedSecret;
use crate::error::ApiError;

/// On-disk shape. Versioned so a later format change is a migration rather
/// than a parse failure that reads as "all your keys are gone".
#[derive(Serialize, Deserialize)]
struct FileDoc {
    version: u32,
    records: HashMap<String, FileRecord>,
}

/// One tenant's stored credential, as persisted.
#[derive(Clone, Serialize, Deserialize)]
struct FileRecord {
    provider: String,
    model: String,
    sealed: SealedSecret,
    fingerprint: String,
    created_at_unix_secs: u64,
}

const CURRENT_VERSION: u32 = 1;

impl From<StoredLlmCredential> for FileRecord {
    fn from(r: StoredLlmCredential) -> Self {
        Self {
            provider: r.provider,
            model: r.model,
            sealed: r.sealed,
            fingerprint: r.fingerprint,
            created_at_unix_secs: r.created_at_unix_secs,
        }
    }
}

impl From<FileRecord> for StoredLlmCredential {
    fn from(r: FileRecord) -> Self {
        Self {
            provider: r.provider,
            model: r.model,
            sealed: r.sealed,
            fingerprint: r.fingerprint,
            created_at_unix_secs: r.created_at_unix_secs,
        }
    }
}

/// A [`LlmCredentialStore`] backed by one JSON file of sealed credentials.
#[derive(Debug)]
pub struct FileLlmCredentialStore {
    path: PathBuf,
    /// Mirrors the file so reads — which happen on every LLM call — do not
    /// touch the disk. The file is the durable copy; this is the fast one.
    records: RwLock<HashMap<String, FileRecord>>,
}

/// `FileRecord` holds ciphertext only, but a `Debug` that dumps every tenant's
/// row into a log is still noise at best. Report the count.
impl std::fmt::Debug for FileRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileRecord")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl FileLlmCredentialStore {
    /// Open (or create) the store at `path`.
    ///
    /// A missing file is an empty store, not an error — first run is the
    /// normal case.
    ///
    /// # Errors
    /// If the file exists but cannot be read or parsed. Deliberately **not**
    /// silently reset: a corrupt file is an incident, and starting empty would
    /// let every tenant's key quietly disappear while the service reports
    /// healthy.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let records = match fs::read_to_string(&path) {
            Ok(raw) => {
                let doc: FileDoc = serde_json::from_str(&raw).map_err(|e| {
                    format!(
                        "credential store at {} is unreadable ({e}); refusing to start empty \
                         rather than silently discarding stored keys",
                        path.display()
                    )
                })?;
                if doc.version != CURRENT_VERSION {
                    return Err(format!(
                        "credential store at {} is version {}, this build understands {}",
                        path.display(),
                        doc.version,
                        CURRENT_VERSION
                    ));
                }
                doc.records
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => {
                return Err(format!(
                    "could not read credential store at {}: {e}",
                    path.display()
                ));
            }
        };
        Ok(Self {
            path,
            records: RwLock::new(records),
        })
    }

    /// Persist the current map atomically.
    fn flush(&self, records: &HashMap<String, FileRecord>) -> Result<(), String> {
        let doc = FileDoc {
            version: CURRENT_VERSION,
            records: records.clone(),
        };
        let json = serde_json::to_vec_pretty(&doc)
            .map_err(|e| format!("serialise credential store: {e}"))?;

        // Temp file beside the target, so the rename stays within one
        // filesystem — a rename across filesystems is not atomic and would
        // silently become copy-then-delete.
        let tmp = self.path.with_extension("tmp");
        {
            let mut f =
                fs::File::create(&tmp).map_err(|e| format!("create {}: {e}", tmp.display()))?;
            restrict_permissions(&f)?;
            f.write_all(&json)
                .map_err(|e| format!("write {}: {e}", tmp.display()))?;
            // fsync before rename: the rename can otherwise land while the
            // contents are still in the page cache, so a power loss leaves a
            // correctly-named, empty file.
            f.sync_all()
                .map_err(|e| format!("sync {}: {e}", tmp.display()))?;
        }
        fs::rename(&tmp, &self.path)
            .map_err(|e| format!("rename {} to {}: {e}", tmp.display(), self.path.display()))
    }

    /// Mutate the map and persist, rolling back in memory if the write fails.
    ///
    /// Without the rollback, a full disk would leave the process serving a
    /// credential the file does not contain, which survives until a restart
    /// silently reverts it.
    ///
    /// The flush happens under the write lock, so the file and the map cannot
    /// disagree. That blocks the executor thread for the duration of a small
    /// write — acceptable because credential writes happen at registration and
    /// rotation only, while the hot path (`get`) takes a read lock and never
    /// touches the disk.
    fn mutate<T>(
        &self,
        f: impl FnOnce(&mut HashMap<String, FileRecord>) -> T,
    ) -> Result<T, ApiError> {
        let mut guard = self
            .records
            .write()
            .map_err(|_| ApiError::internal("credential store lock poisoned"))?;
        let before = guard.clone();
        let out = f(&mut guard);
        if let Err(e) = self.flush(&guard) {
            *guard = before;
            tracing::error!(
                target: "brain_edge::credential_store",
                error = %e,
                "credential store write failed; in-memory state rolled back",
            );
            return Err(ApiError::internal("could not persist credential store"));
        }
        Ok(out)
    }
}

/// Restrict the credential file to the owner.
///
/// Ciphertext, so not catastrophic if read — but a file whose name says
/// "credentials" should not be world-readable, and defence in depth is cheap.
#[cfg(unix)]
fn restrict_permissions(file: &fs::File) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("chmod credential store: {e}"))
}

#[cfg(not(unix))]
fn restrict_permissions(_file: &fs::File) -> Result<(), String> {
    // No portable equivalent; the file holds ciphertext either way.
    Ok(())
}

#[async_trait::async_trait]
impl LlmCredentialStore for FileLlmCredentialStore {
    async fn put(&self, namespace: &str, record: StoredLlmCredential) -> Result<(), ApiError> {
        self.mutate(|m| {
            m.insert(namespace.to_string(), FileRecord::from(record));
        })
    }

    async fn get(&self, namespace: &str) -> Result<Option<StoredLlmCredential>, ApiError> {
        let guard = self
            .records
            .read()
            .map_err(|_| ApiError::internal("credential store lock poisoned"))?;
        Ok(guard.get(namespace).cloned().map(StoredLlmCredential::from))
    }

    async fn delete(&self, namespace: &str) -> Result<bool, ApiError> {
        self.mutate(|m| m.remove(namespace).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{KEK_LEN, KeyRing, SecretBinding};
    use zeroize::Zeroizing;

    fn ring() -> KeyRing {
        KeyRing::new(
            [("k1".to_string(), Zeroizing::new(vec![5u8; KEK_LEN]))],
            "k1",
        )
        .unwrap()
    }

    fn binding<'a>() -> SecretBinding<'a> {
        SecretBinding {
            namespace: "acme",
            provider: "openai",
        }
    }

    fn record(ring: &KeyRing, secret: &str) -> StoredLlmCredential {
        StoredLlmCredential {
            provider: "openai".into(),
            model: "gpt-4o-mini".into(),
            sealed: ring.seal(secret.as_bytes(), binding()).unwrap(),
            fingerprint: super::super::key_fingerprint(secret),
            created_at_unix_secs: 42,
        }
    }

    #[tokio::test]
    async fn a_stored_key_survives_a_restart() {
        // The entire reason this module exists.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        let r = ring();

        let store = FileLlmCredentialStore::open(&path).unwrap();
        store
            .put("acme", record(&r, "sk-live-secret"))
            .await
            .unwrap();

        // Drop and reopen: a fresh process reading the same file.
        drop(store);
        let reopened = FileLlmCredentialStore::open(&path).unwrap();
        let got = reopened
            .get("acme")
            .await
            .unwrap()
            .expect("survived restart");
        assert_eq!(got.model, "gpt-4o-mini");
        assert_eq!(&*r.open(&got.sealed, binding()).unwrap(), b"sk-live-secret");
    }

    #[tokio::test]
    async fn the_file_never_contains_the_plaintext_key() {
        // The property that makes a plain file acceptable at all.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        let r = ring();
        let store = FileLlmCredentialStore::open(&path).unwrap();
        store
            .put("acme", record(&r, "sk-live-0123456789"))
            .await
            .unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("sk-live-0123456789"), "plaintext on disk");
        assert!(raw.contains("k1"), "kek_id is present for rotation");
        // And no key-encryption key either — it lives in the environment.
        assert!(!raw.contains(&"05".repeat(KEK_LEN)));
    }

    #[tokio::test]
    async fn a_missing_file_is_an_empty_store_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileLlmCredentialStore::open(dir.path().join("absent.json")).unwrap();
        assert!(store.get("acme").await.unwrap().is_none());
    }

    #[test]
    fn a_corrupt_file_refuses_to_start_rather_than_starting_empty() {
        // Starting empty would let every tenant's key quietly disappear while
        // the service reports healthy — the failure is loud on purpose.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        fs::write(&path, b"{not json").unwrap();
        let err = FileLlmCredentialStore::open(&path).unwrap_err();
        assert!(err.contains("unreadable"), "{err}");
        assert!(err.contains("refusing to start empty"), "{err}");
    }

    #[test]
    fn an_unknown_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        fs::write(&path, br#"{"version":99,"records":{}}"#).unwrap();
        let err = FileLlmCredentialStore::open(&path).unwrap_err();
        assert!(err.contains("version 99"), "{err}");
    }

    #[tokio::test]
    async fn delete_reports_whether_anything_was_removed_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        let store = FileLlmCredentialStore::open(&path).unwrap();
        store.put("acme", record(&ring(), "sk-x")).await.unwrap();

        assert!(store.delete("acme").await.unwrap());
        assert!(
            !store.delete("acme").await.unwrap(),
            "second delete is false"
        );

        let reopened = FileLlmCredentialStore::open(&path).unwrap();
        assert!(
            reopened.get("acme").await.unwrap().is_none(),
            "delete persisted"
        );
    }

    #[tokio::test]
    async fn one_tenants_record_does_not_leak_to_another() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileLlmCredentialStore::open(dir.path().join("c.json")).unwrap();
        store.put("acme", record(&ring(), "sk-x")).await.unwrap();
        assert!(store.get("globex").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn no_temp_file_is_left_behind() {
        // A stray `.tmp` beside the store would be a second copy of every
        // tenant's ciphertext that nothing cleans up.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        let store = FileLlmCredentialStore::open(&path).unwrap();
        store.put("acme", record(&ring(), "sk-x")).await.unwrap();
        assert!(!path.with_extension("tmp").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("creds.json");
        let store = FileLlmCredentialStore::open(&path).unwrap();
        store.put("acme", record(&ring(), "sk-x")).await.unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "credential file must not be readable by others"
        );
    }
}
