//! Minimal client for YouTube Music's undocumented "InnerTube" web API.
//!
//! Ported from `youtube-music-mcp-server`'s `src/youtube-music/client.ts`,
//! same public `WEB_REMIX` client key (not a secret — the same constant
//! `ytmusicapi` and every other unofficial client uses to identify itself
//! to Google's InnerTube backend; it does not grant access to anyone's
//! account).
//!
//! Scope note: the original TS project has ~600 lines of response parsers
//! (`youtube-music/parsers.ts`) built up over many ytmusicapi-derived
//! renderer-format edge cases. This port implements one generic recursive
//! extractor instead of type-filtered `search_songs`/`search_albums`/
//! `search_artists` — InnerTube's per-type search "params" values are
//! opaque encoded protobufs I did not want to guess at without a way to
//! verify them, so getting the type wrong would fail silently rather than
//! erroring. `search()` returns a best-effort `kind` per result instead;
//! it's an honest v0.1 tradeoff, not full parity.

use chrono::Datelike;
use serde::Serialize;
use serde_json::Value;

const YTM_BASE_URL: &str = "https://music.youtube.com";
const YTM_API_URL: &str = "https://music.youtube.com/youtubei/v1";
// Public InnerTube API key for the WEB_REMIX client — see module docs.
const YTM_API_KEY: &str = "AIzaSyC9XL3ZjWddXya6X74dJoCTL-WEYFDNX30";

fn client_version() -> String {
    let now = chrono::Utc::now();
    format!("1.{:04}{:02}{:02}.01.00", now.year(), now.month(), now.day())
}

fn context() -> Value {
    serde_json::json!({
        "client": {
            "clientName": "WEB_REMIX",
            "clientVersion": client_version(),
        },
        "user": {}
    })
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct SearchResult {
    pub kind: String, // "song_or_video" | "album" | "artist" | "unknown"
    pub title: String,
    pub subtitle: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browse_id: Option<String>,
}

pub struct YouTubeMusicClient {
    http: reqwest::Client,
}

impl YouTubeMusicClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:88.0) Gecko/20100101 Firefox/88.0")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build reqwest client");
        Self { http }
    }

    async fn post(&self, endpoint: &str, mut body: Value) -> anyhow::Result<Value> {
        body["context"] = context();
        let url = format!("{YTM_API_URL}/{endpoint}");
        let resp = self
            .http
            .post(&url)
            .query(&[("key", YTM_API_KEY), ("prettyPrint", "false")])
            .header("Origin", YTM_BASE_URL)
            .header("Referer", format!("{YTM_BASE_URL}/"))
            .header("X-Youtube-Client-Name", "67")
            .header("X-Youtube-Client-Version", client_version())
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        Ok(resp)
    }

    pub async fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<SearchResult>> {
        let resp = self
            .post("search", serde_json::json!({ "query": query }))
            .await?;
        let mut results = Vec::new();
        collect_list_items(&resp, &mut results);
        results.truncate(limit.max(1));
        Ok(results)
    }

    pub async fn get_song(&self, video_id: &str) -> anyhow::Result<Value> {
        self.post("player", serde_json::json!({ "videoId": video_id }))
            .await
            .and_then(|v| extract_song_details(&v, video_id))
    }

    pub async fn get_browse(&self, browse_id: &str) -> anyhow::Result<Value> {
        self.post("browse", serde_json::json!({ "browseId": browse_id }))
            .await
    }
}

/// Recursively walk the InnerTube response looking for
/// `musicResponsiveListItemRenderer` nodes (the row-item renderer used
/// across search/library/playlist views) and turn each into a `SearchResult`.
fn collect_list_items(value: &Value, out: &mut Vec<SearchResult>) {
    match value {
        Value::Object(map) => {
            if let Some(renderer) = map.get("musicResponsiveListItemRenderer") {
                if let Some(item) = parse_list_item(renderer) {
                    out.push(item);
                }
            }
            for v in map.values() {
                collect_list_items(v, out);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                collect_list_items(v, out);
            }
        }
        _ => {}
    }
}

