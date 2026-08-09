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

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let google_client_id = env::var("GOOGLE_OAUTH_CLIENT_ID")
            .map_err(|_| anyhow::anyhow!("GOOGLE_OAUTH_CLIENT_ID is required"))?;
        let google_client_secret = env::var("GOOGLE_OAUTH_CLIENT_SECRET")
            .map_err(|_| anyhow::anyhow!("GOOGLE_OAUTH_CLIENT_SECRET is required"))?;
        let encryption_key_b64 = env::var("ENCRYPTION_KEY").map_err(|_| {
            anyhow::anyhow!(
                "ENCRYPTION_KEY is required (generate with: openssl rand -base64 32). \
                 There is no insecure fallback key in this build."
            )
        })?;
        let oauth_redirect_port = env::var("OAUTH_REDIRECT_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8765);
        let token_storage_path = env::var("TOKEN_STORAGE_PATH").unwrap_or_else(|_| {
            let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
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
