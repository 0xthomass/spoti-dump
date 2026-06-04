# spoti-dump

`spoti-dump` is a local-first music library backup and sync tool. It maintains a canonical SQLite database, then syncs that source of truth with streaming accounts. Each installation keeps provider credentials and library data on the user's machine, so distributing the app does not create a central credential store or a shared database bottleneck.

Current providers:

- Spotify
- YouTube Music

The canonical library database lives at `dump/library.db` by default. Runtime-only state, including linked provider credentials and UI operation history, lives separately at `dump/runtime.db` so library snapshots do not copy account tokens or browser cookies. On Unix, the runtime database is forced to owner-only file permissions. Set `SPOTI_DUMP_DATA_DIR` to keep the `dump/` folder under a dedicated application-data directory. Legacy `dump/library.json` files and legacy Spotify CSV dumps are still accepted and migrated into the database automatically.
Before an existing SQLite database is rewritten, the app saves a timestamped copy under `dump/backups/`. It keeps the newest 50 automatic snapshots.
Schema upgrades also snapshot an existing database before migration.
Manual snapshots created from the Safety page are stored separately under `dump/manual-backups/` and are not pruned by automatic retention.
Older builds that stored provider credentials in `library.db` are migrated automatically: the runtime copies supported credentials into `runtime.db`, snapshots the canonical database, then scrubs the legacy credential rows.

## Model

The project centers on a provider-neutral library database:

- tracks have stable canonical IDs plus provider-specific links
- track-provider availability is stored separately from saved-track or playlist membership
- saved tracks are canonical library entries that reference canonical tracks
- playlists have stable canonical IDs, ordered entries, provider links, and per-provider sync status
- providers can reuse known destination IDs when available, or fall back to metadata matching when syncing across services
- unmatched, missing, and error states are persisted per provider instead of only being printed during a run

That means a Spotify export can enrich the same local source of truth that later syncs into YouTube Music and vice versa. Partial coverage is kept in the database instead of being lost between runs.

Concretely:

- if a track has one provider link, it is known on that provider only
- if it has links on multiple providers, it is known on multiple providers
- if it has a provider link and another provider status of `unmatched`, the database records that as a known gap instead of discarding the failed match

## Source Of Truth

`dump/library.db` is the local source of truth.

- `export` merges a provider's current library into the canonical state
- `resolve-identities` searches a provider catalog for missing track identities, consolidates duplicate canonical rows, and records provider IDs without changing a streaming account
- `import` syncs the canonical state into one provider
- `sync` first merges the source provider into the canonical state, then syncs that state into the destination provider
- when a track or playlist cannot be matched on a destination provider, the unmatched status is written back into the canonical state for later retry or review
- `export-csv` exports the normalized database contents to CSV tables for inspection or backup
- the Safety page can create a manual source-of-truth snapshot before risky provider work

Provider exports are append-only with respect to the source of truth. If a later provider export does not contain a saved track, playlist, or playlist entry that was already recorded in the database, the database keeps that record instead of removing it. This is intentional so incomplete cross-provider syncs do not erase canonical data.

The important consequence is that the project no longer treats one provider export as the whole truth. It accumulates provider links, observed coverage, availability gaps, and unresolved sync problems over time.

## Provider Setup

### Spotify

