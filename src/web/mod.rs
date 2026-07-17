//! Local-first library web server: axum router assembly, the shared
//! application context, the cross-origin guard, and the embedded frontend.

mod artwork;
mod conflicts;
mod dto;
mod error;
mod handlers;
mod mutations;
mod operations;
mod parse;
mod projections;
mod providers;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

use anyhow::{Context, Result};
use axum::extract::{Path, Request};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use rust_embed::RustEmbed;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, RwLock, Semaphore};
use url::Url;

use crate::domain::LibraryState;
use crate::storage;

use error::{ApiError, ErrorPayload};
use handlers::*;
use operations::{load_recovered_operations, OperationRecord};
use providers::spotify_callback;

pub(crate) const PAGE_SIZE: usize = 50;

/// Shared application state passed to every handler.
///
/// The struct is `pub` (with `#[doc(hidden)]`) only so the test-support seam
/// [`build_api_router`]/[`test_context`] can name it across the integration-test
/// crate boundary; all fields stay `pub(crate)`, so it remains opaque and
/// non-constructible outside this crate.
#[doc(hidden)]
pub struct AppContext {
    pub(crate) http_client: reqwest::Client,
    pub(crate) spotify_redirect_uri: String,
    pub(crate) pending_spotify_auth: Mutex<HashMap<String, PendingSpotifyAuth>>,
    pub(crate) operations: StdMutex<HashMap<String, OperationRecord>>,
    /// Canonical library state, held in memory and loaded once at startup.
    /// Browse/read handlers take a read lock; mutating handlers take the write
    /// lock, mutate, and persist write-through before dropping it. SQLite is
    /// never read again after startup except for raw-table browsing/restore.
    pub(crate) library: RwLock<LibraryState>,
    /// Bumped by every user-initiated canonical mutation (mutating handlers and
    /// delete-propagation reconciles). Long-running operations snapshot it
    /// before their off-lock provider I/O and re-check it at commit time to
    /// detect concurrent user edits. Artwork enrichment deliberately does not
    /// bump it: artwork is self-healing bookkeeping, not a user edit.
    pub(crate) library_version: AtomicU64,
    /// One-permit gate so at most one background artwork enrichment pass runs at
    /// a time (debounce): browse handlers try to schedule a pass and give up if
    /// one is already in flight.
    pub(crate) artwork_semaphore: Arc<Semaphore>,
    /// Track IDs that recently yielded no artwork, with the time we last tried,
    /// so enrichment does not hammer external services for artwork-less tracks.
    pub(crate) artwork_negative_cache: StdMutex<HashMap<String, Instant>>,
}

impl AppContext {
    /// Records a user-initiated canonical mutation so long-running operations
    /// can detect concurrent edits.
    pub(crate) fn bump_library_version(&self) {
        self.library_version.fetch_add(1, AtomicOrdering::SeqCst);
    }

    pub(crate) fn library_version(&self) -> u64 {
        self.library_version.load(AtomicOrdering::SeqCst)
    }
}

#[derive(Clone)]
pub(crate) struct PendingSpotifyAuth {
    pub(crate) client_id: String,
    pub(crate) client_secret: String,
}

/// Returns `true` if `host` (a `Host` header value, i.e. `host[:port]`) refers
/// to a loopback address we are willing to serve state-changing requests for.
pub(crate) fn is_loopback_host(host: &str) -> bool {
    // Parse through `Url` so port, userinfo (`user@host`) and other trickery are
    // normalised the same way a browser would; a bare hostname has no scheme so
    // we prefix a synthetic one.
    match Url::parse(&format!("http://{host}")) {
        Ok(url) => matches!(url.host_str(), Some("127.0.0.1") | Some("localhost")),
        Err(_) => false,
    }
}

/// Returns `true` if `origin` is an `http` origin pointing at a loopback host.
/// Loopback is served over plain HTTP, so only the `http` scheme is accepted.
pub(crate) fn is_loopback_http_origin(origin: &str) -> bool {
    match Url::parse(origin) {
        Ok(url) => {
            url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"))
        }
        Err(_) => false,
    }
}

