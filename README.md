# ytmusic-rs

[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2021%20edition-dea584?logo=rust&logoColor=white)](https://doc.rust-lang.org/edition-guide/rust-2021/index.html)
[![MCP](https://img.shields.io/badge/MCP-stdio%20server-5865f2)](https://modelcontextprotocol.io/)
[![rmcp](https://img.shields.io/badge/rmcp-pinned%201.4.0-6e7781)](https://crates.io/crates/rmcp/1.4.0)

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

1. In the [Google Cloud Console](https://console.cloud.google.com/), create a
   project and **enable the YouTube Data API v3** on it. Creating an OAuth
   client alone is not enough — with the API disabled, `authenticate` still
   succeeds but every playlist/library call fails at runtime.
2. Create an OAuth 2.0 client (Desktop or Web application type) with redirect
   URI `http://127.0.0.1:8765/oauth/callback` (or whatever
   `OAUTH_REDIRECT_PORT` you set). The server requests one scope,
   `https://www.googleapis.com/auth/youtube` — full read/write on your
   YouTube account, including creating and **deleting** playlists.
3. `cp .env.example .env` and fill in `GOOGLE_OAUTH_CLIENT_ID`,
   `GOOGLE_OAUTH_CLIENT_SECRET`, and `ENCRYPTION_KEY`
   (`openssl rand -base64 32`).
4. `cargo build --release`
5. Authenticate once: register the server with an MCP client (see below) and
   call the `authenticate` tool, or drive it directly with
   `npx @modelcontextprotocol/inspector`. The tool returns a Google consent
   URL in its response and deliberately does not block — open that URL
   yourself (nothing auto-opens a browser), approve access, then call
   `auth_status` to confirm the token landed. It's cached encrypted on disk
   after that.

To run the binary standalone with `.env` loaded:

```sh
# bash / zsh
env $(cat .env | xargs) ./target/release/ytmusic-rs
```

```powershell
# PowerShell
Get-Content .env | Where-Object { $_ -match '^\s*[^#].+=' } | ForEach-Object {
    $k, $v = $_ -split '=', 2
    [Environment]::SetEnvironmentVariable($k.Trim(), $v.Trim())
}
./target/release/ytmusic-rs.exe
```

### Token lifetime and Google publishing status

A new OAuth client starts in **Testing** status, and for the `youtube` scope
Google expires test users' refresh tokens after **7 days** — so auth breaks
weekly and you have to re-run `authenticate`. Testing status also caps you at
100 explicitly-added test users. Taking the consent screen through Google's
verification to **In production** is what removes the weekly re-auth.

Sharing a built binary is also not enough to share access: whoever runs it
needs their own `GOOGLE_OAUTH_CLIENT_ID` / `GOOGLE_OAUTH_CLIENT_SECRET`.
Being added as a test user only grants permission to consent against someone
else's app — the durable path is each person registering their own OAuth
client per the steps above.

## Registering with an MCP client

This is a stdio server: point the client at the built binary and pass config
through `env`. All configuration comes from environment variables, and the
server refuses to start if `GOOGLE_OAUTH_CLIENT_ID`,
`GOOGLE_OAUTH_CLIENT_SECRET`, or `ENCRYPTION_KEY` is missing — a bare
`command` entry with no `env` block fails immediately.

```json
{
  "mcpServers": {
    "ytmusic": {
      "command": "/absolute/path/to/ytmusic-rs/target/release/ytmusic-rs",
      "env": {
        "GOOGLE_OAUTH_CLIENT_ID": "...",
        "GOOGLE_OAUTH_CLIENT_SECRET": "...",
        "ENCRYPTION_KEY": "...",
        "OAUTH_REDIRECT_PORT": "8765",
        "TOKEN_STORAGE_PATH": "/absolute/path/to/tokens.enc"
      }
    }
  }
}
```

On Windows, use the `.exe` and escape backslashes:
`"C:\\path\\to\\ytmusic-rs\\target\\release\\ytmusic-rs.exe"`.

`TOKEN_STORAGE_PATH` is optional but worth setting explicitly. It defaults to
`$HOME/.config/ytmusic-rs/tokens.enc`; on Windows `HOME` is usually unset, in
which case `$HOME` resolves to `.` and the token lands in
`./.config/ytmusic-rs/tokens.enc` — relative to whatever working directory the
MCP client happened to launch the server in.

Blank values are treated as unset, so leaving `TOKEN_STORAGE_PATH=` empty in
`.env` — the way `.env.example` ships it — falls back to that default rather
than resolving to an empty path. The same normalization applies to the required
variables: an empty or whitespace-only `GOOGLE_OAUTH_CLIENT_ID`,
`GOOGLE_OAUTH_CLIENT_SECRET`, or `ENCRYPTION_KEY` fails at startup with the
same "is required" error as an unset one.

On Windows, note that `cargo build --release` fails with
`Access is denied. (os error 5)` if an MCP client is currently running the
server — the binary is locked while in use. Stop the server (or quit the
client) before rebuilding.

The OAuth callback listener binds `127.0.0.1` on the machine running the
server, so the browser you approve consent in has to be on that same machine.
Remote, headless, or SSH setups will hang until the 3-minute timeout.

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
