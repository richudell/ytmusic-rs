# ytmusic-rs

Rust port by Rich Udell (2026), licensed MIT — see [`LICENSE`](LICENSE).

A Rust rewrite of the core (search + playlist) tools from
[`CaullenOmdahl/youtube-music-mcp-server`](https://github.com/CaullenOmdahl/youtube-music-mcp-server),
built on the official Rust MCP SDK ([`rmcp`](https://crates.io/crates/rmcp),
pinned to `1.4.0`) instead of TypeScript/`@modelcontextprotocol/sdk`.

## What's different from the TS original

**Dropped entirely** — not ported, not planned:
- Adaptive playlist / recommendation engine (MusicBrainz, ListenBrainz,
  Spotify audio-features, ReccoBeats)
- PostgreSQL persistence
- Remote-server OAuth 2.1 + Streamable HTTP / SSE transport (Smithery/Railway
  deployment target)

This is a local **stdio** MCP server instead — the same transport used by
most other MCP servers you'd add to an agent's `mcp.servers` config (no HTTP
port, no bearer-auth reverse proxy, no session management layer).

**Fixed vs. the TS original:** the token store has no insecure hardcoded
fallback encryption key. `ENCRYPTION_KEY` is required at startup; there is
no silent "use a publicly-known default key" path.

**Reduced scope, on purpose:** `search()` returns a best-effort mixed list
(`kind: "song_or_video" | "album" | "artist" | "unknown"`) instead of three
separately-filtered `search_songs` / `search_albums` / `search_artists`
tools. YouTube Music's InnerTube API filters search by an opaque encoded
`params` value per type; getting that wrong fails silently rather than
erroring, so this port didn't guess at it without a way to verify. Likewise
`get_browse_info` returns YouTube's raw browse JSON for an album/artist
`browse_id` rather than the TS project's fully-parsed album/artist models —
that parsing layer (`youtube-music/parsers.ts`, ~600 lines) wasn't ported.

## Setup

1. Register a Google Cloud OAuth 2.0 client (Desktop or Web application
   type), with redirect URI `http://127.0.0.1:8765/oauth/callback`
   (or whatever `OAUTH_REDIRECT_PORT` you set).
2. `cp .env.example .env` and fill in `GOOGLE_OAUTH_CLIENT_ID`,
   `GOOGLE_OAUTH_CLIENT_SECRET`, and `ENCRYPTION_KEY`
   (`openssl rand -base64 32`).
3. `cargo build --release`
4. Run it directly once to authenticate:
   `env $(cat .env | xargs) ./target/release/ytmusic-rs`, then call the
   `authenticate` tool from any MCP client (or via
   `npx @modelcontextprotocol/inspector`) and open the printed URL in a
   browser. The token is cached encrypted on disk after that.

## Tools

Auth: `authenticate`, `auth_status`

No-auth (public InnerTube API): `search`, `get_song_info`, `get_browse_info`

Requires `authenticate` first (official YouTube Data API v3): `get_liked_videos`,
`get_playlists`, `get_playlist_items`, `create_playlist`, `update_playlist`,
`delete_playlist`, `add_to_playlist`, `remove_from_playlist`

## Known rough edge in the upstream `rmcp` crate

`rmcp = "1.4.0"`'s own `Cargo.toml` only loosely pins its `rmcp-macros`
companion crate (`^1.4.0`), so a plain `cargo update` can pull in a newer
`rmcp-macros` whose generated code calls a `schema_for_input` helper that
doesn't exist in `rmcp` 1.4.0's `handler::server::common` module — a build
break from version skew between the two crates. `Cargo.toml` here pins
`rmcp-macros = "=1.4.0"` explicitly to avoid it; don't relax that pin without
also bumping `rmcp` in lockstep (or just move to the latest `rmcp` 3.x line).

## Acknowledgments

This is a derivative work, not an independent creation — see
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md) for full attribution to
[`CaullenOmdahl/youtube-music-mcp-server`](https://github.com/CaullenOmdahl/youtube-music-mcp-server),
whose design this project ports to Rust.
