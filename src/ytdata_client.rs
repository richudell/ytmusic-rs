//! YouTube Data API v3 client — official, documented API (unlike the
//! InnerTube client). Used for everything that touches the user's account:
//! playlists and liked videos. All calls require a valid OAuth bearer token.

use serde::Serialize;
use serde_json::Value;

const BASE_URL: &str = "https://www.googleapis.com/youtube/v3";

pub struct YouTubeDataClient {
    http: reqwest::Client,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct PlaylistSummary {
    pub id: String,
    pub title: String,
    pub description: String,
    pub privacy: String,
    pub item_count: i64,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct PlaylistItemSummary {
    /// Playlist-item ID — needed for `remove_from_playlist`, distinct from `video_id`.
    pub playlist_item_id: String,
    pub video_id: String,
    pub title: String,
    pub channel_title: String,
    pub position: i64,
}

impl YouTubeDataClient {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("failed to build reqwest client"),
        }
    }

    pub async fn list_playlists(
        &self,
        token: &str,
        limit: u32,
    ) -> anyhow::Result<Vec<PlaylistSummary>> {
        let resp: Value = self
            .http
            .get(format!("{BASE_URL}/playlists"))
            .bearer_auth(token)
            .query(&[
                ("part", "snippet,status,contentDetails"),
                ("mine", "true"),
                ("maxResults", &limit.min(50).to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let items = resp["items"].as_array().cloned().unwrap_or_default();
        Ok(items
            .into_iter()
            .map(|it| PlaylistSummary {
                id: it["id"].as_str().unwrap_or_default().to_string(),
                title: it["snippet"]["title"].as_str().unwrap_or_default().to_string(),
                description: it["snippet"]["description"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                privacy: it["status"]["privacyStatus"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                item_count: it["contentDetails"]["itemCount"].as_i64().unwrap_or(0),
            })
            .collect())
    }

    pub async fn list_playlist_items(
        &self,
        token: &str,
        playlist_id: &str,
        limit: u32,
    ) -> anyhow::Result<Vec<PlaylistItemSummary>> {
        let resp: Value = self
            .http
            .get(format!("{BASE_URL}/playlistItems"))
            .bearer_auth(token)
            .query(&[
                ("part", "snippet"),
                ("playlistId", playlist_id),
                ("maxResults", &limit.min(50).to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let items = resp["items"].as_array().cloned().unwrap_or_default();
        Ok(items
            .into_iter()
            .map(|it| PlaylistItemSummary {
                playlist_item_id: it["id"].as_str().unwrap_or_default().to_string(),
                video_id: it["snippet"]["resourceId"]["videoId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                title: it["snippet"]["title"].as_str().unwrap_or_default().to_string(),
                channel_title: it["snippet"]["videoOwnerChannelTitle"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                position: it["snippet"]["position"].as_i64().unwrap_or(0),
            })
            .collect())
    }

    pub async fn create_playlist(
        &self,
        token: &str,
        title: &str,
        description: &str,
        privacy: &str,
    ) -> anyhow::Result<String> {
        let body = serde_json::json!({
            "snippet": { "title": title, "description": description },
            "status": { "privacyStatus": privacy },
        });
        let resp: Value = self
            .http
            .post(format!("{BASE_URL}/playlists"))
            .bearer_auth(token)
            .query(&[("part", "snippet,status")])
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp["id"].as_str().unwrap_or_default().to_string())
    }

    pub async fn update_playlist(
        &self,
        token: &str,
        playlist_id: &str,
        title: Option<&str>,
        description: Option<&str>,
        privacy: Option<&str>,
    ) -> anyhow::Result<()> {
        // YouTube's playlists.update requires the full snippet/status back,
        // so fetch current values first and merge in only the changed fields.
        let current: Value = self
            .http
            .get(format!("{BASE_URL}/playlists"))
            .bearer_auth(token)
            .query(&[("part", "snippet,status"), ("id", playlist_id)])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let existing = current["items"]
            .as_array()
            .and_then(|a| a.first())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("playlist {playlist_id} not found"))?;

        let mut snippet = existing["snippet"].clone();
        if let Some(t) = title {
            snippet["title"] = serde_json::Value::String(t.to_string());
        }
        if let Some(d) = description {
            snippet["description"] = serde_json::Value::String(d.to_string());
        }
        let mut status = existing["status"].clone();
        if let Some(p) = privacy {
            status["privacyStatus"] = serde_json::Value::String(p.to_string());
        }

        let body = serde_json::json!({ "id": playlist_id, "snippet": snippet, "status": status });
        self.http
            .put(format!("{BASE_URL}/playlists"))
            .bearer_auth(token)
            .query(&[("part", "snippet,status")])
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub async fn delete_playlist(&self, token: &str, playlist_id: &str) -> anyhow::Result<()> {
        self.http
            .delete(format!("{BASE_URL}/playlists"))
            .bearer_auth(token)
            .query(&[("id", playlist_id)])
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub async fn add_to_playlist(
        &self,
        token: &str,
        playlist_id: &str,
        video_ids: &[String],
    ) -> anyhow::Result<()> {
        for video_id in video_ids {
            let body = serde_json::json!({
                "snippet": {
                    "playlistId": playlist_id,
                    "resourceId": { "kind": "youtube#video", "videoId": video_id },
                }
            });
            self.http
                .post(format!("{BASE_URL}/playlistItems"))
                .bearer_auth(token)
                .query(&[("part", "snippet")])
                .json(&body)
                .send()
                .await?
                .error_for_status()?;
        }
        Ok(())
    }

    pub async fn remove_from_playlist(
        &self,
        token: &str,
        playlist_item_ids: &[String],
    ) -> anyhow::Result<()> {
        for id in playlist_item_ids {
            self.http
                .delete(format!("{BASE_URL}/playlistItems"))
                .bearer_auth(token)
                .query(&[("id", id.as_str())])
                .send()
                .await?
                .error_for_status()?;
        }
        Ok(())
    }

    pub async fn list_liked_videos(
        &self,
        token: &str,
        limit: u32,
    ) -> anyhow::Result<Vec<PlaylistItemSummary>> {
        // The "liked videos" auto-playlist has the well-known ID "LL".
        self.list_playlist_items(token, "LL", limit).await
    }
}
