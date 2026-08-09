use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::RwLock;
use tokio::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredToken {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix millis
    pub expires_at: i64,
}

/// Refresh this far ahead of expiry, same margin as the TS original.
/// Shared so `auth_status` reports the same boundary that triggers a refresh,
/// rather than restating the number and drifting from it.
pub const REFRESH_MARGIN_MS: i64 = 300_000;

impl StoredToken {
    pub fn needs_refresh(&self) -> bool {
        let now = chrono::Utc::now().timestamp_millis();
        self.expires_at - REFRESH_MARGIN_MS < now
    }
}

/// Encrypted-at-rest OAuth token store.
///
/// Security note (fix vs. the original TS `token-store.ts`): that file fell
/// back to a hardcoded, publicly-known AES key
/// (`'default-insecure-key-32-bytes!'`) with just a warning log if
/// `ENCRYPTION_KEY` wasn't set, silently leaving tokens trivially
/// decryptable by anyone with the source. This port has no such fallback —
/// `Config::load()` refuses to start without a real key.
pub struct TokenStore {
    path: PathBuf,
    key: [u8; 32],
    cached: RwLock<Option<StoredToken>>,
}

impl TokenStore {
    pub fn new(path: impl Into<PathBuf>, key_b64: &str) -> anyhow::Result<Self> {
        let key = derive_key(key_b64)?;
        Ok(Self {
            path: path.into(),
            key,
            cached: RwLock::new(None),
        })
    }

    pub async fn load(&self) -> anyhow::Result<Option<StoredToken>> {
        if let Some(tok) = self.cached.read().unwrap().clone() {
            return Ok(Some(tok));
        }
        let bytes = match fs::read(&self.path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let text = String::from_utf8(bytes)?;
        let token = self.decrypt(&text)?;
        *self.cached.write().unwrap() = Some(token.clone());
        Ok(Some(token))
    }

    pub async fn save(&self, token: &StoredToken) -> anyhow::Result<()> {
        let encrypted = self.encrypt(token)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, encrypted).await?;
        fs::rename(&tmp, &self.path).await?;
        // Cache only after the token is durably on disk. Caching before the
        // write made auth_status report "Authenticated" for the rest of the
        // process lifetime even when the write failed (seen in practice with
        // an unwritable TOKEN_STORAGE_PATH), masking the lost token until the
        // next restart.
        *self.cached.write().unwrap() = Some(token.clone());
        Ok(())
    }

    fn encrypt(&self, token: &StoredToken) -> anyhow::Result<String> {
        let json = serde_json::to_vec(token)?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.key));
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher
            .encrypt(nonce, json.as_ref())
            .map_err(|e| anyhow::anyhow!("encryption failed: {e}"))?;
        let engine = base64::engine::general_purpose::STANDARD;
        Ok(format!(
            "{}:{}",
            engine.encode(nonce_bytes),
            engine.encode(ciphertext)
        ))
    }

    fn decrypt(&self, data: &str) -> anyhow::Result<StoredToken> {
        let engine = base64::engine::general_purpose::STANDARD;
        let (nonce_b64, ct_b64) = data
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("invalid encrypted token file format"))?;
        let nonce_bytes = engine.decode(nonce_b64)?;
        let ciphertext = engine.decode(ct_b64)?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.key));
        let nonce = Nonce::from_slice(&nonce_bytes);
        let plaintext = cipher
            .decrypt(nonce, ciphertext.as_ref())
            .map_err(|e| anyhow::anyhow!("decryption failed (wrong ENCRYPTION_KEY?): {e}"))?;
        Ok(serde_json::from_slice(&plaintext)?)
    }
}

