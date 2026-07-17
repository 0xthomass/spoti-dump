# spoti-dump

`spoti-dump` is a local-first music library backup and sync tool. It keeps a canonical SQLite database of your library on your own machine and treats that database as the source of truth, syncing it out to (and pulling it in from) streaming accounts. Provider credentials and library data never leave the device, and each install is self-contained: distributing the app does not create a shared credential store or a central database.

Providers:

- Spotify
- YouTube Music

The app is a single Rust binary. The web UI is a React + Vite single-page app that is compiled into the binary at build time, so running the UI is just `cargo run -- ui` — there is no separate frontend server to start.

## Quick start

```sh
git clone https://github.com/0xthomass/spoti-dump.git
cd spoti-dump

# The frontend is embedded into the binary at compile time, so it must be
# built first. (CI and the release workflow do this for you.)
cd frontend && npm ci && npm run build && cd ..

# Launch the local web app (binds 127.0.0.1:7878, opens a browser).
cargo run -- ui
```

`cargo run -- ui` starts the local server at `http://127.0.0.1:7878/app/` and tries to open it in your browser. From there you connect a provider, pull your library into the canonical database, and push it back out. See [Provider setup](#provider-setup) for credentials.

The CLI is also available directly:

```sh
cargo run -- --help
cargo build --release   # release binary at target/release/spoti-dump
```

### Frontend dev workflow

The embedded bundle is fine for normal use, but for iterating on the UI you can run Vite's dev server against a live backend:

1. Start the backend: `cargo run -- ui` (serves the API on `127.0.0.1:7878`).
2. In another shell: `cd frontend && npm run dev`.
3. Open the Vite dev URL. Vite proxies `/api` and `/auth` to `http://127.0.0.1:7878`, so the dev frontend talks to the running backend.

In debug builds the server also prefers an on-disk `frontend/dist` copy when one exists, so a rebuilt bundle is picked up without recompiling the binary. Release builds always serve the embedded copy.

## Data model

The project centers on a provider-neutral library database:

- tracks have stable canonical IDs plus provider-specific links
- track-provider availability is stored separately from saved-track or playlist membership
- saved tracks are canonical library entries that reference canonical tracks
- playlists have stable canonical IDs, ordered entries, provider links, and per-provider sync status
- unmatched, missing, and error states are persisted per provider instead of only being printed during a run
- identity conflicts are first-class, persisted records: when a discovered provider ID would collide with another canonical row, the conflict is stored and reviewed rather than silently merged or dropped

That means a Spotify export can enrich the same local source of truth that later syncs into YouTube Music and vice versa. Partial coverage is kept in the database instead of being lost between runs.

Concretely:

- if a track has one provider link, it is known on that provider only
- if it has links on multiple providers, it is known on multiple providers
- if it has a provider link plus another provider status of `unmatched`, the database records that as a known gap instead of discarding the failed match

## Source of truth

`dump/library.db` is the local source of truth.

- `export` merges a provider's current library into the canonical state
- `resolve-identities` searches a provider catalog for missing track identities, consolidates duplicate canonical rows, and records provider IDs — without changing any streaming account
- `import` syncs the canonical state into one provider
- `sync` first merges the source provider into the canonical state, then syncs that state into the destination provider
- when a track or playlist cannot be matched on a destination provider, the unmatched status is written back into the canonical state for later retry or review
- `export-csv` exports the normalized database to CSV tables for inspection or backup
- the Safety page can create a manual snapshot before risky provider work

Provider exports are **append-only** with respect to the source of truth. If a later provider export does not contain a saved track, playlist, or playlist entry that was already recorded, the database keeps that record instead of removing it. This is intentional so incomplete cross-provider syncs do not erase canonical data. Explicit deletes made in the web UI are authoritative and are propagated outward to connected providers.

The consequence is that no single provider export is treated as the whole truth. The database accumulates provider links, observed coverage, availability gaps, and unresolved sync problems over time.

## Web UI

`cargo run -- ui` serves the app at `http://127.0.0.1:7878/app/` and a JSON API at `/api/*`. The canonical state is loaded into memory once at startup; browse/read requests take a read lock and never touch SQLite again, while mutations take the write lock, apply the change, and persist write-through before releasing it. Provider operations do their network I/O off-lock and re-check for concurrent user edits before committing, so a background sync cannot silently overwrite an edit you made while it ran.

Pages:

- **Overview** — library totals, provider connection health, readiness for pushing, and the library-wide "Resolve Missing IDs" maintenance action.
- **Saved tracks** — the canonical saved-track library; remove entries.
- **Tracks** — every canonical track; edit metadata, backfill artwork, merge/repair identities, delete a track everywhere it is referenced.
- **Identity conflicts** — a review queue of collisions where a discovered provider ID already belongs to another canonical row. Each entry shows metadata similarity, duration delta, provider confidence, saved/playlist impact, and a conservative recommendation. Merge individually or run a guarded bulk merge (which snapshots first and re-checks every row); reject a wrong candidate to tombstone it.
- **Identity gaps** — canonical tracks still missing a Spotify or YouTube Music ID, prioritized by whether they affect saved tracks or playlists. Open a row to paste a verified provider track URL/ID into the manual Identity Repair form.
- **Playlists** — canonical playlists and entries; rename, delete, or remove individual entries.
- **Safety** — automatic/manual backups, create a manual snapshot, and restore a backup.

Typical flow: connect a provider, pull (export) its library into the canonical database, resolve missing IDs, review any identity conflicts, inspect the push plan, then push (sync/import) back out. Long-running operations (pulls, pushes, identity runs) are tracked: their progress is shown live, their history is stored in `runtime.db`, and any operation interrupted by a restart is reported as failed rather than vanishing.

Before pushing, use a provider card's **Push Plan** action to see what would be applied and what would be skipped. The plan is read-only — it derives from the current database and provider health and does not call Spotify or YouTube Music.

The health endpoint at `http://127.0.0.1:7878/api/health` reports SQLite integrity and canonical row counts without exposing provider credentials.

## CLI reference

Every mutating command is a **dry run by default**. Add `--force` to actually change the canonical database or a destination provider.

| Command | Flags | Purpose |
| --- | --- | --- |
| `export` | `--provider <spotify\|youtube-music>` `--force` | Pull a provider's library and merge it into the canonical database. |
| `import` | `--provider <spotify\|youtube-music>` `--force` `--reset` | Push the canonical library into one provider. `--reset` purges the destination first (Spotify only). |
| `sync` | `--from <provider>` `--to <provider>` `--force` | Export from the source, merge into the database, persist, then push into the destination. |
| `resolve-identities` | `--provider <provider>` (optional) `--force` | Library-wide reconciliation: search a provider catalog for missing IDs and consolidate duplicate canonical rows. Omitting `--provider` runs every provider in sequence. |
| `export-csv` | `--output <dir>` (optional) | Write normalized CSV tables. Defaults to `dump/csv/`. |
| `ui` | `--port <n>` (default `7878`) `--no-open` | Serve the local web app. `--no-open` skips launching a browser. |
| `purge` | `--provider <spotify\|youtube-music>` `--force` | Delete saved tracks and playlists from a provider account (Spotify only). No undo. |

Notes:

- `export-csv` and `ui` are read-only and take no `--force`.
- Providers are selected with `--provider spotify` or `--provider youtube-music`.
- Account-wide reset/purge is only enabled for providers with verified reset semantics. Spotify supports `--reset`/`purge`; YouTube Music does not (pull and push still work). The web app additionally blocks Spotify reset-and-push while identity gaps or open conflicts remain, since purging first and then skipping unresolved rows would leave the destination incomplete.
- Push commands do not search catalogs — they only apply provider IDs already resolved into the database. Run `resolve-identities` (or Overview → Resolve Missing IDs) first when the database has provider-only tracks.
- With `--force`, `sync` and `import` persist the merged database before the destination push and again after results come back, so partial results survive a provider failure. A rate-limit failure records a per-provider cooldown that blocks further calls until it expires.

## Storage and data layout

The `dump/` folder holds everything:

- `dump/library.db` — the canonical library (SQLite, WAL). Set `SPOTI_DUMP_DATA_DIR` to relocate the `dump/` folder under a dedicated data directory; when unset the app looks for an existing `dump/` beside the binary and falls back to the current directory.
- `dump/runtime.db` — runtime-only state: linked provider credentials, provider health/cooldowns, and UI operation history. Kept separate so library snapshots never copy account tokens or browser cookies. On Unix its file mode is forced to `0600` (owner-only).
- `dump/backups/` — automatic pre-write snapshots, pruned to the newest **50**.
- `dump/manual-backups/` — snapshots created from the Safety page; never auto-pruned.
- `dump/csv/` — default `export-csv` output.

**Change-guarded writes.** A single SQLite connection is kept open per data root for the process lifetime. Each save compares the serialized state against the last persisted content and skips the write (and its backup) when nothing changed, so browsing the UI does not churn the database or spawn redundant snapshots.

**Schema migrations.** The library carries a `schema_version` (currently **5**). A fresh database is created at the current version; an older one is snapshotted into `dump/backups/` and then migrated forward one version at a time, each migration in its own transaction; a database newer than the app understands is rejected with a clear message. The v5 migration introduces the first-class `track_identity_conflicts` table and converts older message-encoded conflict markers into typed rows.

**Legacy import.** When no `library.db` exists, a legacy `dump/library.json` or a legacy Spotify CSV dump is imported once and written into the database automatically. Older databases that stored provider credentials inside `library.db` are migrated: supported credentials are copied into `runtime.db`, the database is snapshotted, and the legacy credential rows are scrubbed.

## Provider setup

Copy `.env.example` to `.env` and fill in credentials.

### Spotify

1. Create an app in the [Spotify Developer Dashboard](https://developer.spotify.com/dashboard).
2. Add redirect URIs:
   - `http://127.0.0.1:7878/auth/spotify/callback` — for connecting from the web UI (adjust the port if you pass `--port`).
   - `http://127.0.0.1:8000/callback` — for the CLI authorization flow.
3. Connect either from the web UI (paste client ID + secret into the Spotify card) or via `.env`:

   ```env
   SPOTIFY_CLIENT_ID=your_client_id_here
   SPOTIFY_CLIENT_SECRET=your_client_secret_here
   SPOTIFY_REFRESH_TOKEN=
   ```

   Leave `SPOTIFY_REFRESH_TOKEN` empty on the first CLI run; after browser authorization a refresh token is printed and persisted so later runs reuse it.

### YouTube Music

YouTube Music uses browser session headers.

1. Sign in to [music.youtube.com](https://music.youtube.com).
2. Open Developer Tools and inspect a request to the site.
3. Save the `cookie` and `x-goog-authuser` headers into a JSON file:

   ```json
   {
     "cookie": "SID=...; __Secure-3PAPISID=...; ...",
     "x-goog-authuser": "0"
   }
   ```

4. Point `YOUTUBE_MUSIC_HEADERS_PATH` at that file:

   ```env
   YOUTUBE_MUSIC_HEADERS_PATH=ytmusic_headers.json
   ```

Keep that file private — it grants account access. If the session expires, capture fresh headers from a signed-in `music.youtube.com` request and relink. The app rejects a stale session instead of treating it as an empty library.

## Matching behavior

When the database already has an ID for the destination provider, push reuses it directly. When it does not, `resolve-identities` (or Overview → Resolve Missing IDs) falls back to metadata matching on title, artist list, album, and duration when available. This is what makes cross-provider sync possible, but mismatches are still possible when catalogs or metadata differ, so misses are written back into `library.db` for later retry or review.

If a discovered provider ID already belongs to another canonical row and both rows also carry conflicting IDs on a third provider, the app does not auto-merge. It records an identity conflict for review on the Conflicts page, where you compare the two rows and either merge (choosing which provider IDs win) or reject the candidate. A rejected candidate is tombstoned so the next identity run does not re-open the same conflict.

## Security

- The server binds `127.0.0.1` only.
- A cross-origin guard rejects state-changing requests (POST/PATCH/DELETE) that come from a non-loopback origin, so a web page you happen to visit cannot drive the local API into destructive operations. Safe methods (GET/HEAD/OPTIONS) always pass so the OAuth callback redirect works.
- Provider credentials live in `runtime.db`, separate from the library, and are `0600` on Unix. Secrets are never printed to logs or surfaced through the health endpoint.

## Development

Build (the frontend bundle must exist first, or the Rust build fails):

```sh
cd frontend && npm ci && npm run build && cd ..
cargo build
```

Test:

```sh
cargo test
cd frontend && npm test   # vitest
```

Lint:

```sh
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
cd frontend && npm run lint   # eslint
```

CI (`.github/workflows/ci.yml`) runs three jobs:

- **frontend** — `npm ci`, eslint, vitest, `npm audit`, and a production build.
- **rust** — `cargo fmt --check`, `cargo test --all-targets`, and `cargo clippy -D warnings` on Ubuntu and Windows.
- **deny** — `cargo-deny check advisories bans sources` for supply-chain gates.

## Windows release build

If you just need a fresh `.exe`, run the **`build-windows-release`** workflow (`.github/workflows/windows-release.yml`):

1. Push your changes, or tag a release with `v*`.
2. In GitHub, open **Actions → build-windows-release → Run workflow** (or push a `v1.2.3` tag).
3. Download the `spoti-dump-windows` artifact. The ZIP contains `spoti-dump.exe`, the compiled web UI under `frontend/dist/`, `README.md`, and `.env.example`.
4. Extract it and keep `frontend/dist/` next to `spoti-dump.exe`.

When triggered by a tag, the workflow also attaches the ZIP to the GitHub Release automatically.

## Tips and troubleshooting

- **Spotify browser didn't open?** Copy the URL printed in the terminal and paste it manually.
- **Spotify redirect error?** Confirm the dashboard has both redirect URIs from [Provider setup](#spotify).
- **Spotify `403` after older exports?** Clear `SPOTIFY_REFRESH_TOKEN` once and reauthorize so the token is recreated with write scopes.
- **YouTube Music auth stopped working?** Refresh the headers JSON from a fresh signed-in browser session and relink.
- **Provider "cooling down"?** A recent rate-limit response set a cooldown; wait until it expires or clear it by letting the next successful call reset it.
- **Moving machines?** Copy `.env`, your YouTube Music headers JSON, and the `dump/` folder.