1. Create a Spotify app in the [Spotify Developer Dashboard](https://developer.spotify.com/dashboard).
2. Add `http://127.0.0.1:8000/callback` as a redirect URI.
3. Copy `.env.example` to `.env` and fill in:

   ```env
   SPOTIFY_CLIENT_ID=your_client_id_here
   SPOTIFY_CLIENT_SECRET=your_client_secret_here
   SPOTIFY_REFRESH_TOKEN=
   ```

4. Leave `SPOTIFY_REFRESH_TOKEN` empty on the first run. The CLI prints a refresh token after browser authorization; save it into `.env` for later runs.

### YouTube Music

The YouTube Music provider currently uses browser session headers.

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

Keep that file private. It grants account access.
If the session expires, capture fresh headers from a signed-in `music.youtube.com` browse request and relink the account. The app rejects stale sessions instead of treating them as an empty library.

## Commands

Every mutating command starts in dry-run mode. Add `--force` to actually change the destination provider.

### Export into the canonical state

```sh
spoti-dump export --provider spotify --force
spoti-dump export --provider youtube-music --force
```

This merges the selected provider's current library into `dump/library.db`.

### Import the canonical state

```sh
spoti-dump import --provider spotify --force
spoti-dump import --provider youtube-music --force
```

This reads `dump/library.db` and applies the canonical state to the target provider.

For a Spotify destination account that should be replaced completely, reset it before importing:

```sh
spoti-dump import --provider spotify --reset --force
```

`--reset` first removes saved tracks and playlists from the Spotify destination, then pushes the canonical database into it. Use it only when the destination account is meant to become a clean copy of the source of truth. YouTube Music pull and push are supported, but account-wide purge/reset is not exposed because this app only enables destructive reset paths with verified provider semantics.
The web app blocks Spotify reset-and-push while provider identity gaps or identity conflicts remain, because purging first and then skipping unresolved rows would create an incomplete destination account.

### Resolve provider identities

```sh
spoti-dump resolve-identities --provider spotify --force
spoti-dump resolve-identities --provider youtube-music --force
spoti-dump resolve-identities --force
```

This is the library-wide reconciliation pass. It searches the selected provider for canonical tracks that do not yet have that provider's ID. When a found provider ID already belongs to another canonical track row, the rows are merged: provider links, sync statuses, artwork, saved-track membership, and playlist references are consolidated into one canonical track.

Run this before pushing when the database has provider-only tracks, such as YouTube Music-only tracks that still need Spotify IDs. Push commands do not perform catalog search; they apply the provider IDs already stored in `dump/library.db`.

The web app exposes the same maintenance step from Overview → Resolve Missing IDs. That library-wide job attempts Spotify and YouTube Music in sequence, skips providers that are unlinked, cooling down, or failing connection checks, and reports those skips in the operation warnings.
After identity sync, use the ID Gaps page to inspect remaining tracks that are still missing Spotify or YouTube Music IDs. Rows with saved-track or playlist references are prioritized because those gaps directly reduce push coverage. Open a row from that page to paste a verified provider track URL or ID into the manual Identity Repair form.

### Sync providers directly

```sh
spoti-dump sync --from spotify --to youtube-music --force
spoti-dump sync --from youtube-music --to spotify --force
```

`sync` exports from the source provider, merges that export into the database, persists the merged database, and then syncs the canonical state into the destination provider.

With `--force`, the merged database is always persisted before destination sync starts, and then written again after sync results come back. That means partial results survive provider failures.

### Export the database as CSV

```sh
spoti-dump export-csv
spoti-dump export-csv --output /path/to/csv-dir
```

This writes normalized CSV tables such as `tracks.csv`, `track_provider_links.csv`, `track_provider_status.csv`, `playlists.csv`, and `playlist_entries.csv`. By default they are written under `dump/csv/`.

### Browse the database in a UI

```sh
spoti-dump ui
spoti-dump ui --port 8787
spoti-dump ui --no-open
```

This starts the local React web app for `dump/library.db`. The runtime serves a compiled frontend bundle at `/app/` and a JSON API at `/api/*`. Canonical mutations are serialized so a background provider sync cannot overwrite a concurrent edit. Pull and push operation history is stored in `runtime.db`, and interrupted work is reported as failed after restart instead of disappearing.

The main interface is an editable source-of-truth console for:

- connecting Spotify and YouTube Music directly from the app
- starting provider imports into the canonical database from the app
- resolving provider identities and deduplicating canonical track rows from a dedicated library-wide maintenance action
- reviewing identity conflicts in a queue before explicitly merging rows and choosing which provider IDs win
- reviewing provider ID gaps in a queue and opening the affected canonical row for manual Spotify or YouTube Music ID repair
- generating a provider push plan before mutating a destination account, including pushable counts, skipped identity gaps, playlist risk examples, and reset blockers
- starting outward syncs from the canonical database into connected providers
- reviewing provider coverage and unmatched pressure
- backfilling missing track artwork into the database from provider-backed identifiers
- removing saved tracks from the canonical library
- renaming or deleting canonical playlists
- removing individual playlist entries
- editing canonical track metadata
- deleting a canonical track everywhere it is referenced
- browsing and paging without full-page reloads

Provider imports remain append-only with respect to the canonical database. Explicit deletes in the app are authoritative and the runtime immediately tries to propagate those deletes to every connected provider that has a linked saved track or playlist.

Before pushing, use each provider card's Push Plan action to inspect what will be applied and what will be skipped. The plan is read-only and does not call Spotify or YouTube Music; it derives from the current source-of-truth database and provider connection health.

By default it listens on `http://127.0.0.1:7878/app/` and tries to open a browser automatically. If the frontend bundle is missing, the `ui` command will try to build it from `frontend/` before starting the server.

The local health endpoint is available at `http://127.0.0.1:7878/api/health`. It reports SQLite integrity and canonical row counts without exposing provider credentials.

### Purge a provider

```sh
spoti-dump purge --provider spotify --force
```

This removes saved tracks and playlists from Spotify. There is no undo. YouTube Music import/export remains available, but account-wide purge/reset is intentionally blocked.

## Matching Behavior

When the database already contains an ID for the destination provider, push reuses that ID directly.

When it does not, run `resolve-identities` or use Overview → Resolve Missing IDs in the web app. That reconciliation pass falls back to metadata matching using:

- title
- artist list
- album
- duration when available

This is what makes cross-provider sync possible, but it also means mismatches are still possible when catalogs differ or metadata is inconsistent. Those misses are written back into `dump/library.db` so they can be retried or inspected later instead of disappearing after the run.

If a discovered provider ID already belongs to another canonical row and both rows also contain conflicting IDs on another provider, the app does not auto-merge. Use the Conflicts page in the web app to compare the source row and candidate owner row; each conflict includes metadata similarity, duration delta, provider confidence when available, saved/playlist impact, and a conservative review recommendation. Then explicitly merge while keeping either the source provider IDs or the candidate provider IDs. If the candidate is the wrong recording, mark it as not the same track; the rejected candidate is recorded so the next identity run does not immediately re-open the same conflict. These canonical repairs do not mutate Spotify or YouTube Music accounts.

## Current Semantics

- Spotify saved tracks map directly to Spotify library tracks.
- YouTube Music currently realizes canonical saved tracks as liked songs.
- Normal Spotify push does not purge unrelated account data. It adds saved tracks and replaces the contents of linked or name-matched canonical playlists.
- Spotify reset-and-push purges the destination account first, then pushes the canonical saved tracks and playlists. The web app only enables it when the push can cover the canonical library without known identity skips or unresolved conflict rows.
- Provider pushes do not search catalogs. They only apply identities already resolved into the canonical database.
- YouTube Music playlist sync fills a replacement playlist before switching the canonical link and deleting the old playlist.

## Build From Source

```sh
git clone https://github.com/0xthomass/spoti-dump.git
cd spoti-dump
cargo run -- --help
```

Build a release binary with:

```sh
cargo build --release
```

If you want to rebuild the web frontend manually:

```sh
cd frontend
npm install
npm run build
```

### Automated Windows builds

If you just need a fresh `.exe`, run the **`build-windows-release`** workflow on GitHub:

1. Push your latest changes (or tag a release with `v*`).
2. In the GitHub UI, open **Actions → build-windows-release → Run workflow**.
3. Download the `spoti-dump-windows.zip` artifact. It contains `spoti-dump.exe`, the compiled web UI, `README.md`, and `.env.example`.
4. Extract the ZIP as a directory. Keep `frontend/dist/` next to `spoti-dump.exe` so the local web UI is available.
5. Attach that ZIP to your GitHub Release if you triggered the workflow manually. When you push a tag (`v1.2.3`, etc.), the workflow auto-attaches the ZIP to the release for you.

## Tips & troubleshooting

- **Spotify browser didn’t open?** Copy the URL printed in the terminal and paste it manually.
- **Spotify redirect error?** Double-check the Spotify dashboard contains `http://127.0.0.1:8000/callback`.
- **Spotify `403` after older exports?** Clear `SPOTIFY_REFRESH_TOKEN` once and reauthorize so the token is recreated with write scopes.
- **YouTube Music auth suddenly stopped working?** Refresh your headers JSON from a fresh signed-in browser session.
- **Need to move machines?** Copy `.env`, your YouTube Music headers JSON, and the `dump` folder.