/// Derive a 32-byte AES-256 key from the configured ENCRYPTION_KEY value.
/// Accepts base64 (as `openssl rand -base64 32` produces); falls back to
/// padding/truncating raw bytes to 32 if it isn't valid base64, matching the
/// original's leniency (minus the insecure default).
fn derive_key(key_b64: &str) -> anyhow::Result<[u8; 32]> {
    let engine = base64::engine::general_purpose::STANDARD;
    if let Ok(decoded) = engine.decode(key_b64) {
        if decoded.len() == 32 {
            let mut out = [0u8; 32];
            out.copy_from_slice(&decoded);
            return Ok(out);
        }
    }
    let raw = key_b64.as_bytes();
    if raw.len() < 32 {
        anyhow::bail!(
            "ENCRYPTION_KEY must decode to (or be) at least 32 bytes; got {} bytes",
            raw.len()
        );
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&raw[..32]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="; // 32 zero bytes, base64
    const KEY_B: &str = "//////////////////////////////////////////8="; // 32 0xff bytes, base64

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ytmusic-rs-test-{}-{}-{}",
            std::process::id(),
            name,
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ))
    }

    #[test]
    fn derive_key_accepts_32_byte_base64() {
        let key = derive_key(KEY_A).unwrap();
        assert_eq!(key, [0u8; 32]);
    }

    #[test]
    fn derive_key_falls_back_to_raw_bytes_when_not_valid_base64_length() {
        // 32 raw ASCII chars — not valid base64 for a 32-byte payload, so this
        // exercises the raw-bytes fallback path rather than the base64 path.
        let raw = "x".repeat(32);
        let key = derive_key(&raw).unwrap();
        assert_eq!(key, [b'x'; 32]);
    }

    #[test]
    fn derive_key_rejects_short_raw_key() {
        let err = derive_key("too-short").unwrap_err();
        assert!(err.to_string().contains("at least 32 bytes"));
    }

    #[test]
    fn derive_key_truncates_long_raw_key_to_32_bytes() {
        let raw = "y".repeat(40);
        let key = derive_key(&raw).unwrap();
        assert_eq!(key, [b'y'; 32]);
    }

    #[test]
    fn needs_refresh_true_when_within_5_minute_margin() {
        let now = chrono::Utc::now().timestamp_millis();
        let token = StoredToken {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: now + 60_000, // 1 minute out — inside the 5 min margin
        };
        assert!(token.needs_refresh());
    }

    #[test]
    fn needs_refresh_false_when_well_before_expiry() {
        let now = chrono::Utc::now().timestamp_millis();
        let token = StoredToken {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: now + 3_600_000, // 1 hour out
        };
        assert!(!token.needs_refresh());
    }

    #[test]
    fn needs_refresh_true_when_already_expired() {
        let now = chrono::Utc::now().timestamp_millis();
        let token = StoredToken {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: now - 1_000,
        };
        assert!(token.needs_refresh());
    }

    #[tokio::test]
    async fn save_then_load_roundtrips_the_token() {
        let store = TokenStore::new(temp_path("roundtrip"), KEY_A).unwrap();
        let token = StoredToken {
            access_token: "access-123".into(),
            refresh_token: "refresh-456".into(),
            expires_at: 1_700_000_000_000,
        };
        store.save(&token).await.unwrap();

        let loaded = store
            .load()
            .await
            .unwrap()
            .expect("token should be present");
        assert_eq!(loaded.access_token, token.access_token);
        assert_eq!(loaded.refresh_token, token.refresh_token);
        assert_eq!(loaded.expires_at, token.expires_at);

        let _ = std::fs::remove_file(store.path);
    }

    #[tokio::test]
    async fn load_returns_none_when_file_does_not_exist() {
        let store = TokenStore::new(temp_path("missing"), KEY_A).unwrap();
        assert!(store.load().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn load_fails_to_decrypt_with_wrong_key() {
        let path = temp_path("wrong-key");
        let writer = TokenStore::new(&path, KEY_A).unwrap();
        writer
            .save(&StoredToken {
                access_token: "a".into(),
                refresh_token: "r".into(),
                expires_at: 0,
            })
            .await
            .unwrap();

        // Fresh TokenStore instance (no in-memory cache) with a different key
        // reading the same file on disk.
        let reader = TokenStore::new(&path, KEY_B).unwrap();
        let err = reader.load().await.unwrap_err();
        assert!(err.to_string().contains("decryption failed"));

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn failed_save_does_not_populate_cache() {
        // The token path's "parent directory" is actually a file, so the disk
        // write inside save() must fail on every platform.
        let blocker = temp_path("save-fail-blocker");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let store = TokenStore::new(blocker.join("tokens.enc"), KEY_A).unwrap();

        let result = store
            .save(&StoredToken {
                access_token: "a".into(),
                refresh_token: "r".into(),
                expires_at: 0,
            })
            .await;
        assert!(result.is_err());
        // A token that never reached disk must not be served from memory —
        // that's the regression where auth_status reported "Authenticated"
        // after a failed save.
        assert!(store.cached.read().unwrap().is_none());

        let _ = std::fs::remove_file(&blocker);
    }

    #[tokio::test]
    async fn load_caches_after_first_read() {
        let path = temp_path("cache");
        let store = TokenStore::new(&path, KEY_A).unwrap();
        store
            .save(&StoredToken {
                access_token: "a".into(),
                refresh_token: "r".into(),
                expires_at: 0,
            })
            .await
            .unwrap();

        store.load().await.unwrap();
        // Remove the on-disk file — a correctly-caching load() should still
        // succeed because it doesn't need to hit disk again.
        std::fs::remove_file(&path).unwrap();
        assert!(store.load().await.unwrap().is_some());
    }
}
