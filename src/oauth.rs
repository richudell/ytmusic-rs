use crate::config::Config;
use crate::token_store::StoredToken;
use base64::Engine;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::timeout;

const AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
// Read/write access to YouTube account data (search + playlists).
const SCOPE: &str = "https://www.googleapis.com/auth/youtube";

fn random_url_safe(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn pkce_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let digest = hasher.finalize();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

pub struct AuthRequest {
    pub url: String,
    pub verifier: String,
    pub state: String,
    pub redirect_uri: String,
}

/// Build the Google consent-screen URL. `access_type=offline` +
/// `prompt=consent` guarantee a refresh_token comes back even on repeat
/// authorizations (Google otherwise only issues one on first consent).
pub fn build_auth_request(config: &Config) -> AuthRequest {
    let verifier = random_url_safe(64);
    let challenge = pkce_challenge(&verifier);
    let state = random_url_safe(24);
    let redirect_uri = format!(
        "http://127.0.0.1:{}/oauth/callback",
        config.oauth_redirect_port
    );

    let url = format!(
        "{AUTH_ENDPOINT}?client_id={client_id}&redirect_uri={redirect_uri}&response_type=code\
         &scope={scope}&code_challenge={challenge}&code_challenge_method=S256\
         &state={state}&access_type=offline&prompt=consent",
        client_id = urlencoding::encode(&config.google_client_id),
        redirect_uri = urlencoding::encode(&redirect_uri),
        scope = urlencoding::encode(SCOPE),
    );

    AuthRequest {
        url,
        verifier,
        state,
        redirect_uri,
    }
}

/// Start a one-shot local HTTP listener on the configured loopback port and
/// wait for Google to redirect the browser back to it with `?code=...`.
/// Times out after 3 minutes if nobody completes the consent flow.
pub async fn wait_for_callback(port: u16, expected_state: &str) -> anyhow::Result<String> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;

    let result = timeout(Duration::from_secs(180), async {
        loop {
            let (mut socket, _) = listener.accept().await?;
            let mut buf = [0u8; 8192];
            let n = socket.read(&mut buf).await?;
            let request = String::from_utf8_lossy(&buf[..n]);
            let first_line = request.lines().next().unwrap_or("");
            // e.g. "GET /oauth/callback?code=...&state=... HTTP/1.1"
            let path = first_line.split_whitespace().nth(1).unwrap_or("");

            let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
            let params: std::collections::HashMap<String, String> = query
                .split('&')
                .filter_map(|pair| {
                    let (k, v) = pair.split_once('=')?;
                    Some((k.to_string(), urlencoding::decode(v).ok()?.into_owned()))
                })
                .collect();

            let body = if params.get("state").map(|s| s.as_str()) == Some(expected_state) {
                "<html><body><h2>YouTube Music MCP: authenticated.</h2>\
                 You can close this tab and go back to your assistant.</body></html>"
            } else {
                "<html><body><h2>State mismatch — authentication rejected.</h2></body></html>"
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;

            if let Some(code) = params.get("code") {
                if params.get("state").map(|s| s.as_str()) == Some(expected_state) {
                    return Ok::<String, anyhow::Error>(code.clone());
                } else {
                    anyhow::bail!("OAuth state mismatch — possible CSRF, aborting");
                }
            }
            if let Some(err) = params.get("error") {
                anyhow::bail!("Google denied authorization: {err}");
            }
            // Otherwise (e.g. favicon request) keep listening for the real callback.
        }
    })
    .await;

    match result {
        Ok(inner) => inner,
        Err(_) => anyhow::bail!("Timed out waiting for OAuth callback (3 min)"),
    }
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: i64,
}

pub async fn exchange_code(
    config: &Config,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> anyhow::Result<StoredToken> {
    let client = reqwest::Client::new();
    let resp = client
        .post(TOKEN_ENDPOINT)
        .form(&[
            ("code", code),
            ("client_id", config.google_client_id.as_str()),
            ("client_secret", config.google_client_secret.as_str()),
            ("redirect_uri", redirect_uri),
            ("grant_type", "authorization_code"),
            ("code_verifier", verifier),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<TokenResponse>()
        .await?;

    let refresh_token = resp.refresh_token.ok_or_else(|| {
        anyhow::anyhow!(
            "Google did not return a refresh_token. This can happen if you've already \
             consented before without revoking access — revoke it at \
             https://myaccount.google.com/permissions and try again."
        )
    })?;

    Ok(StoredToken {
        access_token: resp.access_token,
        refresh_token,
        expires_at: chrono::Utc::now().timestamp_millis() + resp.expires_in * 1000,
    })
}

pub async fn refresh_access_token(
    config: &Config,
    refresh_token: &str,
) -> anyhow::Result<StoredToken> {
    let client = reqwest::Client::new();
    let resp = client
        .post(TOKEN_ENDPOINT)
        .form(&[
            ("refresh_token", refresh_token),
            ("client_id", config.google_client_id.as_str()),
            ("client_secret", config.google_client_secret.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<TokenResponse>()
        .await?;

    Ok(StoredToken {
        access_token: resp.access_token,
        // Google usually doesn't re-issue a refresh_token on refresh; keep the old one.
        refresh_token: resp
            .refresh_token
            .unwrap_or_else(|| refresh_token.to_string()),
        expires_at: chrono::Utc::now().timestamp_millis() + resp.expires_in * 1000,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        Config {
            google_client_id: "test-client-id".to_string(),
            google_client_secret: "test-secret".to_string(),
            oauth_redirect_port: 8765,
            encryption_key_b64: "unused-in-these-tests".to_string(),
            token_storage_path: "/tmp/unused".to_string(),
        }
    }

    #[test]
    fn random_url_safe_produces_requested_length_and_charset() {
        let s = random_url_safe(64);
        // base64 URL-safe, no padding: 64 raw bytes -> ceil(64*4/3) = 86 chars.
        assert_eq!(s.len(), 86);
        assert!(s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn random_url_safe_is_actually_random() {
        let a = random_url_safe(24);
        let b = random_url_safe(24);
        assert_ne!(a, b, "two consecutive calls should not collide");
    }

    #[test]
    fn pkce_challenge_is_deterministic_sha256_base64url() {
        let verifier = "fixed-test-verifier";
        let challenge_a = pkce_challenge(verifier);
        let challenge_b = pkce_challenge(verifier);
        assert_eq!(challenge_a, challenge_b);

        // Cross-check against a manual SHA-256 + base64url-nopad computation
        // so this doesn't just test "the function agrees with itself".
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize());
        assert_eq!(challenge_a, expected);
    }

    #[test]
    fn pkce_challenge_differs_for_different_verifiers() {
        assert_ne!(
            pkce_challenge("verifier-one"),
            pkce_challenge("verifier-two")
        );
    }

    #[test]
    fn build_auth_request_url_is_well_formed() {
        let config = test_config();
        let req = build_auth_request(&config);

        assert_eq!(req.redirect_uri, "http://127.0.0.1:8765/oauth/callback");
        assert!(req.url.starts_with(AUTH_ENDPOINT));
        assert!(req.url.contains("client_id=test-client-id"));
        assert!(req.url.contains(&format!(
            "redirect_uri={}",
            urlencoding::encode(&req.redirect_uri)
        )));
        assert!(req.url.contains("response_type=code"));
        assert!(req.url.contains("code_challenge_method=S256"));
        assert!(req.url.contains(&format!("state={}", req.state)));
        assert!(req.url.contains("access_type=offline"));
        assert!(req.url.contains("prompt=consent"));

        // The URL's code_challenge must match PKCE-deriving the returned verifier.
        let expected_challenge = pkce_challenge(&req.verifier);
        assert!(req
            .url
            .contains(&format!("code_challenge={expected_challenge}")));
    }

    #[test]
    fn build_auth_request_generates_fresh_state_and_verifier_each_call() {
        let config = test_config();
        let a = build_auth_request(&config);
        let b = build_auth_request(&config);
        assert_ne!(a.state, b.state);
        assert_ne!(a.verifier, b.verifier);
    }
}
