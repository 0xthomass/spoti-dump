# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [3.0.0] - 2026-07-12

A ground-up overhaul of the internals, safety model, and UI. The SQLite database
format and the CLI commands/flags remain compatible; older databases and legacy
JSON/CSV dumps are migrated forward automatically.

### Architecture

- Split the domain model into a dedicated `src/domain/` module (tracks, provider
  links, saved tracks, playlists, sync status, snapshots, merges).
- Decomposed the monolithic web server into `src/web/` (router, context,
  handlers per area, DTOs, projections, operations, conflicts, artwork).
- Split persistence into `src/storage/` (single-connection `library.db` handle,
  separate `runtime.db` handle, migrations, backups, CSV export, legacy import).
- Embedded the React frontend into the binary via `rust-embed`; the `ui` command
  no longer shells out to `npm` to build or serve the frontend on demand.
- Broke the 4,400-line `App.tsx` into typed API client, hooks, contexts, pages,
  and components.

### Reliability & data safety

- Made identity conflicts first-class, persisted records instead of message
  markers sniffed from status strings: discovered provider IDs that collide with
  another canonical row are stored for explicit review, merge, or rejection
  (rejections are tombstoned so they are not re-proposed).
- Added a real schema-migration framework with a stored `schema_version` (now
  v5), ordered forward migrations each in their own transaction, pre-migration
  snapshots, and rejection of databases newer than the app understands. The v5
  migration introduces the typed `track_identity_conflicts` table.
- Reworked storage to a single WAL SQLite connection per data root with
  change-guarded writes: saves compare against the last persisted content and
  skip redundant writes and backups (browsing no longer churns the database).
- Held the canonical state in memory behind an `RwLock`: reads are non-blocking
  and mutations write through under the write lock. Long-running provider
  operations do network I/O off-lock and re-check a library version at commit
  time to detect concurrent user edits.
- Fixed merge correctness: ISRC/fuzzy matches no longer overwrite an existing
  same-provider ID, and provider status merges stay coherent.
- Added per-item resilience for provider pushes (continue-on-failure,
  commit-after-success ordering) and provider token refresh with 401 retry;
  rate-limit responses now set a bounded per-provider cooldown.
- Persisted UI operation history in `runtime.db`; operations interrupted by a
  restart are reported as failed instead of disappearing.

### Security

- Added a cross-origin guard that blocks state-changing requests from
  non-loopback origins (CSRF protection for the loopback-bound API).
- Moved provider credentials into a separate `runtime.db`, forced to `0600` on
  Unix, so library snapshots never copy tokens or cookies.
- Scrubbed secrets from error output; the health endpoint exposes integrity and
  row counts only.
- Added supply-chain gates: `cargo-deny` (advisories/bans/sources) and a
  frontend `npm audit` step in CI.

### UI/UX

- Introduced a design-token system with a light theme and a single unified
  button family.
- Added dedicated pages: Overview, Saved tracks, Tracks, Identity conflicts,
  Identity gaps, Playlists, and Safety.
- Added push-plan previews, a guarded bulk-merge flow for identity conflicts, a
  manual identity-repair form for ID gaps, and live operation tracking.
- Accessibility pass: focus-visible states, keyboard affordances, and WCAG AA
  contrast.

### Testing & tooling

- Expanded the Rust test suite (storage round-trips, merge edge cases, identity
  reconciliation, typed provider-error policy, cross-origin guard) and added a
  frontend test suite with vitest and Testing Library.
- Typed provider errors at the boundary (`ProviderError`/`ProviderFailure`) so
  cooldown and health policy match on structured categories instead of parsing
  message text.
- Hardened the release profile and CI (fmt/clippy `-D warnings`/tests on Ubuntu
  and Windows, eslint, pinned action SHAs).

## [2.0.5]

Prior release.