/// Decides whether a request is allowed through the cross-origin guard.
///
/// Safe methods (GET/HEAD/OPTIONS) always pass. For any state-changing method
/// the request must satisfy every check that applies to it:
/// - `Sec-Fetch-Site`, if present, must be `same-origin`, `same-site` or `none`.
/// - `Host`, if present, must be a loopback host.
/// - `Origin`, if present, must be an `http` loopback origin. A missing `Origin`
///   is allowed (curl, some same-origin browser fetches, other CLI tools).
pub(crate) fn is_request_origin_allowed(method: &Method, headers: &HeaderMap) -> bool {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return true;
    }

    if let Some(site) = headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
    {
        if !matches!(site, "same-origin" | "same-site" | "none") {
            return false;
        }
    }

    if let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    {
        if !is_loopback_host(host) {
            return false;
        }
    }

    if let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    {
        if !is_loopback_http_origin(origin) {
            return false;
        }
    }

    true
}

/// Axum middleware that blocks cross-origin state-changing requests so that a
/// web page the user happens to visit cannot drive the local API (which is
/// bound to loopback) into destructive operations.
pub(crate) async fn cross_origin_guard(request: Request, next: Next) -> Response {
    if is_request_origin_allowed(request.method(), request.headers()) {
        next.run(request).await
    } else {
        (
            StatusCode::FORBIDDEN,
            Json(ErrorPayload {
                error: "Cross-origin request blocked.".to_string(),
            }),
        )
            .into_response()
    }
}

pub async fn serve(port: u16, open_browser: bool) -> Result<()> {
    let database_path = storage::library_state_path();
    if !database_path.exists() {
        let _ = storage::read_library_state()?;
    }

    if !database_path.exists() {
        anyhow::bail!(
            "No library database found at {}",
            storage::library_state_path().display()
        );
    }

    let spotify_redirect_uri = format!("http://127.0.0.1:{port}/auth/spotify/callback");
    let operations = load_recovered_operations()?;
    // Load the canonical state once. Every request then reads from (or mutates
    // and writes through) this in-memory copy instead of re-opening SQLite.
    let library = storage::read_library_state_or_new()?;
    let shared = Arc::new(AppContext {
        http_client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?,
        spotify_redirect_uri: spotify_redirect_uri.clone(),
        pending_spotify_auth: Mutex::new(HashMap::new()),
        operations: StdMutex::new(operations),
        library: RwLock::new(library),
        library_version: AtomicU64::new(0),
        artwork_semaphore: Arc::new(Semaphore::new(1)),
        artwork_negative_cache: StdMutex::new(HashMap::new()),
    });

    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| format!("Failed to bind web app on 127.0.0.1:{port}"))?;
    let ui_url = format!("http://127.0.0.1:{port}/app/");

    println!("Library web app listening on {ui_url}");
    println!("Database: {}", database_path.display());
    println!("Frontend bundle: embedded in the executable.");
    println!("Press Ctrl+C to stop the server.");

    if open_browser {
        match open::that(&ui_url) {
            Ok(_) => println!("Opened the library web app in your browser."),
            Err(error) => println!("Could not open a browser automatically: {error}"),
        }
    } else {
        println!("Open this URL manually in your browser: {ui_url}");
    }

    let app = Router::new()
        .route("/", get(root_redirect))
        .route("/app", get(app_redirect))
        .route("/app/", get(frontend_index))
        .route("/app/*path", get(frontend_asset))
        .route("/auth/spotify/callback", get(spotify_callback))
        .nest_service("/api", build_api_router(Arc::clone(&shared)))
        .with_state(shared);

    axum::serve(listener, app).await?;
    Ok(())
}

