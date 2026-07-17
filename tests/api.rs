//! HTTP-level integration tests for the local-first library web API.
//!
//! Each test seeds an isolated temp data root through the storage `_in(root)`
//! seam, builds the real `/api` router over that root with
//! [`spoti_dump::web::test_context`] + [`spoti_dump::web::build_api_router`], and
//! drives it in-process with `tower`'s `oneshot` (no bound TCP port).
//!
//! Several handlers (health, backups, and the write-through persist path) resolve
//! their data root from `SPOTI_DUMP_DATA_DIR` rather than from the request-scoped
//! context, and that variable is process-global. Tests therefore serialize on
//! [`ENV_GUARD`] and each points the variable at its own unique temp root while it
//! holds the guard, so no two tests interfere.

use std::collections::BTreeMap;

use axum::body::{to_bytes, Body};
use axum::http::{header, Request, StatusCode};
use axum::Router;
use chrono::Utc;
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::sync::{Mutex, MutexGuard};
use tower::ServiceExt;

use spoti_dump::domain::{
    IdentityConflictStatus, LibraryState, LinkSource, PlaylistEntity, PlaylistEntry, ProviderKind,
    ProviderTrackArtwork, ProviderTrackLink, SavedTrackEntry, TrackEntity, TrackIdentityConflict,
    TrackMetadata,
};
use spoti_dump::storage;
use spoti_dump::web::{build_api_router, test_context};

/// Serializes tests because `SPOTI_DUMP_DATA_DIR` is process-global; the guard is
/// held for the whole test body so the env var and the router's context always
/// agree on the same isolated root. An async mutex is used because the guard is
/// deliberately held across the awaits in each test.
static ENV_GUARD: Mutex<()> = Mutex::const_new(());

/// A live router over a freshly seeded, isolated temp data root. Holds the
/// serialization guard and the temp dir alive for the duration of the test.
struct TestApp {
    app: Router,
    _dir: TempDir,
    _guard: MutexGuard<'static, ()>,
}

impl TestApp {
    async fn call(&self, request: Request<Body>) -> (StatusCode, Value) {
        let response = self
            .app
            .clone()
            .oneshot(request)
            .await
            .expect("router produced a response");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("response body is valid JSON")
        };
        (status, json)
    }
}

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .body(Body::empty())
        .expect("build GET request")
}

fn json_request(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&body).expect("serialize body"),
        ))
        .expect("build JSON request")
}

/// A POST that carries only an `Origin` header (empty body), used to probe the
/// cross-origin guard.
fn post_with_origin(uri: &str, origin: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method("POST").uri(uri);
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    builder.body(Body::empty()).expect("build POST request")
}

async fn setup(state: LibraryState) -> TestApp {
    let guard = ENV_GUARD.lock().await;
    let dir = tempfile::tempdir().expect("create temp data root");
    std::env::set_var(storage::DATA_DIR_ENV, dir.path());
    storage::write_library_state_in(dir.path(), &state).expect("seed library state");
    let context = test_context(dir.path()).await;
    let app = Router::new().nest("/api", build_api_router(context));
    TestApp {
        app,
        _dir: dir,
        _guard: guard,
    }
}

// ---------------------------------------------------------------------------
// Seed fixture
// ---------------------------------------------------------------------------

fn artwork() -> BTreeMap<String, ProviderTrackArtwork> {
    // A single artwork entry per track keeps the browse handlers from scheduling
    // a background artwork-enrichment pass (which would attempt network I/O):
    // `collect_artwork_targets` skips any track that already has artwork.
    let mut map = BTreeMap::new();
    map.insert(
        ProviderKind::YoutubeMusic.as_key().to_string(),
        ProviderTrackArtwork {
            url: "https://example.test/art.jpg".to_string(),
            width: Some(480),
            height: Some(360),
            last_seen_at: None,
        },
    );
    map
}

