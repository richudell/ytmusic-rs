use std::env;

/// Server configuration, loaded from environment variables.
///
/// Unlike the original TypeScript server, this does NOT require Spotify
/// credentials or a Postgres database — those powered features (adaptive
/// playlists / audio-feature recommendations) were dropped in the stripped
/// TS build this was ported from, and never existed here.
#[derive(Debug, Clone)]
pub struct Config {
    pub google_client_id: String,
    pub google_client_secret: String,
    /// Loopback redirect URI Google sends the auth code to. Must be
    /// registered in the Google Cloud Console OAuth client as an
    /// "http://127.0.0.1:<port>/oauth/callback" redirect URI.
    pub oauth_redirect_port: u16,
    /// AES-256-GCM key, base64-encoded (32 raw bytes). Required — unlike the
    /// TS original, there is no insecure hardcoded fallback key here.
    pub encryption_key_b64: String,
    /// Where encrypted OAuth tokens are persisted on disk.
    pub token_storage_path: String,
}

/// Collapse a set-but-empty (or whitespace-only) value to `None`.
///
/// `env::var` returns `Ok("")` for `FOO=`, which is exactly what a `.env` line
/// left blank produces once loaded — and `.env.example` ships
/// `TOKEN_STORAGE_PATH=` that way. Treating that as configured meant defaults
/// never applied and required-var checks passed on nothing: the server would
/// start with an empty token path, so OAuth consent appeared to succeed in the
/// browser while `auth_status` kept reporting "Not authenticated".
fn normalize(raw: Option<String>) -> Option<String> {
    raw.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn var(key: &str) -> Option<String> {
    normalize(env::var(key).ok())
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let google_client_id = var("GOOGLE_OAUTH_CLIENT_ID")
            .ok_or_else(|| anyhow::anyhow!("GOOGLE_OAUTH_CLIENT_ID is required"))?;
        let google_client_secret = var("GOOGLE_OAUTH_CLIENT_SECRET")
            .ok_or_else(|| anyhow::anyhow!("GOOGLE_OAUTH_CLIENT_SECRET is required"))?;
        let encryption_key_b64 = var("ENCRYPTION_KEY").ok_or_else(|| {
            anyhow::anyhow!(
                "ENCRYPTION_KEY is required (generate with: openssl rand -base64 32). \
                 There is no insecure fallback key in this build."
            )
        })?;
        let oauth_redirect_port = var("OAUTH_REDIRECT_PORT")
            .and_then(|v| v.parse().ok())
            .unwrap_or(8765);
        let token_storage_path = var("TOKEN_STORAGE_PATH").unwrap_or_else(|| {
            let home = var("HOME").unwrap_or_else(|| ".".to_string());
            format!("{home}/.config/ytmusic-rs/tokens.enc")
        });

        Ok(Self {
            google_client_id,
            google_client_secret,
            oauth_redirect_port,
            encryption_key_b64,
            token_storage_path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::normalize;

    // `normalize` is tested rather than `Config::load` because the latter reads
    // process-wide env state, which races against other tests in the same binary.

    #[test]
    fn unset_stays_none() {
        assert_eq!(normalize(None), None);
    }

    #[test]
    fn empty_string_is_treated_as_unset() {
        assert_eq!(normalize(Some(String::new())), None);
    }

    #[test]
    fn whitespace_only_is_treated_as_unset() {
        assert_eq!(normalize(Some("   \t ".to_string())), None);
    }

    #[test]
    fn real_value_is_kept_and_trimmed() {
        assert_eq!(
            normalize(Some("  /tmp/tokens.enc \n".to_string())),
            Some("/tmp/tokens.enc".to_string())
        );
    }

    #[test]
    fn base64_padding_survives_trimming() {
        // Trailing '=' padding must not be mistaken for whitespace.
        let key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
        assert_eq!(normalize(Some(key.to_string())), Some(key.to_string()));
    }
}