/// Assembles the `/api` router: every API route plus the [`cross_origin_guard`]
/// layer, with `context` applied as state so the returned router is
/// self-contained. [`serve`] mounts it under `/api` via `nest_service`; the
/// HTTP-level integration tests drive the same router directly with
/// `tower`'s `oneshot`, which is why this (and the `AppContext`/`test_context`
/// seam) is exposed rather than kept private. It is `#[doc(hidden)]` and carries
/// no stability guarantee — it is not part of the crate's supported API.
#[doc(hidden)]
pub fn build_api_router(context: Arc<AppContext>) -> Router {
    let api = Router::new()
        .route("/health", get(api_health))
        .route("/overview", get(api_overview))
        .route("/providers", get(api_providers))
        .route(
            "/providers/spotify/connect/start",
            post(api_start_spotify_connect),
        )
        .route(
            "/providers/youtube-music/connect",
            post(api_connect_youtube_music),
        )
        .route(
            "/providers/:provider/export/start",
            post(api_start_provider_export),
        )
        .route(
            "/providers/:provider/verify/start",
            post(api_start_provider_verify),
        )
        .route(
            "/providers/:provider/preflight",
            get(api_provider_preflight),
        )
        .route(
            "/providers/:provider/push-plan",
            get(api_provider_push_plan),
        )
        .route(
            "/providers/:provider/identity/start",
            post(api_start_provider_identity),
        )
        .route("/identity/start", post(api_start_library_identity))
        .route("/identity/conflicts", get(api_identity_conflicts))
        .route(
            "/identity/conflicts/bulk-merge-plan",
            get(api_identity_conflicts_bulk_merge_plan),
        )
        .route(
            "/identity/conflicts/bulk-merge",
            post(api_identity_conflicts_bulk_merge),
        )
        .route("/identity/gaps", get(api_identity_gaps))
        .route(
            "/providers/:provider/sync/start",
            post(api_start_provider_sync),
        )
        .route(
            "/providers/:provider/reset-sync/start",
            post(api_start_provider_reset_sync),
        )
        .route(
            "/providers/:provider/connection",
            delete(api_disconnect_provider),
        )
        .route("/operations/:operation_id", get(api_operation))
        .route("/saved-tracks", get(api_saved_tracks))
        .route(
            "/saved-tracks/:saved_track_id",
            delete(api_delete_saved_track),
        )
        .route("/tracks", get(api_tracks))
        .route(
            "/tracks/:track_id",
            get(api_track_detail)
                .patch(api_update_track)
                .delete(api_delete_track),
        )
        .route(
            "/tracks/:track_id/identities",
            post(api_apply_track_identity),
        )
        .route("/tracks/:track_id/merge", post(api_merge_track))
        .route(
            "/tracks/:track_id/identity-conflicts/reject",
            post(api_reject_track_identity_conflict),
        )
        .route("/playlists", get(api_playlists))
        .route(
            "/playlists/:playlist_id",
            get(api_playlist_detail)
                .patch(api_update_playlist)
                .delete(api_delete_playlist),
        )
        .route(
            "/playlists/:playlist_id/entries/:entry_id",
            delete(api_delete_playlist_entry),
        )
        .route("/backups", get(api_backups))
        .route("/backups/manual", post(api_create_manual_backup))
        .route("/backups/restore", post(api_restore_backup))
        .layer(middleware::from_fn(cross_origin_guard));

    api.with_state(context)
}

/// Builds an [`AppContext`] whose canonical state is loaded from `root` (via the
/// storage `_in(root)` seam), with a real HTTP client and empty
/// operations/pending-auth/artwork caches at version 0. This is the seam the
/// HTTP-level integration tests use to point the router at an isolated temp data
/// root. Like [`build_api_router`], it is `#[doc(hidden)]` test-support surface
/// with no stability guarantee.
///
/// Handlers that reach the storage layer directly (health, backups, and the
/// write-through persist path) resolve their root from `SPOTI_DUMP_DATA_DIR`
/// rather than from this context, so a test that exercises those must also set
/// that variable to `root`.
#[doc(hidden)]
pub async fn test_context(root: &std::path::Path) -> Arc<AppContext> {
    let library =
        storage::read_library_state_in(root, true).expect("load test library state from root");
    Arc::new(AppContext {
        http_client: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("build test HTTP client"),
        spotify_redirect_uri: "http://127.0.0.1:7878/auth/spotify/callback".to_string(),
        pending_spotify_auth: Mutex::new(HashMap::new()),
        operations: StdMutex::new(HashMap::new()),
        library: RwLock::new(library),
        library_version: AtomicU64::new(0),
        artwork_semaphore: Arc::new(Semaphore::new(1)),
        artwork_negative_cache: StdMutex::new(HashMap::new()),
    })
}

