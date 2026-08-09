use crate::config::Config;
use crate::oauth;
use crate::token_store::{StoredToken, TokenStore, REFRESH_MARGIN_MS};
use crate::ytdata_client::YouTubeDataClient;
use crate::ytmusic_client::YouTubeMusicClient;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

fn ok_json<T: serde::Serialize>(value: &T) -> Result<CallToolResult, McpError> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
    Ok(CallToolResult::success(vec![Content::text(text)]))
}

fn ok_text(text: impl Into<String>) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![Content::text(text.into())]))
}

fn to_mcp_err(e: anyhow::Error) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

/// Describe a stored token in the same terms `valid_access_token` acts on.
///
/// Reporting a bare "Authenticated" with a `.max(0)` countdown meant an
/// expired token displayed as "expires in 0 seconds" — indistinguishable
/// from a healthy one about to lapse, and silent about the fact that the
/// next call would refresh it. Takes `now` so it stays pure and testable.
fn describe_auth_state(expires_at: i64, now: i64) -> String {
    let remaining_secs = (expires_at - now) / 1000;
    if remaining_secs <= 0 {
        format!(
            "Authenticated, but the access token expired {} seconds ago. It will be \
             refreshed automatically on the next authenticated call.",
            -remaining_secs
        )
    } else if expires_at - REFRESH_MARGIN_MS < now {
        format!(
            "Authenticated. Access token expires in {remaining_secs} seconds, which is \
             inside the refresh margin — it will be refreshed on the next authenticated call."
        )
    } else {
        format!("Authenticated. Access token valid for another {remaining_secs} seconds.")
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Free-text search query, e.g. "Sweet Child O' Mine Guns N' Roses"
    pub query: String,
    /// Max results to return (default 10, max 50)
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct VideoIdParams {
    pub video_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowseIdParams {
    pub browse_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LimitParams {
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PlaylistIdParams {
    pub playlist_id: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreatePlaylistParams {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    /// "private" (default), "public", or "unlisted"
    #[serde(default)]
    pub privacy: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdatePlaylistParams {
    pub playlist_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub privacy: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeletePlaylistParams {
    pub playlist_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddToPlaylistParams {
    pub playlist_id: String,
    /// YouTube video IDs to add
    pub video_ids: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RemoveFromPlaylistParams {
    /// Playlist-ITEM IDs (from get_playlist_items), not video IDs
    pub playlist_item_ids: Vec<String>,
}

#[derive(Clone)]
pub struct YtMusicServer {
    config: Arc<Config>,
    token_store: Arc<TokenStore>,
    yt_music: Arc<YouTubeMusicClient>,
    yt_data: Arc<YouTubeDataClient>,
    #[allow(dead_code)] // read by the #[tool_handler] macro's generated dispatch, not directly
    tool_router: ToolRouter<YtMusicServer>,
}

impl YtMusicServer {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        let token_store = TokenStore::new(&config.token_storage_path, &config.encryption_key_b64)?;
        Ok(Self {
            config: Arc::new(config),
            token_store: Arc::new(token_store),
            yt_music: Arc::new(YouTubeMusicClient::new()),
            yt_data: Arc::new(YouTubeDataClient::new()),
            tool_router: Self::tool_router(),
        })
    }

    /// Load the stored token, transparently refreshing it if it's expiring
    /// soon. Returns a clear error (not a panic/crash) if the user hasn't
    /// run `authenticate` yet.
    async fn valid_access_token(&self) -> anyhow::Result<String> {
        let stored = self
            .token_store
            .load()
            .await?
            .ok_or_else(|| anyhow::anyhow!("Not authenticated yet — call the `authenticate` tool first."))?;

        if stored.needs_refresh() {
            let refreshed = oauth::refresh_access_token(&self.config, &stored.refresh_token).await?;
            self.token_store.save(&refreshed).await?;
            Ok(refreshed.access_token)
        } else {
            Ok(stored.access_token)
        }
    }
}

#[tool_router]
impl YtMusicServer {
    #[tool(
        description = "Start Google OAuth login: returns a URL to open in a browser immediately, then keeps listening in the background (up to 3 minutes) for you to approve access and stores an encrypted token when you do. Call `auth_status` afterward to confirm it completed. Run this once before any playlist/library tool."
    )]
    async fn authenticate(&self) -> Result<CallToolResult, McpError> {
        let req = oauth::build_auth_request(&self.config);
        let url = req.url.clone();

        // Bug fix: this used to `.await` wait_for_callback + exchange_code
        // right here, so the tool call itself blocked for up to 3 minutes
        // and the URL was only ever visible via `tracing::info!` in server
        // logs — never in the actual returned CallToolResult a client sees.
        // Spawn the wait in the background instead so the URL comes back in
        // the tool response right away.
        let config = self.config.clone();
        let token_store = self.token_store.clone();
        tokio::spawn(async move {
            let outcome: anyhow::Result<()> = async {
                let code =
                    oauth::wait_for_callback(config.oauth_redirect_port, &req.state).await?;
                let token =
                    oauth::exchange_code(&config, &code, &req.verifier, &req.redirect_uri)
                        .await?;
                token_store.save(&token).await
            }
            .await;

            match outcome {
                Ok(()) => tracing::info!("OAuth flow completed; token stored."),
                Err(e) => tracing::error!("OAuth flow failed: {e}"),
            }
        });

        ok_text(format!(
            "Open this URL in your browser to grant access:\n{url}\n\n\
             Listening in the background for up to 3 minutes. Call `auth_status` \
             once you've approved it to confirm the token was stored."
        ))
    }

    #[tool(description = "Check whether we currently hold a stored, usable OAuth token")]
    async fn auth_status(&self) -> Result<CallToolResult, McpError> {
        match self.token_store.load().await.map_err(to_mcp_err)? {
            Some(StoredToken { expires_at, .. }) => {
                let now = chrono::Utc::now().timestamp_millis();
                ok_text(describe_auth_state(expires_at, now))
            }
            None => ok_text("Not authenticated. Call the `authenticate` tool."),
        }
    }

    #[tool(
        description = "Search YouTube Music (songs, videos, albums, artists all mixed together — see result `kind` field). No auth required."
    )]
    async fn search(
        &self,
        Parameters(p): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let limit = p.limit.unwrap_or(10).min(50) as usize;
        let results = self
            .yt_music
            .search(&p.query, limit)
            .await
            .map_err(to_mcp_err)?;
        ok_json(&results)
    }

    #[tool(description = "Get details for a song/video by its YouTube video ID. No auth required.")]
    async fn get_song_info(
        &self,
        Parameters(p): Parameters<VideoIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let details = self
            .yt_music
            .get_song(&p.video_id)
            .await
            .map_err(to_mcp_err)?;
        ok_json(&details)
    }

    #[tool(
        description = "Get raw browse data for an album or artist by browse ID (from search results). No auth required. Returns YouTube's raw InnerTube browse response — this port does not re-implement the TS project's full album/artist renderer parsing, so expect to read the JSON structure yourself."
    )]
    async fn get_browse_info(
        &self,
        Parameters(p): Parameters<BrowseIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let details = self
            .yt_music
            .get_browse(&p.browse_id)
            .await
            .map_err(to_mcp_err)?;
        ok_json(&details)
    }

    #[tool(description = "List your liked videos (\"Liked Music\" playlist). Requires authenticate first.")]
    async fn get_liked_videos(
        &self,
        Parameters(p): Parameters<LimitParams>,
    ) -> Result<CallToolResult, McpError> {
        let token = self.valid_access_token().await.map_err(to_mcp_err)?;
        let items = self
            .yt_data
            .list_liked_videos(&token, p.limit.unwrap_or(25))
            .await
            .map_err(to_mcp_err)?;
        ok_json(&items)
    }

    #[tool(description = "List your YouTube playlists. Requires authenticate first.")]
    async fn get_playlists(
        &self,
        Parameters(p): Parameters<LimitParams>,
    ) -> Result<CallToolResult, McpError> {
        let token = self.valid_access_token().await.map_err(to_mcp_err)?;
        let playlists = self
            .yt_data
            .list_playlists(&token, p.limit.unwrap_or(25))
            .await
            .map_err(to_mcp_err)?;
        ok_json(&playlists)
    }

    #[tool(description = "List the tracks in a playlist by playlist ID. Requires authenticate first.")]
    async fn get_playlist_items(
        &self,
        Parameters(p): Parameters<PlaylistIdParams>,
    ) -> Result<CallToolResult, McpError> {
        let token = self.valid_access_token().await.map_err(to_mcp_err)?;
        let items = self
            .yt_data
            .list_playlist_items(&token, &p.playlist_id, p.limit.unwrap_or(50))
            .await
            .map_err(to_mcp_err)?;
        ok_json(&items)
    }

    #[tool(description = "Create a new YouTube playlist. Requires authenticate first.")]
    async fn create_playlist(
        &self,
        Parameters(p): Parameters<CreatePlaylistParams>,
    ) -> Result<CallToolResult, McpError> {
        let token = self.valid_access_token().await.map_err(to_mcp_err)?;
        let id = self
            .yt_data
            .create_playlist(
                &token,
                &p.title,
                p.description.as_deref().unwrap_or(""),
                p.privacy.as_deref().unwrap_or("private"),
            )
            .await
            .map_err(to_mcp_err)?;
        ok_text(format!("Created playlist: {id}"))
    }

    #[tool(description = "Update a playlist's title/description/privacy. Requires authenticate first.")]
    async fn update_playlist(
        &self,
        Parameters(p): Parameters<UpdatePlaylistParams>,
    ) -> Result<CallToolResult, McpError> {
        if p.title.is_none() && p.description.is_none() && p.privacy.is_none() {
            return Err(McpError::invalid_params(
                "At least one of title, description, or privacy must be provided",
                None,
            ));
        }
        let token = self.valid_access_token().await.map_err(to_mcp_err)?;
        self.yt_data
            .update_playlist(
                &token,
                &p.playlist_id,
                p.title.as_deref(),
                p.description.as_deref(),
                p.privacy.as_deref(),
            )
            .await
            .map_err(to_mcp_err)?;
        ok_text("Playlist updated.")
    }

    #[tool(description = "Delete a playlist by ID. Requires authenticate first.")]
    async fn delete_playlist(
        &self,
        Parameters(p): Parameters<DeletePlaylistParams>,
    ) -> Result<CallToolResult, McpError> {
        let token = self.valid_access_token().await.map_err(to_mcp_err)?;
        self.yt_data
            .delete_playlist(&token, &p.playlist_id)
            .await
            .map_err(to_mcp_err)?;
        ok_text("Playlist deleted.")
    }

    #[tool(description = "Add one or more videos to a playlist by video ID. Requires authenticate first.")]
    async fn add_to_playlist(
        &self,
        Parameters(p): Parameters<AddToPlaylistParams>,
    ) -> Result<CallToolResult, McpError> {
        let token = self.valid_access_token().await.map_err(to_mcp_err)?;
        self.yt_data
            .add_to_playlist(&token, &p.playlist_id, &p.video_ids)
            .await
            .map_err(to_mcp_err)?;
        ok_text(format!("Added {} track(s).", p.video_ids.len()))
    }

    #[tool(
        description = "Remove tracks from a playlist. Takes playlist-ITEM IDs (get them from get_playlist_items), not video IDs. Requires authenticate first."
    )]
    async fn remove_from_playlist(
        &self,
        Parameters(p): Parameters<RemoveFromPlaylistParams>,
    ) -> Result<CallToolResult, McpError> {
        let token = self.valid_access_token().await.map_err(to_mcp_err)?;
        self.yt_data
            .remove_from_playlist(&token, &p.playlist_item_ids)
            .await
            .map_err(to_mcp_err)?;
        ok_text(format!("Removed {} track(s).", p.playlist_item_ids.len()))
    }
}

#[tool_handler]
impl ServerHandler for YtMusicServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder().enable_tools().build(),
        )
        .with_server_info(Implementation::from_build_env())
        .with_protocol_version(ProtocolVersion::V_2024_11_05)
        .with_instructions(
            "YouTube Music MCP server (Rust port, core search/playlist tools only — \
             no Spotify/MusicBrainz/adaptive-playlist recommendation engine, no database). \
             Call `authenticate` once before any playlist/library tool."
                .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{describe_auth_state, REFRESH_MARGIN_MS};

    // `now` is passed explicitly so these don't depend on wall-clock timing.
    const NOW: i64 = 1_700_000_000_000;

    #[test]
    fn healthy_token_reports_remaining_lifetime() {
        let s = describe_auth_state(NOW + 3_600_000, NOW);
        assert!(s.contains("valid for another 3600 seconds"), "{s}");
        assert!(!s.contains("refresh"), "{s}");
    }

    #[test]
    fn token_inside_refresh_margin_says_it_will_refresh() {
        let s = describe_auth_state(NOW + 60_000, NOW);
        assert!(s.contains("expires in 60 seconds"), "{s}");
        assert!(s.contains("inside the refresh margin"), "{s}");
    }

    #[test]
    fn expired_token_is_not_reported_as_simply_authenticated() {
        let s = describe_auth_state(NOW - 120_000, NOW);
        assert!(s.contains("expired 120 seconds ago"), "{s}");
        assert!(s.contains("refreshed automatically"), "{s}");
        // The old formatting collapsed every expired token to this.
        assert!(!s.contains("expires in 0 seconds"), "{s}");
    }

    #[test]
    fn exactly_expired_counts_as_expired_not_valid() {
        let s = describe_auth_state(NOW, NOW);
        assert!(s.contains("expired 0 seconds ago"), "{s}");
    }

    #[test]
    fn boundary_just_outside_margin_is_reported_healthy() {
        let s = describe_auth_state(NOW + REFRESH_MARGIN_MS + 1_000, NOW);
        assert!(s.contains("valid for another"), "{s}");
    }
}