fn text_runs_joined(node: &Value) -> Option<String> {
    let runs = node.get("text")?.get("runs")?.as_array()?;
    let joined: String = runs
        .iter()
        .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
        .collect();
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

fn parse_list_item(renderer: &Value) -> Option<SearchResult> {
    let columns = renderer.get("flexColumns")?.as_array()?;
    let title = columns
        .first()
        .and_then(|c| c.get("musicResponsiveListItemFlexColumnRenderer"))
        .and_then(text_runs_joined)?;
    let subtitle = columns
        .get(1)
        .and_then(|c| c.get("musicResponsiveListItemFlexColumnRenderer"))
        .and_then(text_runs_joined)
        .unwrap_or_default();

    let video_id = renderer
        .pointer("/playlistItemData/videoId")
        .or_else(|| {
            renderer.pointer(
                "/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/watchEndpoint/videoId",
            )
        })
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let browse_id = renderer
        .pointer("/navigationEndpoint/browseEndpoint/browseId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let kind = if video_id.is_some() {
        "song_or_video"
    } else if browse_id.as_deref().is_some_and(|b| b.starts_with("MPRE")) {
        "album"
    } else if browse_id.as_deref().is_some_and(|b| b.starts_with("UC")) {
        "artist"
    } else {
        "unknown"
    };

    Some(SearchResult {
        kind: kind.to_string(),
        title,
        subtitle,
        video_id,
        browse_id,
    })
}

fn extract_song_details(resp: &Value, video_id: &str) -> anyhow::Result<Value> {
    let details = resp.get("videoDetails").cloned().unwrap_or(Value::Null);
    Ok(serde_json::json!({
        "videoId": video_id,
        "title": details.get("title"),
        "author": details.get("author"),
        "lengthSeconds": details.get("lengthSeconds"),
        "shortDescription": details.get("shortDescription"),
        "viewCount": details.get("viewCount"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_runs(text: &str) -> Value {
        serde_json::json!({ "text": { "runs": [ { "text": text } ] } })
    }

    fn song_renderer(title: &str, subtitle: &str, video_id: &str) -> Value {
        serde_json::json!({
            "musicResponsiveListItemRenderer": {
                "flexColumns": [
                    { "musicResponsiveListItemFlexColumnRenderer": text_runs(title) },
                    { "musicResponsiveListItemFlexColumnRenderer": text_runs(subtitle) },
                ],
                "overlay": {
                    "musicItemThumbnailOverlayRenderer": {
                        "content": {
                            "musicPlayButtonRenderer": {
                                "playNavigationEndpoint": {
                                    "watchEndpoint": { "videoId": video_id }
                                }
                            }
                        }
                    }
                }
            }
        })
    }

    fn browse_renderer(title: &str, subtitle: &str, browse_id: &str) -> Value {
        serde_json::json!({
            "musicResponsiveListItemRenderer": {
                "flexColumns": [
                    { "musicResponsiveListItemFlexColumnRenderer": text_runs(title) },
                    { "musicResponsiveListItemFlexColumnRenderer": text_runs(subtitle) },
                ],
                "navigationEndpoint": { "browseEndpoint": { "browseId": browse_id } }
            }
        })
    }

    #[test]
    fn client_version_matches_expected_format() {
        let v = client_version();
        // e.g. "1.20260808.01.00"
        let re_ok = v.starts_with("1.")
            && v.ends_with(".01.00")
            && v.trim_start_matches("1.")
                .trim_end_matches(".01.00")
                .len()
                == 8;
        assert!(re_ok, "unexpected client_version format: {v}");
    }

    #[test]
    fn text_runs_joined_concatenates_multiple_runs() {
        let node = serde_json::json!({
            "text": { "runs": [ { "text": "Sweet " }, { "text": "Child " }, { "text": "O' Mine" } ] }
        });
        assert_eq!(text_runs_joined(&node), Some("Sweet Child O' Mine".to_string()));
    }

    #[test]
    fn text_runs_joined_returns_none_when_missing() {
        assert_eq!(text_runs_joined(&serde_json::json!({})), None);
    }

    #[test]
    fn text_runs_joined_returns_none_when_empty() {
        let node = serde_json::json!({ "text": { "runs": [] } });
        assert_eq!(text_runs_joined(&node), None);
    }

    #[test]
    fn parse_list_item_extracts_song_with_video_id() {
        let renderer = song_renderer("November Rain", "Guns N' Roses", "8SbUC-UaAxE")
            ["musicResponsiveListItemRenderer"]
            .clone();
        let item = parse_list_item(&renderer).expect("should parse");
        assert_eq!(item.kind, "song_or_video");
        assert_eq!(item.title, "November Rain");
        assert_eq!(item.subtitle, "Guns N' Roses");
        assert_eq!(item.video_id.as_deref(), Some("8SbUC-UaAxE"));
        assert_eq!(item.browse_id, None);
    }

    #[test]
    fn parse_list_item_classifies_album_browse_id() {
        let renderer = browse_renderer("Appetite for Destruction", "Guns N' Roses", "MPREb_abc123")
            ["musicResponsiveListItemRenderer"]
            .clone();
        let item = parse_list_item(&renderer).expect("should parse");
        assert_eq!(item.kind, "album");
        assert_eq!(item.browse_id.as_deref(), Some("MPREb_abc123"));
        assert_eq!(item.video_id, None);
    }

    #[test]
    fn parse_list_item_classifies_artist_browse_id() {
        let renderer = browse_renderer("Guns N' Roses", "Artist", "UC1234567890abcdef")
            ["musicResponsiveListItemRenderer"]
            .clone();
        let item = parse_list_item(&renderer).expect("should parse");
        assert_eq!(item.kind, "artist");
    }

    #[test]
    fn parse_list_item_falls_back_to_unknown_kind() {
        let renderer = browse_renderer("Mystery", "???", "SOMETHING_ELSE")
            ["musicResponsiveListItemRenderer"]
            .clone();
        let item = parse_list_item(&renderer).expect("should parse");
        assert_eq!(item.kind, "unknown");
    }

    #[test]
    fn parse_list_item_returns_none_without_flex_columns() {
        let renderer = serde_json::json!({});
        assert!(parse_list_item(&renderer).is_none());
    }

    #[test]
    fn parse_list_item_defaults_subtitle_when_second_column_missing() {
        let renderer = serde_json::json!({
            "flexColumns": [
                { "musicResponsiveListItemFlexColumnRenderer": text_runs("Only Title") }
            ]
        });
        let item = parse_list_item(&renderer).expect("should parse");
        assert_eq!(item.title, "Only Title");
        assert_eq!(item.subtitle, "");
    }

    #[test]
    fn collect_list_items_walks_nested_structure_and_flattens_results() {
        let response = serde_json::json!({
            "contents": {
                "sectionListRenderer": {
                    "contents": [
                        { "musicShelfRenderer": {
                            "contents": [
                                song_renderer("Track One", "Artist A", "vid1"),
                                song_renderer("Track Two", "Artist B", "vid2"),
                            ]
                        }},
                        browse_renderer("Some Album", "Artist A", "MPREb_xyz"),
                    ]
                }
            }
        });

        let mut out = Vec::new();
        collect_list_items(&response, &mut out);

        assert_eq!(out.len(), 3);
        assert_eq!(out[0].title, "Track One");
        assert_eq!(out[1].title, "Track Two");
        assert_eq!(out[2].kind, "album");
    }

    #[test]
    fn collect_list_items_on_empty_response_yields_nothing() {
        let mut out = Vec::new();
        collect_list_items(&serde_json::json!({}), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn extract_song_details_maps_known_fields() {
        let resp = serde_json::json!({
            "videoDetails": {
                "title": "Sweet Child O' Mine",
                "author": "Guns N' Roses",
                "lengthSeconds": "356",
                "shortDescription": "official audio",
                "viewCount": "123456789",
            }
        });
        let details = extract_song_details(&resp, "1w7OgIMMRc4").unwrap();
        assert_eq!(details["videoId"], "1w7OgIMMRc4");
        assert_eq!(details["title"], "Sweet Child O' Mine");
        assert_eq!(details["author"], "Guns N' Roses");
        assert_eq!(details["lengthSeconds"], "356");
        assert_eq!(details["viewCount"], "123456789");
    }

    #[test]
    fn extract_song_details_handles_missing_video_details() {
        let details = extract_song_details(&serde_json::json!({}), "abc").unwrap();
        assert_eq!(details["videoId"], "abc");
        assert!(details["title"].is_null());
    }
}