/// Frontend assets, embedded into the executable at compile time from
/// `frontend/dist`. `cargo build` therefore requires the frontend to be built
/// first (`cd frontend && npm run build`); rust-embed fails the build with a
/// clear message otherwise.
#[derive(RustEmbed)]
#[folder = "frontend/dist"]
pub(crate) struct FrontendAssets;

pub(crate) async fn frontend_index() -> Response {
    serve_frontend_asset("index.html")
}

pub(crate) async fn frontend_asset(Path(path): Path<String>) -> Response {
    serve_frontend_asset(&path)
}

/// Serves one embedded frontend asset by request path, guessing the content type
/// with `mime_guess`. Unmatched paths that do not look like a static asset (no
/// file extension) fall back to `index.html` so the SPA's client-side router can
/// handle them; unmatched asset-looking paths return 404.
pub(crate) fn serve_frontend_asset(path: &str) -> Response {
    let trimmed = path.trim_start_matches('/');
    let candidate = if trimmed.is_empty() {
        "index.html"
    } else {
        trimmed
    };

    if let Some((bytes, mime)) = load_frontend_asset(candidate) {
        return frontend_asset_response(bytes, &mime);
    }

    let looks_like_asset = candidate
        .rsplit('/')
        .next()
        .map(|segment| segment.contains('.'))
        .unwrap_or(false);
    if looks_like_asset {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }

    match load_frontend_asset("index.html") {
        Some((bytes, mime)) => frontend_asset_response(bytes, &mime),
        None => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}

pub(crate) fn frontend_asset_response(bytes: Vec<u8>, mime: &str) -> Response {
    ([(header::CONTENT_TYPE, mime.to_string())], bytes).into_response()
}

/// Loads a frontend asset's bytes and MIME type. In debug builds an on-disk
/// `frontend/dist` copy is preferred when present (so the frontend can be rebuilt
/// and reloaded without recompiling the server); release builds always use the
/// embedded copy.
pub(crate) fn load_frontend_asset(path: &str) -> Option<(Vec<u8>, String)> {
    #[cfg(debug_assertions)]
    if let Some(asset) = load_frontend_asset_from_disk(path) {
        return Some(asset);
    }

    let asset = FrontendAssets::get(path)?;
    let mime = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();
    Some((asset.data.into_owned(), mime))
}

/// Reads a frontend asset from the on-disk `frontend/dist` directory (debug-only
/// hot-reload override), guarding against path traversal outside that directory.
#[cfg(debug_assertions)]
pub(crate) fn load_frontend_asset_from_disk(path: &str) -> Option<(Vec<u8>, String)> {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("frontend")
        .join("dist");
    let canonical_base = base.canonicalize().ok()?;
    let full = canonical_base.join(path).canonicalize().ok()?;
    if !full.starts_with(&canonical_base) {
        return None;
    }
    let bytes = std::fs::read(&full).ok()?;
    let mime = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();
    Some((bytes, mime))
}

pub(crate) async fn root_redirect() -> Redirect {
    Redirect::to("/app/")
}

pub(crate) async fn app_redirect() -> Redirect {
    Redirect::to("/app/")
}

/// Write-through persist of the in-memory canonical state. Callers hold the
/// library write lock, mutate `*guard`, then call this before dropping the lock
/// so persistence stays ordered with the in-memory copy. The change-guarded
/// [`storage::write_library_state`] skips redundant writes/backups, and the
/// blocking SQLite work runs on a blocking thread. This clones the state to move
/// it into the blocking task; the clone cost is paid only on actual mutations.
pub(crate) async fn persist_library(state: &LibraryState) -> Result<(), ApiError> {
    let snapshot = state.clone();
    tokio::task::spawn_blocking(move || storage::write_library_state(&snapshot))
        .await
        .context("Failed to join database write task")?
        .map(|_| ())
        .map_err(ApiError::from)
}

/// Runs a blocking runtime-database (`runtime.db`) call on a blocking thread so
/// the async worker is never parked on SQLite. Used for the small provider
/// health/cooldown/connection lookups that previously ran inline.
pub(crate) async fn runtime_db<T, F>(operation: F) -> Result<T, ApiError>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .context("Failed to join runtime database task")?
        .map_err(ApiError::from)
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::BTreeMap;

    use chrono::Utc;

    use crate::domain::{
        LinkSource, ProviderKind, ProviderTrackLink, SavedTrackEntry, TrackEntity, TrackMetadata,
    };

    pub(crate) fn saved_entry(id: &str, track_id: &str, added_at: Option<&str>) -> SavedTrackEntry {
        SavedTrackEntry {
            id: id.to_string(),
            track_id: track_id.to_string(),
            added_at: added_at.map(str::to_string),
            provider_state: BTreeMap::new(),
        }
    }

    pub(crate) fn test_track(id: &str, title: &str) -> TrackEntity {
        TrackEntity {
            id: id.to_string(),
            metadata: TrackMetadata {
                title: title.to_string(),
                artists: vec!["Artist".to_string()],
                album: None,
                duration_seconds: None,
                isrc: None,
            },
            provider_links: BTreeMap::new(),
            provider_artwork: BTreeMap::new(),
            provider_state: BTreeMap::new(),
            identity_conflicts: Vec::new(),
        }
    }

    pub(crate) struct IdentityConflictPairFixture<'a> {
        pub(crate) source_id: &'a str,
        pub(crate) source_title: &'a str,
        pub(crate) owner_id: &'a str,
        pub(crate) owner_title: &'a str,
        pub(crate) candidate_provider: ProviderKind,
        pub(crate) candidate_provider_id: &'a str,
        pub(crate) source_duration_seconds: Option<u32>,
        pub(crate) owner_duration_seconds: Option<u32>,
        pub(crate) confidence: Option<f64>,
        pub(crate) now: chrono::DateTime<Utc>,
    }

    pub(crate) fn identity_conflict_pair(
        fixture: IdentityConflictPairFixture<'_>,
    ) -> (TrackEntity, TrackEntity) {
        let conflicting_provider = match fixture.candidate_provider {
            ProviderKind::Spotify => ProviderKind::YoutubeMusic,
            ProviderKind::YoutubeMusic => ProviderKind::Spotify,
        };
        let mut source = test_track_with_link(
            fixture.source_id,
            fixture.source_title,
            conflicting_provider,
            &format!("{}-source", conflicting_provider.as_key()),
            fixture.now,
        );
        source.metadata.duration_seconds = fixture.source_duration_seconds;
        source.record_identity_conflict(
            fixture.candidate_provider,
            fixture.candidate_provider_id,
            fixture.confidence,
            fixture.now,
        );

        let mut owner = test_track_with_link(
            fixture.owner_id,
            fixture.owner_title,
            conflicting_provider,
            &format!("{}-owner", conflicting_provider.as_key()),
            fixture.now,
        );
        owner.metadata.duration_seconds = fixture.owner_duration_seconds;
        owner.provider_links.insert(
            fixture.candidate_provider.as_key().to_string(),
            ProviderTrackLink {
                provider_id: fixture.candidate_provider_id.to_string(),
                source: LinkSource::Export,
                confidence: Some(1.0),
                linked_at: fixture.now,
                last_seen_at: Some(fixture.now),
            },
        );

        (source, owner)
    }

    pub(crate) fn test_track_with_link(
        id: &str,
        title: &str,
        provider: ProviderKind,
        provider_id: &str,
        now: chrono::DateTime<Utc>,
    ) -> TrackEntity {
        let mut track = test_track(id, title);
        track.provider_links.insert(
            provider.as_key().to_string(),
            ProviderTrackLink {
                provider_id: provider_id.to_string(),
                source: LinkSource::Export,
                confidence: Some(1.0),
                linked_at: now,
                last_seen_at: Some(now),
            },
        );
        track
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method};

    use super::is_request_origin_allowed;

    fn header_map(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn cross_origin_guard_rejects_cross_site_origin() {
        // A POST from an attacker-controlled page must be blocked even though it
        // targets the loopback host.
        let headers = header_map(&[
            (header::HOST.as_str(), "127.0.0.1:7878"),
            (header::ORIGIN.as_str(), "https://evil.example.com"),
        ]);
        assert!(!is_request_origin_allowed(&Method::POST, &headers));

        // An http origin on a non-loopback host is likewise rejected.
        let headers = header_map(&[
            (header::HOST.as_str(), "127.0.0.1:7878"),
            (header::ORIGIN.as_str(), "http://evil.example.com"),
        ]);
        assert!(!is_request_origin_allowed(&Method::POST, &headers));
    }

    #[test]
    fn cross_origin_guard_allows_same_origin_request() {
        let headers = header_map(&[
            (header::HOST.as_str(), "127.0.0.1:7878"),
            (header::ORIGIN.as_str(), "http://127.0.0.1:7878"),
            ("sec-fetch-site", "same-origin"),
        ]);
        assert!(is_request_origin_allowed(&Method::POST, &headers));

        // localhost (any port) is treated as loopback too.
        let headers = header_map(&[
            (header::HOST.as_str(), "localhost:7878"),
            (header::ORIGIN.as_str(), "http://localhost:7878"),
        ]);
        assert!(is_request_origin_allowed(&Method::DELETE, &headers));
    }

    #[test]
    fn cross_origin_guard_allows_request_without_origin() {
        // curl / CLI tools and some same-origin browser fetches omit Origin.
        let headers = header_map(&[(header::HOST.as_str(), "127.0.0.1:7878")]);
        assert!(is_request_origin_allowed(&Method::POST, &headers));

        // No headers at all is also allowed for state-changing methods.
        assert!(is_request_origin_allowed(&Method::POST, &HeaderMap::new()));
    }

    #[test]
    fn cross_origin_guard_rejects_evil_host() {
        let headers = header_map(&[(header::HOST.as_str(), "evil.example.com")]);
        assert!(!is_request_origin_allowed(&Method::POST, &headers));

        // A look-alike host that merely embeds the loopback literal is rejected.
        let headers = header_map(&[(header::HOST.as_str(), "127.0.0.1.evil.example.com")]);
        assert!(!is_request_origin_allowed(&Method::POST, &headers));
    }

    #[test]
    fn cross_origin_guard_allows_safe_methods_regardless_of_headers() {
        // GET must stay reachable even from a cross-site context (e.g. the
        // Spotify OAuth callback redirect).
        let headers = header_map(&[
            (header::HOST.as_str(), "evil.example.com"),
            (header::ORIGIN.as_str(), "https://evil.example.com"),
        ]);
        assert!(is_request_origin_allowed(&Method::GET, &headers));
        assert!(is_request_origin_allowed(&Method::HEAD, &headers));
        assert!(is_request_origin_allowed(&Method::OPTIONS, &headers));
    }

    #[test]
    fn cross_origin_guard_rejects_disallowed_sec_fetch_site() {
        let headers = header_map(&[
            (header::HOST.as_str(), "127.0.0.1:7878"),
            (header::ORIGIN.as_str(), "http://127.0.0.1:7878"),
            ("sec-fetch-site", "cross-site"),
        ]);
        assert!(!is_request_origin_allowed(&Method::POST, &headers));
    }
}