fn link(provider_id: &str) -> ProviderTrackLink {
    ProviderTrackLink {
        provider_id: provider_id.to_string(),
        source: LinkSource::Export,
        confidence: None,
        linked_at: Utc::now(),
        last_seen_at: Some(Utc::now()),
    }
}

fn track(id: &str, title: &str, artist: &str, links: &[(ProviderKind, &str)]) -> TrackEntity {
    let mut provider_links = BTreeMap::new();
    for (provider, provider_id) in links {
        provider_links.insert(provider.as_key().to_string(), link(provider_id));
    }
    TrackEntity {
        id: id.to_string(),
        metadata: TrackMetadata {
            title: title.to_string(),
            artists: vec![artist.to_string()],
            album: Some(format!("{title} Album")),
            duration_seconds: Some(200),
            isrc: None,
        },
        provider_links,
        provider_artwork: artwork(),
        provider_state: BTreeMap::new(),
        identity_conflicts: Vec::new(),
    }
}

/// Builds a deterministic library:
/// * 4 tracks (alpha is multi-provider; beta and delta are spotify-only; gamma is
///   youtube-only and owns the disputed `yt-shared` id),
/// * 2 saved tracks (alpha, beta),
/// * 1 playlist with 2 entries (alpha, gamma),
/// * 1 open identity conflict: delta proposes YouTube `yt-shared`, which gamma
///   already owns, so the conflict has a real owner and surfaces in the queue.
fn seed_state() -> LibraryState {
    let mut state = LibraryState::new();

    let alpha = track(
        "track-alpha",
        "Alpha",
        "Alpha Artist",
        &[
            (ProviderKind::Spotify, "sp-alpha"),
            (ProviderKind::YoutubeMusic, "yt-alpha"),
        ],
    );
    let beta = track(
        "track-beta",
        "Beta",
        "Beta Artist",
        &[(ProviderKind::Spotify, "sp-beta")],
    );
    let gamma = track(
        "track-gamma",
        "Gamma",
        "Gamma Artist",
        &[(ProviderKind::YoutubeMusic, "yt-shared")],
    );
    let mut delta = track(
        "track-delta",
        "Delta",
        "Delta Artist",
        &[(ProviderKind::Spotify, "sp-delta")],
    );
    // Open conflict against gamma's YouTube id, owned by another track.
    delta.identity_conflicts.push(TrackIdentityConflict {
        provider: ProviderKind::YoutubeMusic,
        candidate_provider_id: "yt-shared".to_string(),
        confidence: Some(0.72),
        detected_at: Utc::now(),
        status: IdentityConflictStatus::Open,
        rejected_at: None,
    });

    state.tracks = vec![alpha, beta, gamma, delta];

    state.saved_tracks = vec![
        SavedTrackEntry {
            id: "saved-alpha".to_string(),
            track_id: "track-alpha".to_string(),
            added_at: Some(Utc::now().to_rfc3339()),
            provider_state: BTreeMap::new(),
        },
        SavedTrackEntry {
            id: "saved-beta".to_string(),
            track_id: "track-beta".to_string(),
            added_at: Some(Utc::now().to_rfc3339()),
            provider_state: BTreeMap::new(),
        },
    ];

    state.playlists = vec![PlaylistEntity {
        id: "playlist-one".to_string(),
        name: "Playlist One".to_string(),
        description: Some("Test playlist".to_string()),
        provider_links: BTreeMap::new(),
        provider_state: BTreeMap::new(),
        entries: vec![
            PlaylistEntry {
                id: "entry-1".to_string(),
                track_id: "track-alpha".to_string(),
                added_at: Some(Utc::now().to_rfc3339()),
                provider_state: BTreeMap::new(),
            },
            PlaylistEntry {
                id: "entry-2".to_string(),
                track_id: "track-gamma".to_string(),
                added_at: Some(Utc::now().to_rfc3339()),
                provider_state: BTreeMap::new(),
            },
        ],
    }];

    state.validate().expect("seed state is valid");
    state
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_reports_ok_and_seeded_counts() {
    let app = setup(seed_state()).await;
    let (status, body) = app.call(get("/api/health")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["integrity_check"], "ok");
    assert_eq!(body["tracks"], 4);
    assert_eq!(body["saved_tracks"], 2);
    assert_eq!(body["playlists"], 1);
    assert_eq!(body["playlist_entries"], 2);
    assert_eq!(body["durable_operation_history"], true);
}

#[tokio::test]
async fn overview_reports_expected_rollups() {
    let app = setup(seed_state()).await;
    let (status, body) = app.call(get("/api/overview")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tracks"], 4);
    assert_eq!(body["saved_tracks"], 2);
    assert_eq!(body["playlists"], 1);
    assert_eq!(body["playlist_entries"], 2);
    assert_eq!(body["identity_conflicts"], 1);
}

#[tokio::test]
async fn tracks_and_saved_tracks_paginate() {
    let app = setup(seed_state()).await;

    let (status, body) = app.call(get("/api/tracks")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 4);
    assert_eq!(body["page"], 1);
    assert_eq!(body["page_size"], 50);
    assert_eq!(body["total_pages"], 1);
    assert_eq!(body["items"].as_array().expect("items array").len(), 4);

    let (status, body) = app.call(get("/api/saved-tracks")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 2);
    assert_eq!(body["page"], 1);
    assert_eq!(body["page_size"], 50);
    assert_eq!(body["items"].as_array().expect("items array").len(), 2);
}

#[tokio::test]
async fn patch_track_updates_metadata_and_is_reflected() {
    let app = setup(seed_state()).await;

    let (status, _) = app
        .call(json_request(
            "PATCH",
            "/api/tracks/track-beta",
            json!({
                "title": "Beta Renamed",
                "artists": ["Beta Artist", "Featured"],
                "album": "New Album",
                "duration_seconds": 222,
                "isrc": null
            }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = app.call(get("/api/tracks/track-beta")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["title"], "Beta Renamed");
    assert_eq!(body["duration_seconds"], 222);
    assert_eq!(body["album"], "New Album");
    assert_eq!(body["artists"].as_array().expect("artists array").len(), 2);
}

#[tokio::test]
async fn delete_saved_track_removes_it() {
    let app = setup(seed_state()).await;

    let (status, _) = app
        .call(json_request(
            "DELETE",
            "/api/saved-tracks/saved-beta",
            Value::Null,
        ))
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = app.call(get("/api/saved-tracks")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 1);
    let remaining: Vec<&str> = body["items"]
        .as_array()
        .expect("items array")
        .iter()
        .map(|item| item["saved_track_id"].as_str().expect("saved_track_id"))
        .collect();
    assert_eq!(remaining, vec!["saved-alpha"]);
}

#[tokio::test]
async fn identity_conflict_surfaces_then_reject_removes_it() {
    let app = setup(seed_state()).await;

    let (status, body) = app.call(get("/api/identity/conflicts")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 1);
    let item = &body["items"][0];
    assert_eq!(item["source_track"]["track_id"], "track-delta");
    assert_eq!(item["conflict"]["provider"], "youtube-music");
    assert_eq!(item["conflict"]["provider_id"], "yt-shared");
    assert_eq!(item["conflict"]["owner_track"]["track_id"], "track-gamma");

    let (status, _) = app
        .call(json_request(
            "POST",
            "/api/tracks/track-delta/identity-conflicts/reject",
            json!({
                "provider": "youtube-music",
                "provider_id": "yt-shared",
                "owner_track_id": "track-gamma"
            }),
        ))
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = app.call(get("/api/identity/conflicts")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 0);
    assert_eq!(body["items"].as_array().expect("items array").len(), 0);
}

#[tokio::test]
async fn bulk_merge_with_empty_plan_is_a_no_op() {
    let app = setup(seed_state()).await;

    // A query that matches nothing yields an empty eligible set, so the merge
    // loop runs zero iterations and must not crash.
    let (status, body) = app
        .call(json_request(
            "POST",
            "/api/identity/conflicts/bulk-merge",
            json!({
                "q": "zzz-no-such-conflict-xyz",
                "conflict_resolution": "keep_target"
            }),
        ))
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["eligible_count"], 0);
    assert_eq!(body["merged_count"], 0);
    assert_eq!(body["skipped_count"], 0);
    assert_eq!(body["resolved_provider_conflicts"], 0);
    // An empty plan must not snapshot a no-op backup.
    assert!(body["pre_merge_backup_path"].is_null());
    let (_, backups) = app.call(get("/api/backups")).await;
    let manual_count = backups["backups"]
        .as_array()
        .expect("backups array")
        .iter()
        .filter(|backup| backup["backup_type"] == "manual")
        .count();
    assert_eq!(
        manual_count, 0,
        "an empty bulk merge must not create a manual backup"
    );
}

#[tokio::test]
async fn manual_backup_is_created_and_listed() {
    let app = setup(seed_state()).await;

    let (status, body) = app
        .call(post_with_origin("/api/backups/manual", None))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["backup"]["backup_type"], "manual");
    let created = body["backup"]["file_name"]
        .as_str()
        .expect("created backup file name")
        .to_string();

    let (status, body) = app.call(get("/api/backups")).await;
    assert_eq!(status, StatusCode::OK);
    let listed: Vec<&str> = body["backups"]
        .as_array()
        .expect("backups array")
        .iter()
        .filter(|backup| backup["backup_type"] == "manual")
        .map(|backup| backup["file_name"].as_str().expect("file name"))
        .collect();
    assert!(
        listed.contains(&created.as_str()),
        "expected listing to include the manual backup just created"
    );
}

#[tokio::test]
async fn restore_with_traversal_file_name_is_rejected() {
    let app = setup(seed_state()).await;

    let (status, body) = app
        .call(json_request(
            "POST",
            "/api/backups/restore",
            json!({ "backup_type": "manual", "file_name": "../evil" }),
        ))
        .await;

    assert!(
        status.is_client_error(),
        "traversal file name must be rejected with a 4xx, got {status}"
    );
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_ne!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(body["error"].is_string());
}

#[tokio::test]
async fn providers_lists_both_disconnected() {
    let app = setup(seed_state()).await;
    let (status, body) = app.call(get("/api/providers")).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body["spotify_redirect_uri"].is_string());
    let providers = body["providers"].as_array().expect("providers array");
    assert_eq!(providers.len(), 2);

    let mut by_key: BTreeMap<&str, bool> = BTreeMap::new();
    for provider in providers {
        by_key.insert(
            provider["key"].as_str().expect("provider key"),
            provider["connected"].as_bool().expect("connected flag"),
        );
    }
    assert_eq!(by_key.get("spotify"), Some(&false));
    assert_eq!(by_key.get("youtube-music"), Some(&false));
}

#[tokio::test]
async fn cross_origin_guard_blocks_foreign_origin() {
    let app = setup(seed_state()).await;

    let (status, _) = app
        .call(post_with_origin(
            "/api/backups/manual",
            Some("http://evil.com"),
        ))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cross_origin_guard_allows_loopback_and_missing_origin() {
    let app = setup(seed_state()).await;

    let (status, _) = app
        .call(post_with_origin(
            "/api/backups/manual",
            Some("http://127.0.0.1:7878"),
        ))
        .await;
    assert_ne!(status, StatusCode::FORBIDDEN);
    assert_eq!(status, StatusCode::OK);

    let (status, _) = app
        .call(post_with_origin("/api/backups/manual", None))
        .await;
    assert_ne!(status, StatusCode::FORBIDDEN);
    assert_eq!(status, StatusCode::OK);
}
