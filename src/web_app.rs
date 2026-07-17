use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::{Path as FsPath, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{delete, get, get_service, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use rand::{distributions::Alphanumeric, Rng};
use rusqlite::types::ValueRef;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, RwLock, Semaphore};
use tower_http::services::{ServeDir, ServeFile};
use url::Url;
use uuid::Uuid;

use crate::domain::{
    merge_provider_snapshot, LibraryState, LinkSource, PlaylistEntity, PlaylistEntry,
    ProviderConnection, ProviderConnectionConfig, ProviderCooldown, ProviderHealth, ProviderKind,
    ProviderTrackArtwork, SavedTrackEntry, SyncState, SyncStatusRecord, TrackEntity,
    TrackIdentityApplyResult, TrackIdentityConflict, TrackMergeConflictResolution, TrackMetadata,
    YoutubeMusicConnectionConfig,
};
use crate::matching::metadata_similarity;
use crate::provider::{ProgressHandler, ProviderCapability, ProviderProgress, StreamingProvider};
use crate::providers::policy;
use crate::providers::spotify::SpotifyProvider;
use crate::providers::youtube_music::YoutubeMusicProvider;
use crate::storage;

const PAGE_SIZE: usize = 50;
const RAW_PAGE_SIZE: usize = 120;
const BULK_IDENTITY_CONFLICT_EXAMPLE_LIMIT: usize = 10;
const BULK_IDENTITY_CONFLICT_MERGE_LIMIT: usize = 250;
/// How long a track that yielded no artwork is skipped before enrichment
/// retries it, to prevent refetch storms for artwork-less tracks.
const ARTWORK_NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
/// Upper bound on how many tracks a single background enrichment pass fetches,
/// so one browse request cannot schedule an unbounded run of external lookups.
const ARTWORK_ENRICHMENT_BATCH: usize = 50;
/// Concurrent external artwork fetches within one enrichment pass.
const ARTWORK_FETCH_CONCURRENCY: usize = 4;

struct AppContext {
    http_client: reqwest::Client,
    spotify_redirect_uri: String,
    pending_spotify_auth: Mutex<HashMap<String, PendingSpotifyAuth>>,
    operations: StdMutex<HashMap<String, OperationRecord>>,
    /// Canonical library state, held in memory and loaded once at startup.
    /// Browse/read handlers take a read lock; mutating handlers take the write
    /// lock, mutate, and persist write-through before dropping it. SQLite is
    /// never read again after startup except for raw-table browsing/restore.
    library: RwLock<LibraryState>,
    /// Bumped by every user-initiated canonical mutation (mutating handlers and
    /// delete-propagation reconciles). Long-running operations snapshot it
    /// before their off-lock provider I/O and re-check it at commit time to
    /// detect concurrent user edits. Artwork enrichment deliberately does not
    /// bump it: artwork is self-healing bookkeeping, not a user edit.
    library_version: AtomicU64,
    /// One-permit gate so at most one background artwork enrichment pass runs at
    /// a time (debounce): browse handlers try to schedule a pass and give up if
    /// one is already in flight.
    artwork_semaphore: Arc<Semaphore>,
    /// Track IDs that recently yielded no artwork, with the time we last tried,
    /// so enrichment does not hammer external services for artwork-less tracks.
    artwork_negative_cache: StdMutex<HashMap<String, Instant>>,
}

impl AppContext {
    /// Records a user-initiated canonical mutation so long-running operations
    /// can detect concurrent edits.
    fn bump_library_version(&self) {
        self.library_version.fetch_add(1, AtomicOrdering::SeqCst);
    }

    fn library_version(&self) -> u64 {
        self.library_version.load(AtomicOrdering::SeqCst)
    }
}

#[derive(Clone)]
struct PendingSpotifyAuth {
    client_id: String,
    client_secret: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum OperationStatus {
    Running,
    Succeeded,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum OperationKind {
    Verify,
    Pull,
    Push,
    ResetPush,
    Identity,
    IdentityAll,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct OperationRecord {
    id: String,
    provider: ProviderKind,
    kind: OperationKind,
    status: OperationStatus,
    stage: String,
    detail: Option<String>,
    saved_tracks_done: usize,
    saved_tracks_total: Option<usize>,
    playlists_done: usize,
    playlists_total: Option<usize>,
    playlist_entries_done: usize,
    playlist_entries_total: Option<usize>,
    message: Option<String>,
    warnings: Vec<String>,
    error: Option<String>,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    #[serde(skip)]
    last_persisted_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    database_path: String,
    integrity_check: String,
    tracks: usize,
    saved_tracks: usize,
    playlists: usize,
    playlist_entries: usize,
    durable_operation_history: bool,
}

#[derive(Serialize)]
struct OperationStartResponse {
    operation_id: String,
}

#[derive(Clone, Serialize)]
struct OperationResponse {
    operation_id: String,
    provider_key: String,
    provider_name: String,
    kind: OperationKind,
    status: OperationStatus,
    stage: String,
    detail: Option<String>,
    saved_tracks_done: usize,
    saved_tracks_total: Option<usize>,
    playlists_done: usize,
    playlists_total: Option<usize>,
    playlist_entries_done: usize,
    playlist_entries_total: Option<usize>,
    message: Option<String>,
    warnings: Vec<String>,
    error: Option<String>,
    started_at: String,
    finished_at: Option<String>,
}

#[derive(Clone, Copy, Debug)]
enum ApiErrorKind {
    BadRequest,
    NotFound,
    RateLimited,
    Internal,
}

#[derive(Debug)]
struct ApiError {
    kind: ApiErrorKind,
    message: String,
    /// The originating `anyhow` error, preserved so cooldown/health decisions
    /// can be made from the typed [`crate::error::ProviderError`] in its chain
    /// rather than from the display message.
    source: Option<anyhow::Error>,
}

#[derive(Serialize)]
struct ErrorPayload {
    error: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            kind: ApiErrorKind::BadRequest,
            message: message.into(),
            source: None,
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: ApiErrorKind::NotFound,
            message: message.into(),
            source: None,
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: ApiErrorKind::Internal,
            message: message.into(),
            source: None,
        }
    }

    fn rate_limited(message: impl Into<String>) -> Self {
        Self {
            kind: ApiErrorKind::RateLimited,
            message: message.into(),
            source: None,
        }
    }

    /// Attaches the originating `anyhow` error so downstream cooldown/health
    /// classification can inspect the typed provider failure it carries.
    fn with_source(mut self, source: anyhow::Error) -> Self {
        self.source = Some(source);
        self
    }

    fn status_code(&self) -> StatusCode {
        match self.kind {
            ApiErrorKind::BadRequest => StatusCode::BAD_REQUEST,
            ApiErrorKind::NotFound => StatusCode::NOT_FOUND,
            ApiErrorKind::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            ApiErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

fn sanitize_error_message(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "The provider returned an empty error.".to_string();
    }

    if looks_like_google_block_page(trimmed) {
        return "YouTube Music blocked this request with a Google anti-bot page (403). Relink YouTube Music with fresh browser headers and try again. If it keeps happening, wait a bit or retry from the same browser and network you used to capture the headers.".to_string();
    }

    if looks_like_html(trimmed) {
        let stripped = strip_html(trimmed);
        if stripped.is_empty() {
            return "The provider returned an HTML error page instead of an API response."
                .to_string();
        }
        return truncate_message(&stripped, 280);
    }

    truncate_message(trimmed, 400)
}

fn looks_like_google_block_page(raw: &str) -> bool {
    let lowercase = raw.to_ascii_lowercase();
    lowercase.contains("automated queries")
        || (lowercase.contains("server error 403")
            && lowercase.contains("<html")
            && lowercase.contains("google"))
}

fn looks_like_html(raw: &str) -> bool {
    let lowercase = raw.to_ascii_lowercase();
    lowercase.contains("<html")
        || lowercase.contains("<body")
        || lowercase.contains("<title")
        || lowercase.contains("<div")
}

fn strip_html(raw: &str) -> String {
    let mut plain = String::with_capacity(raw.len());
    let mut in_tag = false;

    for character in raw.chars() {
        match character {
            '<' => {
                in_tag = true;
                plain.push(' ');
            }
            '>' => in_tag = false,
            _ if !in_tag => plain.push(character),
            _ => {}
        }
    }

    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_message(raw: &str, max_chars: usize) -> String {
    let count = raw.chars().count();
    if count <= max_chars {
        return raw.to_string();
    }

    let shortened = raw
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    format!("{shortened}…")
}

async fn ensure_provider_not_cooling_down(provider: ProviderKind) -> Result<(), ApiError> {
    if let Some(cooldown) = runtime_db(move || storage::read_provider_cooldown(provider)).await? {
        return Err(ApiError::rate_limited(format!(
            "{} is cooling down until {} because the provider recently rejected requests: {}",
            provider.display_name(),
            cooldown.blocked_until.to_rfc3339(),
            cooldown.reason
        )));
    }
    Ok(())
}

async fn ensure_provider_health_allows_operation(provider: ProviderKind) -> Result<(), ApiError> {
    if let Some(health) = runtime_db(move || storage::read_provider_health(provider)).await? {
        if !health.ok {
            return Err(ApiError::bad_request(format!(
                "Last {} connection check failed: {}. Relink or run Check Connection before starting sync.",
                provider.display_name(),
                health
                    .message
                    .as_deref()
                    .unwrap_or("No detailed provider message was recorded.")
            )));
        }
    }
    Ok(())
}

async fn library_identity_skip_reason(provider: ProviderKind) -> Result<Option<String>, ApiError> {
    if runtime_db(move || storage::read_provider_connection(provider))
        .await?
        .is_none()
    {
        return Ok(Some(format!(
            "Skipped {} identity sync because the provider is not linked.",
            provider.display_name()
        )));
    }

    if let Some(cooldown) = runtime_db(move || storage::read_provider_cooldown(provider)).await? {
        return Ok(Some(format!(
            "Skipped {} identity sync because the provider is cooling down until {}: {}",
            provider.display_name(),
            cooldown.blocked_until.to_rfc3339(),
            cooldown.reason
        )));
    }

    if let Some(health) = runtime_db(move || storage::read_provider_health(provider)).await? {
        if !health.ok {
            return Ok(Some(format!(
                "Skipped {} identity sync because the last connection check failed: {}",
                provider.display_name(),
                health
                    .message
                    .as_deref()
                    .unwrap_or("No detailed provider message was recorded.")
            )));
        }
    }

    Ok(None)
}

fn provider_health_ok(provider: ProviderKind, message: impl Into<String>) -> ProviderHealth {
    ProviderHealth {
        provider,
        checked_at: Utc::now(),
        ok: true,
        message: Some(message.into()),
    }
}

fn provider_health_failed(provider: ProviderKind, message: impl Into<String>) -> ProviderHealth {
    ProviderHealth {
        provider,
        checked_at: Utc::now(),
        ok: false,
        message: Some(message.into()),
    }
}

async fn save_provider_health(health: ProviderHealth) -> Result<(), ApiError> {
    runtime_db(move || storage::save_provider_health(&health)).await
}

/// Records a cooldown and/or unhealthy state after an identity-sync provider
/// failure, classifying the failure from the typed provider error in the
/// `anyhow` chain. A missing source (e.g. a plain bad-request) records nothing.
async fn remember_identity_provider_failure(
    provider: ProviderKind,
    error: Option<&anyhow::Error>,
) -> Result<(), ApiError> {
    let Some(error) = error else {
        return Ok(());
    };

    if let Some(cooldown) = policy::cooldown_from_error(provider, error) {
        runtime_db(move || storage::save_provider_cooldown(&cooldown)).await?;
    }

    if policy::is_connection_health_failure(error) {
        save_provider_health(provider_health_failed(
            provider,
            sanitize_error_message(&error.to_string()),
        ))
        .await?;
    }

    Ok(())
}

fn looks_like_placeholder_ytmusic_cookie(cookie: &str) -> bool {
    let lowered = cookie.to_ascii_lowercase();
    lowered.contains("your_cookie_here") || lowered.contains("dummy") || lowered.contains("paste")
}

fn looks_like_placeholder_ytmusic_authuser(authuser: &str) -> bool {
    let lowered = authuser.trim().to_ascii_lowercase();
    lowered.is_empty() || lowered.contains("paste")
}

impl From<anyhow::Error> for ApiError {
    fn from(value: anyhow::Error) -> Self {
        Self {
            kind: ApiErrorKind::Internal,
            message: sanitize_error_message(&value.to_string()),
            source: Some(value),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status_code(),
            Json(ErrorPayload {
                error: self.message,
            }),
        )
            .into_response()
    }
}

#[derive(Default, Deserialize)]
struct SavedTracksQuery {
    q: Option<String>,
    page: Option<usize>,
}

#[derive(Default, Deserialize)]
struct TracksQuery {
    q: Option<String>,
    coverage: Option<String>,
    page: Option<usize>,
}

#[derive(Default, Deserialize)]
struct IdentityConflictsQuery {
    q: Option<String>,
    provider: Option<ProviderKind>,
    recommendation: Option<String>,
    impact: Option<String>,
    page: Option<usize>,
}

#[derive(Default, Deserialize)]
struct IdentityGapsQuery {
    provider: Option<ProviderKind>,
    q: Option<String>,
    page: Option<usize>,
}

#[derive(Default, Deserialize)]
struct PlaylistsQuery {
    q: Option<String>,
    page: Option<usize>,
}

#[derive(Default, Deserialize)]
struct RawTablesQuery {
    page: Option<usize>,
}

#[derive(Deserialize)]
struct UpdateTrackRequest {
    title: String,
    artists: Vec<String>,
    album: Option<String>,
    duration_seconds: Option<u32>,
    isrc: Option<String>,
}

#[derive(Deserialize)]
struct UpdatePlaylistRequest {
    name: String,
    description: Option<String>,
}

#[derive(Serialize)]
struct MessageResponse {
    message: String,
    warnings: Vec<String>,
}

impl MessageResponse {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            warnings: Vec::new(),
        }
    }

    fn with_warnings(message: impl Into<String>, warnings: Vec<String>) -> Self {
        Self {
            message: message.into(),
            warnings,
        }
    }
}

#[derive(Serialize)]
struct PageResponse<T> {
    items: Vec<T>,
    total: usize,
    page: usize,
    page_size: usize,
    total_pages: usize,
}

impl<T> PageResponse<T> {
    fn new(items: Vec<T>, total: usize, page: usize, page_size: usize) -> Self {
        Self {
            items,
            total,
            page,
            page_size,
            total_pages: total_pages(total, page_size),
        }
    }
}

#[derive(Serialize)]
struct OverviewResponse {
    library_updated_at: String,
    tracks: usize,
    saved_tracks: usize,
    playlists: usize,
    playlist_entries: usize,
    canonical_only: usize,
    multi_provider: usize,
    unmatched_tracks: usize,
    identity_conflicts: usize,
    provider_only_counts: Vec<ProviderOnlyCountDto>,
    provider_metrics: Vec<ProviderStatsDto>,
}

#[derive(Serialize)]
struct ProviderOnlyCountDto {
    key: String,
    name: String,
    count: usize,
}

#[derive(Serialize)]
struct ProviderStatsDto {
    key: String,
    name: String,
    linked_tracks: usize,
    missing_track_ids: usize,
    unmatched_tracks: usize,
    synced_saved_tracks: usize,
    pushable_saved_tracks: usize,
    saved_tracks_missing_identity: usize,
    unmatched_saved_tracks: usize,
    linked_playlists: usize,
    pushable_playlist_entries: usize,
    playlist_entries_missing_identity: usize,
    unmatched_playlist_entries: usize,
}

#[derive(Clone, Serialize)]
struct ProviderPreflightDto {
    can_pull: bool,
    can_push: bool,
    can_reset_push: bool,
    blockers: Vec<String>,
    reset_blockers: Vec<String>,
    warnings: Vec<String>,
    saved_tracks_total: usize,
    saved_tracks_pushable: usize,
    saved_tracks_missing_identity: usize,
    playlists_total: usize,
    linked_playlists: usize,
    playlist_entries_total: usize,
    playlist_entries_pushable: usize,
    playlist_entries_missing_identity: usize,
    track_ids_total: usize,
    track_ids_linked: usize,
    track_ids_missing: usize,
}

#[derive(Serialize)]
struct ProviderPushPlanDto {
    provider: String,
    provider_name: String,
    preflight: ProviderPreflightDto,
    saved_tracks: PushPlanSectionDto,
    playlist_entries: PushPlanSectionDto,
    playlists: PushPlaylistPlanSectionDto,
}

#[derive(Serialize)]
struct PushPlanSectionDto {
    total: usize,
    pushable: usize,
    skipped_missing_identity: usize,
    skipped_examples: Vec<ConflictTrackDto>,
}

#[derive(Serialize)]
struct PushPlaylistPlanSectionDto {
    total: usize,
    linked: usize,
    unlinked: usize,
    examples: Vec<PushPlaylistPlanItemDto>,
}

#[derive(Serialize)]
struct PushPlaylistPlanItemDto {
    playlist_id: String,
    name: String,
    entry_count: usize,
    linked: bool,
    missing_entries: usize,
}

#[derive(Clone, Serialize)]
struct ProviderBadgeDto {
    key: String,
    label: String,
    source: String,
    provider_id: String,
}

#[derive(Clone, Serialize)]
struct StatusPillDto {
    key: String,
    label: String,
    title: String,
}

#[derive(Clone, Serialize)]
struct CoverageDto {
    key: String,
    label: String,
    short_label: String,
}

#[derive(Clone, Serialize)]
struct SavedTrackItemDto {
    saved_track_id: String,
    track_id: String,
    title: String,
    artists: Vec<String>,
    artist_summary: String,
    album: Option<String>,
    subtitle: String,
    duration_seconds: Option<u32>,
    duration_label: String,
    isrc: Option<String>,
    added_at: Option<String>,
    added_label: String,
    coverage: CoverageDto,
    providers: Vec<ProviderBadgeDto>,
    status_pills: Vec<StatusPillDto>,
    artwork_url: Option<String>,
}

#[derive(Clone, Serialize)]
struct TrackListItemDto {
    track_id: String,
    title: String,
    artists: Vec<String>,
    artist_summary: String,
    album: Option<String>,
    subtitle: String,
    duration_seconds: Option<u32>,
    duration_label: String,
    isrc: Option<String>,
    coverage: CoverageDto,
    providers: Vec<ProviderBadgeDto>,
    status_pills: Vec<StatusPillDto>,
    saved_count: usize,
    playlist_refs: usize,
    artwork_url: Option<String>,
}

#[derive(Serialize)]
struct TrackDetailDto {
    track_id: String,
    title: String,
    artists: Vec<String>,
    artist_summary: String,
    album: Option<String>,
    duration_seconds: Option<u32>,
    duration_label: String,
    isrc: Option<String>,
    coverage: CoverageDto,
    providers: Vec<ProviderBadgeDto>,
    provider_status: Vec<ProviderStatusDetailDto>,
    identity_conflicts: Vec<TrackIdentityConflictDto>,
    saved_count: usize,
    playlist_refs: usize,
    artwork_url: Option<String>,
}

#[derive(Clone, Serialize)]
struct TrackIdentityConflictDto {
    provider: String,
    provider_name: String,
    provider_id: String,
    owner_track: ConflictTrackDto,
    conflicting_provider_links: Vec<ProviderLinkConflictDto>,
    evidence: TrackIdentityConflictEvidenceDto,
    message: String,
}

#[derive(Clone, Serialize)]
struct ConflictTrackDto {
    track_id: String,
    title: String,
    artist_summary: String,
    album: Option<String>,
    coverage: CoverageDto,
    providers: Vec<ProviderBadgeDto>,
    saved_count: usize,
    playlist_refs: usize,
    artwork_url: Option<String>,
}

#[derive(Clone, Serialize)]
struct ProviderLinkConflictDto {
    provider: String,
    provider_name: String,
    source_provider_id: String,
    target_provider_id: String,
}

#[derive(Clone, Serialize)]
struct TrackIdentityConflictEvidenceDto {
    provider_confidence: Option<f64>,
    metadata_similarity: f64,
    duration_delta_seconds: Option<u32>,
    source_saved_tracks: usize,
    source_playlist_entries: usize,
    candidate_saved_tracks: usize,
    candidate_playlist_entries: usize,
    recommendation: TrackIdentityConflictRecommendationDto,
}

#[derive(Clone, Serialize)]
struct TrackIdentityConflictRecommendationDto {
    key: String,
    label: String,
    detail: String,
}

#[derive(Clone, Serialize)]
struct TrackIdentityConflictQueueItemDto {
    source_track: ConflictTrackDto,
    conflict: TrackIdentityConflictDto,
}

#[derive(Clone, Serialize)]
struct TrackIdentityGapQueueItemDto {
    provider: String,
    provider_name: String,
    track: ConflictTrackDto,
    push_blocking: bool,
}

#[derive(Serialize)]
struct ProviderStatusDetailDto {
    provider: String,
    state: String,
    message: Option<String>,
    provider_item_id: Option<String>,
    confidence: Option<f64>,
    last_attempt_at: Option<String>,
    last_success_at: Option<String>,
    last_seen_at: Option<String>,
}

#[derive(Deserialize)]
struct ApplyTrackIdentityRequest {
    provider: ProviderKind,
    provider_id: String,
}

#[derive(Serialize)]
struct ApplyTrackIdentityResponse {
    message: String,
    result: String,
    provider: String,
    provider_id: String,
    track_id: String,
}

#[derive(Deserialize)]
struct MergeTrackRequest {
    target_track_id: String,
    conflict_resolution: MergeConflictResolutionChoice,
}

#[derive(Default, Deserialize)]
struct BulkMergeIdentityConflictsPlanQuery {
    q: Option<String>,
    provider: Option<ProviderKind>,
    impact: Option<String>,
}

#[derive(Deserialize)]
struct BulkMergeIdentityConflictsRequest {
    q: Option<String>,
    provider: Option<ProviderKind>,
    impact: Option<String>,
    conflict_resolution: MergeConflictResolutionChoice,
    max_merges: Option<usize>,
}

#[derive(Deserialize)]
struct RejectTrackIdentityConflictRequest {
    provider: ProviderKind,
    provider_id: String,
    owner_track_id: String,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MergeConflictResolutionChoice {
    KeepSource,
    KeepTarget,
}

#[derive(Serialize)]
struct MergeTrackResponse {
    message: String,
    source_track_id: String,
    target_track_id: String,
    resolved_conflicts: Vec<ResolvedProviderConflictDto>,
}

#[derive(Serialize)]
struct ResolvedProviderConflictDto {
    provider: String,
    provider_name: String,
    kept_provider_id: String,
    dropped_provider_id: String,
    kept_from_source: bool,
}

#[derive(Serialize)]
struct BulkMergeIdentityConflictsPlanDto {
    eligible_count: usize,
    examples: Vec<TrackIdentityConflictQueueItemDto>,
    warnings: Vec<String>,
}

#[derive(Serialize)]
struct BulkMergeIdentityConflictsResponse {
    message: String,
    eligible_count: usize,
    merged_count: usize,
    skipped_count: usize,
    resolved_provider_conflicts: usize,
    conflict_resolution: String,
    conflict_resolution_label: String,
    pre_merge_backup_path: String,
    merged_examples: Vec<BulkMergedIdentityConflictDto>,
    warnings: Vec<String>,
}

#[derive(Serialize)]
struct BulkMergedIdentityConflictDto {
    source_track_id: String,
    target_track_id: String,
    title: String,
    provider: String,
    provider_id: String,
    resolved_conflicts: Vec<ResolvedProviderConflictDto>,
}

#[derive(Clone, Serialize)]
struct PlaylistSummaryDto {
    playlist_id: String,
    name: String,
    description: Option<String>,
    entry_count: usize,
    providers: Vec<ProviderBadgeDto>,
    status_pills: Vec<StatusPillDto>,
    artwork_url: Option<String>,
}

#[derive(Clone, Serialize)]
struct PlaylistEntryDto {
    entry_id: String,
    track_id: String,
    title: String,
    artists: Vec<String>,
    artist_summary: String,
    album: Option<String>,
    subtitle: String,
    added_at: Option<String>,
    added_label: String,
    coverage: CoverageDto,
    providers: Vec<ProviderBadgeDto>,
    status_pills: Vec<StatusPillDto>,
    artwork_url: Option<String>,
}

#[derive(Serialize)]
struct PlaylistDetailDto {
    playlist: PlaylistSummaryDto,
    entries: Vec<PlaylistEntryDto>,
}

#[derive(Clone, Serialize)]
struct SchemaColumnDto {
    name: String,
    data_type: String,
}

#[derive(Clone, Serialize)]
struct SchemaTableDto {
    name: String,
    row_count: usize,
    columns: Vec<SchemaColumnDto>,
}

#[derive(Serialize)]
struct RawTableDto {
    name: String,
    columns: Vec<SchemaColumnDto>,
    rows: Vec<Vec<String>>,
    total_rows: usize,
    page: usize,
    page_size: usize,
    total_pages: usize,
}

#[derive(Serialize)]
struct BackupsResponse {
    automatic_backup_dir: String,
    manual_backup_dir: String,
    backups: Vec<BackupDto>,
}

#[derive(Serialize)]
struct CreateBackupResponse {
    message: String,
    backup: BackupDto,
}

#[derive(Deserialize)]
struct RestoreBackupRequest {
    backup_type: String,
    file_name: String,
}

#[derive(Serialize)]
struct RestoreBackupResponse {
    message: String,
    restored_backup: BackupDto,
    pre_restore_backup: BackupDto,
}

#[derive(Clone, Serialize)]
struct BackupDto {
    file_name: String,
    path: String,
    backup_type: String,
    size_bytes: u64,
    modified_at: Option<String>,
}

#[derive(Serialize)]
struct ProvidersResponse {
    spotify_redirect_uri: String,
    providers: Vec<ProviderConnectionDto>,
}

#[derive(Serialize)]
struct ProviderConnectionDto {
    key: String,
    name: String,
    connected: bool,
    connected_at: Option<String>,
    updated_at: Option<String>,
    health_checked_at: Option<String>,
    health_ok: Option<bool>,
    health_message: Option<String>,
    cooldown_until: Option<String>,
    cooldown_reason: Option<String>,
    preflight: ProviderPreflightDto,
}

#[derive(Deserialize)]
struct SpotifyConnectStartRequest {
    client_id: String,
    client_secret: String,
}

#[derive(Serialize)]
struct SpotifyConnectStartResponse {
    authorization_url: String,
}

#[derive(Deserialize)]
struct YoutubeMusicConnectRequest {
    headers_json: String,
}

#[derive(Deserialize)]
struct SpotifyCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct SpotifyOembedResponse {
    thumbnail_url: Option<String>,
    thumbnail_width: Option<u32>,
    thumbnail_height: Option<u32>,
}

/// Returns `true` if `host` (a `Host` header value, i.e. `host[:port]`) refers
/// to a loopback address we are willing to serve state-changing requests for.
fn is_loopback_host(host: &str) -> bool {
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
fn is_loopback_http_origin(origin: &str) -> bool {
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
fn is_request_origin_allowed(method: &Method, headers: &HeaderMap) -> bool {
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
async fn cross_origin_guard(request: Request, next: Next) -> Response {
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

    let frontend_dist = ensure_frontend_dist()?;
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
    println!("Frontend bundle: {}", frontend_dist.display());
    println!("Press Ctrl+C to stop the server.");

    if open_browser {
        match open::that(&ui_url) {
            Ok(_) => println!("Opened the library web app in your browser."),
            Err(error) => println!("Could not open a browser automatically: {error}"),
        }
    } else {
        println!("Open this URL manually in your browser: {ui_url}");
    }

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
        .route("/providers/:provider/export", post(api_provider_export))
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
        .route("/providers/:provider/sync", post(api_provider_sync))
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
        .route("/database/schema", get(api_schema))
        .route("/database/tables/:table_name", get(api_raw_table))
        .route("/backups", get(api_backups))
        .route("/backups/manual", post(api_create_manual_backup))
        .route("/backups/restore", post(api_restore_backup))
        .layer(middleware::from_fn(cross_origin_guard));

    let frontend_service = get_service(
        ServeDir::new(&frontend_dist)
            .append_index_html_on_directories(true)
            .fallback(ServeFile::new(frontend_dist.join("index.html"))),
    );

    let app = Router::new()
        .route("/", get(root_redirect))
        .route("/app", get(app_redirect))
        .route("/auth/spotify/callback", get(spotify_callback))
        .nest("/api", api)
        .nest_service("/app/", frontend_service)
        .with_state(shared);

    axum::serve(listener, app).await?;
    Ok(())
}

async fn root_redirect() -> Redirect {
    Redirect::to("/app/")
}

async fn app_redirect() -> Redirect {
    Redirect::to("/app/")
}

async fn spotify_callback(
    State(context): State<Arc<AppContext>>,
    Query(query): Query<SpotifyCallbackQuery>,
) -> Result<Response, ApiError> {
    if let Some(error) = query.error {
        return Ok(Redirect::to(&app_notice_redirect(Some(format!(
            "Spotify connection failed: {error}"
        ))))
        .into_response());
    }

    let code = query
        .code
        .ok_or_else(|| ApiError::bad_request("Spotify callback did not include a code."))?;
    let state = query
        .state
        .ok_or_else(|| ApiError::bad_request("Spotify callback did not include a state."))?;

    let pending = context
        .pending_spotify_auth
        .lock()
        .await
        .remove(&state)
        .ok_or_else(|| ApiError::bad_request("Spotify authorization state expired."))?;

    let config = SpotifyProvider::exchange_authorization_code(
        &pending.client_id,
        &pending.client_secret,
        &context.spotify_redirect_uri,
        &code,
    )
    .await
    .map_err(ApiError::from)?;

    let spotify =
        match SpotifyProvider::from_connection(&config, ProviderCapability::ReadWrite).await {
            Ok(provider) => provider,
            Err(error) => {
                return Ok(Redirect::to(&app_notice_redirect(Some(format!(
                    "Spotify link failed: {}",
                    sanitize_error_message(&error.to_string())
                ))))
                .into_response())
            }
        };

    if let Err(error) = spotify.verify_connection().await {
        return Ok(Redirect::to(&app_notice_redirect(Some(format!(
            "Spotify link failed: {}",
            sanitize_error_message(&error.to_string())
        ))))
        .into_response());
    }

    let now = Utc::now();
    runtime_db(move || {
        storage::save_provider_connection(&ProviderConnection {
            provider: ProviderKind::Spotify,
            connected_at: now,
            updated_at: now,
            config: ProviderConnectionConfig::Spotify(config),
        })?;
        storage::clear_provider_cooldown(ProviderKind::Spotify)
    })
    .await?;
    save_provider_health(provider_health_ok(
        ProviderKind::Spotify,
        "Connection verified during Spotify link.",
    ))
    .await?;

    Ok(Html(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta http-equiv="refresh" content="0; url=/app/overview?notice=Spotify%20connected">
    <title>Spotify Connected</title>
  </head>
  <body style="font-family: system-ui, sans-serif; background: #0d0d12; color: #f6f4ef; display:grid; place-items:center; min-height:100vh;">
    <p>Spotify connected. Returning to the app…</p>
  </body>
</html>"#
            .to_string(),
    )
    .into_response())
}

async fn api_overview(
    State(context): State<Arc<AppContext>>,
) -> Result<Json<OverviewResponse>, ApiError> {
    let state = context.library.read().await;
    Ok(Json(overview_payload(&state)))
}

async fn api_health() -> Result<Json<HealthResponse>, ApiError> {
    let health = tokio::task::spawn_blocking(storage::database_health)
        .await
        .context("Failed to join database health task")?
        .map_err(ApiError::from)?;
    let status = if health.integrity_check == "ok" {
        "ok"
    } else {
        "degraded"
    };
    Ok(Json(HealthResponse {
        status,
        database_path: health.path.display().to_string(),
        integrity_check: health.integrity_check,
        tracks: health.tracks,
        saved_tracks: health.saved_tracks,
        playlists: health.playlists,
        playlist_entries: health.playlist_entries,
        durable_operation_history: true,
    }))
}

async fn api_backups() -> Result<Json<BackupsResponse>, ApiError> {
    let backups = tokio::task::spawn_blocking(storage::list_library_backups)
        .await
        .context("Failed to join backup listing task")?
        .map_err(ApiError::from)?;
    Ok(Json(BackupsResponse {
        automatic_backup_dir: storage::automatic_backup_dir().display().to_string(),
        manual_backup_dir: storage::manual_backup_dir().display().to_string(),
        backups: backups.into_iter().map(backup_dto).collect(),
    }))
}

async fn api_create_manual_backup() -> Result<Json<CreateBackupResponse>, ApiError> {
    let backup = tokio::task::spawn_blocking(storage::create_manual_library_backup)
        .await
        .context("Failed to join manual backup task")?
        .map_err(ApiError::from)?;
    Ok(Json(CreateBackupResponse {
        message: format!(
            "Created manual source-of-truth backup at {}.",
            backup.path.display()
        ),
        backup: backup_dto(backup),
    }))
}

async fn api_restore_backup(
    State(context): State<Arc<AppContext>>,
    Json(request): Json<RestoreBackupRequest>,
) -> Result<Json<RestoreBackupResponse>, ApiError> {
    let backup_type = request.backup_type;
    let file_name = request.file_name;
    // A restore replaces the entire canonical database, so it must exclude all
    // other access: hold the write lock across the (local, blocking) restore and
    // the reload that refreshes the in-memory copy from the restored file.
    let mut guard = context.library.write().await;
    let summary = tokio::task::spawn_blocking(move || {
        storage::restore_library_backup(&backup_type, &file_name)
    })
    .await
    .context("Failed to join restore backup task")?
    .map_err(|error| ApiError::bad_request(sanitize_error_message(&error.to_string())))?;
    let reloaded = tokio::task::spawn_blocking(storage::read_library_state)
        .await
        .context("Failed to join post-restore reload task")?
        .map_err(ApiError::from)?;
    *guard = reloaded;
    context.bump_library_version();
    drop(guard);

    Ok(Json(RestoreBackupResponse {
        message: format!(
            "Restored {} from backup. A pre-restore manual backup was saved at {}.",
            storage::library_state_path().display(),
            summary.pre_restore_backup.path.display()
        ),
        restored_backup: backup_dto(summary.restored_backup),
        pre_restore_backup: backup_dto(summary.pre_restore_backup),
    }))
}

async fn api_providers(
    State(context): State<Arc<AppContext>>,
) -> Result<Json<ProvidersResponse>, ApiError> {
    // Gather the runtime-database rows off-lock first, then read the in-memory
    // state without holding it across any await.
    let connections = read_provider_connections().await?;
    let cooldowns = runtime_db(storage::list_provider_cooldowns).await?;
    let healths = runtime_db(storage::list_provider_healths).await?;
    let state = context.library.read().await;
    Ok(Json(ProvidersResponse {
        spotify_redirect_uri: context.spotify_redirect_uri.clone(),
        providers: provider_connection_payloads(&state, &connections, &cooldowns, &healths),
    }))
}

async fn api_provider_preflight(
    State(context): State<Arc<AppContext>>,
    Path(provider): Path<ProviderKind>,
) -> Result<Json<ProviderPreflightDto>, ApiError> {
    let connections = read_provider_connections().await?;
    let cooldowns = runtime_db(storage::list_provider_cooldowns).await?;
    let healths = runtime_db(storage::list_provider_healths).await?;
    let connection = connections
        .iter()
        .find(|connection| connection.provider == provider);
    let cooldown = cooldowns
        .iter()
        .find(|cooldown| cooldown.provider == provider);
    let health = healths.iter().find(|health| health.provider == provider);

    let state = context.library.read().await;
    let identity_conflicts = identity_conflict_rows(&state, None).len();
    Ok(Json(provider_preflight_payload(
        &state,
        provider,
        connection,
        cooldown,
        health,
        identity_conflicts,
    )))
}

async fn api_provider_push_plan(
    State(context): State<Arc<AppContext>>,
    Path(provider): Path<ProviderKind>,
) -> Result<Json<ProviderPushPlanDto>, ApiError> {
    let connections = read_provider_connections().await?;
    let cooldowns = runtime_db(storage::list_provider_cooldowns).await?;
    let healths = runtime_db(storage::list_provider_healths).await?;
    let connection = connections
        .iter()
        .find(|connection| connection.provider == provider);
    let cooldown = cooldowns
        .iter()
        .find(|cooldown| cooldown.provider == provider);
    let health = healths.iter().find(|health| health.provider == provider);

    let state = context.library.read().await;
    Ok(Json(provider_push_plan_payload(
        &state, provider, connection, cooldown, health,
    )))
}

async fn api_start_provider_verify(
    State(context): State<Arc<AppContext>>,
    Path(provider): Path<ProviderKind>,
) -> Result<Json<OperationStartResponse>, ApiError> {
    let operation_id = Uuid::new_v4().to_string();
    insert_operation(
        &context,
        OperationRecord {
            id: operation_id.clone(),
            provider,
            kind: OperationKind::Verify,
            status: OperationStatus::Running,
            stage: "Checking connection".to_string(),
            detail: Some(provider.display_name().to_string()),
            saved_tracks_done: 0,
            saved_tracks_total: None,
            playlists_done: 0,
            playlists_total: None,
            playlist_entries_done: 0,
            playlist_entries_total: None,
            message: None,
            warnings: Vec::new(),
            error: None,
            started_at: Utc::now(),
            finished_at: None,
            last_persisted_at: None,
        },
    )
    .await?;

    let background_context = context.clone();
    let background_operation_id = operation_id.clone();
    tokio::spawn(async move {
        let result = async {
            let provider_client =
                build_connected_provider_allowing_failed_health(provider, ProviderCapability::Read)
                    .await?;
            match provider_client.verify_connection().await {
                Ok(()) => {
                    save_provider_health(provider_health_ok(
                        provider,
                        "Connection check succeeded.",
                    ))
                    .await?;
                    runtime_db(move || storage::clear_provider_cooldown(provider)).await?;
                    Ok::<MessageResponse, ApiError>(MessageResponse::new(format!(
                        "{} connection check succeeded.",
                        provider.display_name()
                    )))
                }
                Err(error) => {
                    let message = sanitize_error_message(&error.to_string());
                    save_provider_health(provider_health_failed(provider, message.clone())).await?;
                    Err(ApiError::bad_request(message).with_source(error))
                }
            }
        }
        .await;
        finish_operation(background_context, &background_operation_id, result).await;
    });

    Ok(Json(OperationStartResponse { operation_id }))
}

async fn api_start_spotify_connect(
    State(context): State<Arc<AppContext>>,
    Json(request): Json<SpotifyConnectStartRequest>,
) -> Result<Json<SpotifyConnectStartResponse>, ApiError> {
    let client_id = request.client_id.trim();
    let client_secret = request.client_secret.trim();
    if client_id.is_empty() || client_secret.is_empty() {
        return Err(ApiError::bad_request(
            "Spotify client ID and client secret are both required.",
        ));
    }

    let state = random_state();
    let redirect_uri = url::Url::parse(&context.spotify_redirect_uri)
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let authorization_url = SpotifyProvider::authorization_url(
        ProviderCapability::ReadWrite,
        client_id,
        &redirect_uri,
        &state,
    )
    .map_err(ApiError::from)?;

    context.pending_spotify_auth.lock().await.insert(
        state,
        PendingSpotifyAuth {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
        },
    );

    Ok(Json(SpotifyConnectStartResponse {
        authorization_url: authorization_url.to_string(),
    }))
}

async fn api_connect_youtube_music(
    Json(request): Json<YoutubeMusicConnectRequest>,
) -> Result<Json<MessageResponse>, ApiError> {
    let auth = ytmusicapi::BrowserAuth::from_json(request.headers_json.trim())
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let config = YoutubeMusicConnectionConfig {
        cookie: auth.cookie,
        x_goog_authuser: auth.x_goog_authuser,
        origin: Some(auth.origin),
    };
    if looks_like_placeholder_ytmusic_cookie(&config.cookie) {
        return Err(ApiError::bad_request(
            "Replace the sample YouTube Music cookie values with real browser headers before linking.",
        ));
    }
    if looks_like_placeholder_ytmusic_authuser(&config.x_goog_authuser) {
        return Err(ApiError::bad_request(
            "Replace the sample x-goog-authuser value with the exact account index from your browser request before linking.",
        ));
    }
    let provider = YoutubeMusicProvider::from_connection(&config)
        .map_err(|error| ApiError::bad_request(sanitize_error_message(&error.to_string())))?;
    provider
        .verify_connection()
        .await
        .map_err(|error| ApiError::bad_request(sanitize_error_message(&error.to_string())))?;

    let now = Utc::now();
    runtime_db(move || {
        storage::save_provider_connection(&ProviderConnection {
            provider: ProviderKind::YoutubeMusic,
            connected_at: now,
            updated_at: now,
            config: ProviderConnectionConfig::YoutubeMusic(config),
        })?;
        storage::clear_provider_cooldown(ProviderKind::YoutubeMusic)
    })
    .await?;
    save_provider_health(provider_health_ok(
        ProviderKind::YoutubeMusic,
        "Connection verified during YouTube Music link.",
    ))
    .await?;

    Ok(Json(MessageResponse::new(
        "Linked YouTube Music. Test call succeeded.",
    )))
}

async fn api_disconnect_provider(
    Path(provider): Path<ProviderKind>,
) -> Result<Json<MessageResponse>, ApiError> {
    runtime_db(move || storage::delete_provider_connection(provider)).await?;
    Ok(Json(MessageResponse::new(format!(
        "Disconnected {} from the app.",
        provider.display_name()
    ))))
}

async fn api_start_provider_export(
    State(context): State<Arc<AppContext>>,
    Path(provider): Path<ProviderKind>,
) -> Result<Json<OperationStartResponse>, ApiError> {
    let operation_id = Uuid::new_v4().to_string();
    insert_operation(
        &context,
        OperationRecord {
            id: operation_id.clone(),
            provider,
            kind: OperationKind::Pull,
            status: OperationStatus::Running,
            stage: "Starting pull".to_string(),
            detail: None,
            saved_tracks_done: 0,
            saved_tracks_total: None,
            playlists_done: 0,
            playlists_total: None,
            playlist_entries_done: 0,
            playlist_entries_total: None,
            message: None,
            warnings: Vec::new(),
            error: None,
            started_at: Utc::now(),
            finished_at: None,
            last_persisted_at: None,
        },
    )
    .await?;

    let background_context = context.clone();
    let background_operation_id = operation_id.clone();
    tokio::spawn(async move {
        let progress = progress_handler_for_operation(
            background_context.clone(),
            background_operation_id.clone(),
        );
        let result = async {
            let provider_client =
                build_connected_provider(provider, ProviderCapability::Read).await?;
            // Provider network I/O runs with no library lock held; the snapshot
            // is merged onto the current in-memory state afterwards.
            let snapshot = provider_client
                .export_library_with_progress(Some(progress))
                .await
                .map_err(ApiError::from)?;
            let summary = {
                let mut guard = background_context.library.write().await;
                let summary = merge_provider_snapshot(&mut guard, snapshot);
                persist_library(&guard).await?;
                summary
            };
            Ok::<MessageResponse, ApiError>(MessageResponse::with_warnings(
                format!(
                    "Merged {} saved tracks and {} playlists from {}.",
                    summary.saved_tracks_seen,
                    summary.playlists_seen,
                    provider.display_name()
                ),
                summary.warnings,
            ))
        }
        .await;
        finish_operation(background_context, &background_operation_id, result).await;
    });

    Ok(Json(OperationStartResponse { operation_id }))
}

async fn api_provider_export(
    State(context): State<Arc<AppContext>>,
    Path(provider): Path<ProviderKind>,
) -> Result<Json<MessageResponse>, ApiError> {
    let provider_client = build_connected_provider(provider, ProviderCapability::Read).await?;
    // Export network I/O runs off-lock; merge onto current state under the write
    // lock (the merge is incremental, so concurrent user edits survive).
    let snapshot = provider_client
        .export_library()
        .await
        .map_err(ApiError::from)?;
    let summary = {
        let mut guard = context.library.write().await;
        let summary = merge_provider_snapshot(&mut guard, snapshot);
        persist_library(&guard).await?;
        summary
    };

    Ok(Json(MessageResponse::with_warnings(
        format!(
            "Merged {} saved tracks and {} playlists from {} into the canonical database.",
            summary.saved_tracks_seen,
            summary.playlists_seen,
            provider.display_name()
        ),
        summary.warnings,
    )))
}

async fn api_start_provider_identity(
    State(context): State<Arc<AppContext>>,
    Path(provider): Path<ProviderKind>,
) -> Result<Json<OperationStartResponse>, ApiError> {
    let operation_id = Uuid::new_v4().to_string();
    insert_operation(
        &context,
        OperationRecord {
            id: operation_id.clone(),
            provider,
            kind: OperationKind::Identity,
            status: OperationStatus::Running,
            stage: "Starting identity sync".to_string(),
            detail: None,
            saved_tracks_done: 0,
            saved_tracks_total: None,
            playlists_done: 0,
            playlists_total: None,
            playlist_entries_done: 0,
            playlist_entries_total: None,
            message: None,
            warnings: Vec::new(),
            error: None,
            started_at: Utc::now(),
            finished_at: None,
            last_persisted_at: None,
        },
    )
    .await?;

    let background_context = context.clone();
    let background_operation_id = operation_id.clone();
    tokio::spawn(async move {
        let progress = progress_handler_for_operation(
            background_context.clone(),
            background_operation_id.clone(),
        );
        let result = async {
            let provider_client =
                build_connected_provider(provider, ProviderCapability::Read).await?;
            // Snapshot the state and its version under a brief read lock, then
            // run the provider identity I/O on the clone with no lock held.
            let (mut working, base_version) = {
                let guard = background_context.library.read().await;
                (guard.clone(), background_context.library_version())
            };
            let summary = crate::identity::reconcile_provider_identities(
                provider_client.as_ref(),
                &mut working,
                Some(progress),
            )
            .await
            .map_err(ApiError::from)?;
            let deferred =
                summary.unprocessed_due_rate_limit + summary.unprocessed_due_safety_limit;
            let concurrent_warning =
                commit_identity_results(&background_context, working, base_version).await?;
            let mut warnings = summary.warnings;
            warnings.extend(concurrent_warning);
            Ok::<MessageResponse, ApiError>(MessageResponse::with_warnings(
                format!(
                    "{} identity sync performed {} provider identity lookups, added {} links, merged {} duplicate track rows, skipped {} merge conflicts, flagged {} invalid metadata rows, removed {} duplicate saved rows, left {} unmatched, and deferred {} tracks for a later run.",
                    provider.display_name(),
                    summary.provider_searches,
                    summary.provider_links_added,
                    summary.tracks_merged,
                    summary.merge_conflicts,
                    summary.invalid_metadata,
                    summary.duplicate_saved_tracks_removed,
                    summary.unmatched,
                    deferred
                ),
                warnings,
            ))
        }
        .await;
        finish_operation(background_context, &background_operation_id, result).await;
    });

    Ok(Json(OperationStartResponse { operation_id }))
}

/// Commits the results of an identity run that mutated `working` (a clone taken
/// while `base_version` was current) back into the canonical state.
///
/// Identity's mutations (track merges, duplicate removals, conflict tombstones,
/// link additions) reshape the library across dimensions and cannot be cleanly
/// re-applied item-by-item onto a concurrently edited state. So this uses an
/// optimistic version check: if no user mutation happened while the provider I/O
/// ran (`library_version` unchanged), the current state still equals the clone's
/// base and committing the clone wholesale is exactly correct. If a user edit
/// did land, clobbering it would lose data, so the network-derived identity
/// results are discarded; only the cheap, safe, non-network duplicate-saved
/// consolidation is re-run against the *current* state, and a warning tells the
/// user to re-run identity. This never loses a user edit and never corrupts
/// state, at the cost of an occasional identity re-run in the rare
/// edited-during-sync case. Returns an extra warning to surface, if any.
async fn commit_identity_results(
    context: &Arc<AppContext>,
    working: LibraryState,
    base_version: u64,
) -> Result<Option<String>, ApiError> {
    let mut guard = context.library.write().await;
    if context.library_version() == base_version {
        *guard = working;
        persist_library(&guard).await?;
        Ok(None)
    } else {
        if guard.consolidate_duplicate_saved_tracks() > 0 {
            persist_library(&guard).await?;
        }
        Ok(Some(
            "Detected library edits made while identity sync was running, so the provider identity results were not applied to avoid overwriting your changes. Re-run identity sync.".to_string(),
        ))
    }
}

async fn api_start_library_identity(
    State(context): State<Arc<AppContext>>,
) -> Result<Json<OperationStartResponse>, ApiError> {
    let operation_id = Uuid::new_v4().to_string();
    insert_operation(
        &context,
        OperationRecord {
            id: operation_id.clone(),
            provider: ProviderKind::Spotify,
            kind: OperationKind::IdentityAll,
            status: OperationStatus::Running,
            stage: "Starting library identity sync".to_string(),
            detail: None,
            saved_tracks_done: 0,
            saved_tracks_total: None,
            playlists_done: 0,
            playlists_total: None,
            playlist_entries_done: 0,
            playlist_entries_total: None,
            message: None,
            warnings: Vec::new(),
            error: None,
            started_at: Utc::now(),
            finished_at: None,
            last_persisted_at: None,
        },
    )
    .await?;

    let background_context = context.clone();
    let background_operation_id = operation_id.clone();
    tokio::spawn(async move {
        let progress = progress_handler_for_operation(
            background_context.clone(),
            background_operation_id.clone(),
        );
        let result = async {
            // Snapshot state + version once, then run every provider's identity
            // I/O against the clone with no lock held. Results are committed once
            // at the end via the same version-checked path as a single provider.
            let (mut working, base_version) = {
                let guard = background_context.library.read().await;
                (guard.clone(), background_context.library_version())
            };
            let mut warnings = Vec::new();
            let mut providers_ran = 0_usize;
            let mut links_added = 0_usize;
            let mut tracks_merged = 0_usize;
            let mut merge_conflicts = 0_usize;
            let mut invalid_metadata = 0_usize;
            let mut duplicate_saved_removed = 0_usize;
            let mut unmatched = 0_usize;
            let mut provider_searches = 0_usize;
            let mut deferred = 0_usize;

            for provider in ProviderKind::all().iter().copied() {
                progress(ProviderProgress {
                    stage: format!("Preparing {} identity sync", provider.display_name()),
                    detail: Some("Checking connection, health, and cooldown state.".to_string()),
                    saved_tracks_done: 0,
                    saved_tracks_total: Some(working.tracks.len()),
                    ..Default::default()
                });

                if let Some(reason) = library_identity_skip_reason(provider).await? {
                    warnings.push(reason);
                    continue;
                }

                let provider_client =
                    match build_connected_provider(provider, ProviderCapability::Read).await {
                        Ok(provider_client) => provider_client,
                        Err(error) => {
                            let message = sanitize_error_message(&error.message);
                            remember_identity_provider_failure(provider, error.source.as_ref())
                                .await?;
                            warnings.push(format!(
                                "Skipped {} identity sync: {}",
                                provider.display_name(),
                                message
                            ));
                            continue;
                        }
                    };

                progress(ProviderProgress {
                    stage: format!("Resolving {} identities", provider.display_name()),
                    detail: Some("Searching provider catalog for missing canonical IDs.".to_string()),
                    saved_tracks_done: 0,
                    saved_tracks_total: Some(working.tracks.len()),
                    ..Default::default()
                });

                let summary = match crate::identity::reconcile_provider_identities(
                    provider_client.as_ref(),
                    &mut working,
                    Some(progress.clone()),
                )
                .await
                {
                    Ok(summary) => summary,
                    Err(error) => {
                        let message = sanitize_error_message(&error.to_string());
                        remember_identity_provider_failure(provider, Some(&error)).await?;
                        warnings.push(format!(
                            "{} identity sync stopped early: {}",
                            provider.display_name(),
                            message
                        ));
                        continue;
                    }
                };

                providers_ran += 1;
                links_added += summary.provider_links_added;
                tracks_merged += summary.tracks_merged;
                merge_conflicts += summary.merge_conflicts;
                invalid_metadata += summary.invalid_metadata;
                duplicate_saved_removed += summary.duplicate_saved_tracks_removed;
                unmatched += summary.unmatched;
                provider_searches += summary.provider_searches;
                deferred +=
                    summary.unprocessed_due_rate_limit + summary.unprocessed_due_safety_limit;
                warnings.extend(summary.warnings);
                runtime_db(move || storage::clear_provider_cooldown(provider)).await?;
                save_provider_health(provider_health_ok(
                    provider,
                    format!("{} identity sync succeeded.", provider.display_name()),
                ))
                .await?;
            }

            let track_total = working.tracks.len();
            progress(ProviderProgress {
                stage: "Library identity sync complete".to_string(),
                saved_tracks_done: track_total,
                saved_tracks_total: Some(track_total),
                ..Default::default()
            });

            let concurrent_warning =
                commit_identity_results(&background_context, working, base_version).await?;
            warnings.extend(concurrent_warning);

            let message = if providers_ran == 0 {
                "Library identity sync did not run against any provider.".to_string()
            } else {
                format!(
                    "Library identity sync ran against {providers_ran} provider(s), performed {provider_searches} provider identity lookups, added {links_added} links, merged {tracks_merged} duplicate track rows, skipped {merge_conflicts} merge conflicts, flagged {invalid_metadata} invalid metadata rows, removed {duplicate_saved_removed} duplicate saved rows, left {unmatched} unmatched, and deferred {deferred} tracks for a later run."
                )
            };

            Ok::<MessageResponse, ApiError>(MessageResponse::with_warnings(
                message, warnings,
            ))
        }
        .await;
        finish_operation(background_context, &background_operation_id, result).await;
    });

    Ok(Json(OperationStartResponse { operation_id }))
}

async fn api_start_provider_sync(
    State(context): State<Arc<AppContext>>,
    Path(provider): Path<ProviderKind>,
) -> Result<Json<OperationStartResponse>, ApiError> {
    start_provider_sync_operation(context, provider, false).await
}

async fn api_start_provider_reset_sync(
    State(context): State<Arc<AppContext>>,
    Path(provider): Path<ProviderKind>,
) -> Result<Json<OperationStartResponse>, ApiError> {
    if !provider.supports_library_reset() {
        return Err(ApiError::bad_request(format!(
            "{} does not support account-wide library reset in this app.",
            provider.display_name()
        )));
    }
    start_provider_sync_operation(context, provider, true).await
}

async fn start_provider_sync_operation(
    context: Arc<AppContext>,
    provider: ProviderKind,
    reset_destination: bool,
) -> Result<Json<OperationStartResponse>, ApiError> {
    let operation_id = Uuid::new_v4().to_string();
    insert_operation(
        &context,
        OperationRecord {
            id: operation_id.clone(),
            provider,
            kind: if reset_destination {
                OperationKind::ResetPush
            } else {
                OperationKind::Push
            },
            status: OperationStatus::Running,
            stage: if reset_destination {
                "Starting reset and push".to_string()
            } else {
                "Starting push".to_string()
            },
            detail: None,
            saved_tracks_done: 0,
            saved_tracks_total: None,
            playlists_done: 0,
            playlists_total: None,
            playlist_entries_done: 0,
            playlist_entries_total: None,
            message: None,
            warnings: Vec::new(),
            error: None,
            started_at: Utc::now(),
            finished_at: None,
            last_persisted_at: None,
        },
    )
    .await?;

    let background_context = context.clone();
    let background_operation_id = operation_id.clone();
    tokio::spawn(async move {
        let progress = progress_handler_for_operation(
            background_context.clone(),
            background_operation_id.clone(),
        );
        let result = async {
            let provider_client = build_connected_provider(
                provider,
                if reset_destination {
                    ProviderCapability::ReadWrite
                } else {
                    ProviderCapability::Write
                },
            )
            .await?;
            let purge_report = if reset_destination {
                progress(ProviderProgress {
                    stage: "Resetting destination".to_string(),
                    detail: Some(format!(
                        "Removing saved tracks and playlists from {} before push.",
                        provider.display_name()
                    )),
                    ..Default::default()
                });
                Some(
                    provider_client
                        .purge_library(true)
                        .await
                        .map_err(ApiError::from)?,
                )
            } else {
                None
            };
            // Snapshot the state (clearing this provider's playlist links for a
            // reset), run the push against the clone with no lock held, then
            // reconcile the provider-scoped results back into the current state.
            let mut working = {
                let guard = background_context.library.read().await;
                guard.clone()
            };
            if reset_destination {
                working.clear_playlist_provider_state(provider);
            }
            let sync_result = provider_client
                .sync_library_with_progress(&mut working, true, Some(progress.clone()))
                .await;
            {
                let mut guard = background_context.library.write().await;
                reapply_provider_sync(&mut guard, &working, provider);
                persist_library(&guard).await?;
            }
            let summary = sync_result.map_err(ApiError::from)?;
            let reset_prefix = purge_report
                .map(|report| {
                    format!(
                        "Reset removed {} saved tracks and {} playlists. ",
                        report.saved_tracks, report.playlists
                    )
                })
                .unwrap_or_default();
            Ok::<MessageResponse, ApiError>(MessageResponse::with_warnings(
                format!(
                    "{}Synced {} saved tracks and {} playlist entries into {}.",
                    reset_prefix,
                    summary.saved_tracks_synced,
                    summary.playlist_entries_synced,
                    provider.display_name()
                ),
                summary.warnings,
            ))
        }
        .await;
        finish_operation(background_context, &background_operation_id, result).await;
    });

    Ok(Json(OperationStartResponse { operation_id }))
}

async fn api_provider_sync(
    State(context): State<Arc<AppContext>>,
    Path(provider): Path<ProviderKind>,
) -> Result<Json<MessageResponse>, ApiError> {
    let provider_client = build_connected_provider(provider, ProviderCapability::Write).await?;
    let mut working = {
        let guard = context.library.read().await;
        guard.clone()
    };
    let sync_result = provider_client.sync_library(&mut working, true).await;
    {
        let mut guard = context.library.write().await;
        reapply_provider_sync(&mut guard, &working, provider);
        persist_library(&guard).await?;
    }
    let summary = sync_result.map_err(ApiError::from)?;
    Ok(Json(MessageResponse::with_warnings(
        format!(
            "Synced {} saved tracks and {} playlist entries into {}.",
            summary.saved_tracks_synced,
            summary.playlist_entries_synced,
            provider.display_name()
        ),
        summary.warnings,
    )))
}

async fn api_operation(
    State(context): State<Arc<AppContext>>,
    Path(operation_id): Path<String>,
) -> Result<Json<OperationResponse>, ApiError> {
    let operation = {
        let operations = context
            .operations
            .lock()
            .map_err(|_| ApiError::internal("Failed to read operation state."))?;
        operations.get(&operation_id).cloned()
    };
    let operation = match operation {
        Some(operation) => operation,
        None => tokio::task::spawn_blocking({
            let operation_id = operation_id.clone();
            move || storage::read_ui_operation_json(&operation_id)
        })
        .await
        .context("Failed to join operation history read task")?
        .map_err(ApiError::from)?
        .map(|json| serde_json::from_str::<OperationRecord>(&json))
        .transpose()
        .map_err(|error| ApiError::internal(error.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("Unknown operation '{operation_id}'.")))?,
    };
    Ok(Json(operation_response(&operation)))
}

async fn api_saved_tracks(
    State(context): State<Arc<AppContext>>,
    Query(query): Query<SavedTracksQuery>,
) -> Result<Json<PageResponse<SavedTrackItemDto>>, ApiError> {
    let page = normalized_page(query.page);
    let (page_rows, total, track_ids) = {
        let state = context.library.read().await;
        let rows = saved_track_rows(&state, query.q.as_deref());
        let page_rows = paginate_vec(&rows, page, PAGE_SIZE);
        let track_ids = page_rows
            .iter()
            .map(|item| item.track_id.clone())
            .collect::<Vec<_>>();
        (page_rows, rows.len(), track_ids)
    };
    schedule_artwork_enrichment(&context, track_ids);
    Ok(Json(PageResponse::new(page_rows, total, page, PAGE_SIZE)))
}

async fn api_delete_saved_track(
    State(context): State<Arc<AppContext>>,
    Path(saved_track_id): Path<String>,
) -> Result<Json<MessageResponse>, ApiError> {
    let connections = read_provider_connections().await?;
    let linked_track_ids = {
        let mut state = context.library.write().await;
        let linked_track_ids = saved_track_provider_links(&state, &saved_track_id)?;
        if !state.remove_saved_track(&saved_track_id) {
            return Err(ApiError::not_found(format!(
                "Unknown saved track '{saved_track_id}'."
            )));
        }
        persist_library(&state).await?;
        context.bump_library_version();
        linked_track_ids
    };
    // Provider network propagation runs with no library lock held.
    let warnings = propagate_saved_track_delete(&connections, &linked_track_ids).await;
    Ok(Json(MessageResponse::with_warnings(
        "Removed saved track from the canonical library.",
        warnings,
    )))
}

async fn api_tracks(
    State(context): State<Arc<AppContext>>,
    Query(query): Query<TracksQuery>,
) -> Result<Json<PageResponse<TrackListItemDto>>, ApiError> {
    let page = normalized_page(query.page);
    let (page_rows, total, track_ids) = {
        let state = context.library.read().await;
        let rows = track_rows(&state, query.q.as_deref(), query.coverage.as_deref());
        let page_rows = paginate_vec(&rows, page, PAGE_SIZE);
        let track_ids = page_rows
            .iter()
            .map(|item| item.track_id.clone())
            .collect::<Vec<_>>();
        (page_rows, rows.len(), track_ids)
    };
    schedule_artwork_enrichment(&context, track_ids);
    Ok(Json(PageResponse::new(page_rows, total, page, PAGE_SIZE)))
}

async fn api_identity_conflicts(
    State(context): State<Arc<AppContext>>,
    Query(query): Query<IdentityConflictsQuery>,
) -> Result<Json<PageResponse<TrackIdentityConflictQueueItemDto>>, ApiError> {
    let page = normalized_page(query.page);
    let filters = IdentityConflictFilters {
        query: query.q.as_deref(),
        provider: query.provider,
        recommendation: query.recommendation.as_deref(),
        impact: query.impact.as_deref(),
    };
    let (page_rows, total, track_ids) = {
        let state = context.library.read().await;
        let rows = identity_conflict_rows_filtered(&state, filters);
        let page_rows = paginate_vec(&rows, page, PAGE_SIZE);
        let track_ids = page_rows
            .iter()
            .flat_map(|item| {
                [
                    item.source_track.track_id.clone(),
                    item.conflict.owner_track.track_id.clone(),
                ]
            })
            .collect::<Vec<_>>();
        (page_rows, rows.len(), track_ids)
    };
    schedule_artwork_enrichment(&context, track_ids);
    Ok(Json(PageResponse::new(page_rows, total, page, PAGE_SIZE)))
}

async fn api_identity_gaps(
    State(context): State<Arc<AppContext>>,
    Query(query): Query<IdentityGapsQuery>,
) -> Result<Json<PageResponse<TrackIdentityGapQueueItemDto>>, ApiError> {
    let page = normalized_page(query.page);
    let (page_rows, total, track_ids) = {
        let state = context.library.read().await;
        let rows = identity_gap_rows(&state, query.provider, query.q.as_deref());
        let page_rows = paginate_vec(&rows, page, PAGE_SIZE);
        let track_ids = page_rows
            .iter()
            .map(|item| item.track.track_id.clone())
            .collect::<Vec<_>>();
        (page_rows, rows.len(), track_ids)
    };
    schedule_artwork_enrichment(&context, track_ids);
    Ok(Json(PageResponse::new(page_rows, total, page, PAGE_SIZE)))
}

async fn api_track_detail(
    State(context): State<Arc<AppContext>>,
    Path(track_id): Path<String>,
) -> Result<Json<TrackDetailDto>, ApiError> {
    let detail = {
        let state = context.library.read().await;
        build_track_detail(&state, &track_id)?
    };
    schedule_artwork_enrichment(&context, vec![track_id]);
    Ok(Json(detail))
}

async fn api_update_track(
    State(context): State<Arc<AppContext>>,
    Path(track_id): Path<String>,
    Json(request): Json<UpdateTrackRequest>,
) -> Result<Json<MessageResponse>, ApiError> {
    let metadata = TrackMetadata {
        title: request.title,
        artists: request
            .artists
            .into_iter()
            .map(|artist| artist.trim().to_string())
            .filter(|artist| !artist.is_empty())
            .collect(),
        album: normalize_optional_text(request.album),
        duration_seconds: request.duration_seconds,
        isrc: normalize_optional_text(request.isrc),
    };
    let mut state = context.library.write().await;
    state
        .update_track_metadata(&track_id, metadata)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    persist_library(&state).await?;
    context.bump_library_version();
    Ok(Json(MessageResponse::new(
        "Updated canonical track metadata.",
    )))
}

async fn api_apply_track_identity(
    State(context): State<Arc<AppContext>>,
    Path(track_id): Path<String>,
    Json(request): Json<ApplyTrackIdentityRequest>,
) -> Result<Json<ApplyTrackIdentityResponse>, ApiError> {
    let provider_id = normalize_manual_provider_track_id(request.provider, &request.provider_id)?;
    let now = Utc::now();
    let mut state = context.library.write().await;
    let result = state
        .apply_track_identity(
            &track_id,
            request.provider,
            provider_id.clone(),
            LinkSource::Manual,
            Some(1.0),
            now,
        )
        .map_err(|error| ApiError::bad_request(sanitize_error_message(&error.to_string())))?;
    let result_track_id = result.track_id().to_string();
    state.set_track_status(
        &result_track_id,
        request.provider,
        SyncStatusRecord::synced(
            Some(provider_id.clone()),
            Some(1.0),
            Some(format!(
                "Manually linked {} identity.",
                request.provider.display_name()
            )),
            now,
        ),
    );
    persist_library(&state).await?;
    context.bump_library_version();
    drop(state);

    let (result_key, message) = match &result {
        TrackIdentityApplyResult::Linked { .. } => (
            "linked",
            format!(
                "Linked {} ID {} to the canonical track.",
                request.provider.display_name(),
                provider_id
            ),
        ),
        TrackIdentityApplyResult::AlreadyLinked { .. } => (
            "already_linked",
            format!(
                "{} ID {} was already linked to this canonical track.",
                request.provider.display_name(),
                provider_id
            ),
        ),
        TrackIdentityApplyResult::Merged {
            source_track_id,
            target_track_id,
        } => (
            "merged",
            format!(
                "Merged canonical track {source_track_id} into {target_track_id} through {} ID {}.",
                request.provider.display_name(),
                provider_id
            ),
        ),
    };

    Ok(Json(ApplyTrackIdentityResponse {
        message,
        result: result_key.to_string(),
        provider: request.provider.as_key().to_string(),
        provider_id,
        track_id: result_track_id,
    }))
}

async fn api_merge_track(
    State(context): State<Arc<AppContext>>,
    Path(source_track_id): Path<String>,
    Json(request): Json<MergeTrackRequest>,
) -> Result<Json<MergeTrackResponse>, ApiError> {
    let now = Utc::now();
    let resolution = track_merge_conflict_resolution(request.conflict_resolution);
    let mut state = context.library.write().await;
    let result = state
        .merge_track_into_resolving_conflicts(
            &source_track_id,
            &request.target_track_id,
            resolution,
            now,
        )
        .map_err(|error| ApiError::bad_request(sanitize_error_message(&error.to_string())))?;
    state.validate().map_err(ApiError::from)?;
    persist_library(&state).await?;
    context.bump_library_version();
    drop(state);

    let resolved_conflicts = resolved_provider_conflict_dtos(&result.resolved_conflicts);
    let conflict_count = resolved_conflicts.len();

    Ok(Json(MergeTrackResponse {
        message: format!(
            "Merged canonical track {} into {} and resolved {} provider ID conflict(s). Provider accounts were not changed.",
            result.source_track_id, result.target_track_id, conflict_count
        ),
        source_track_id: result.source_track_id,
        target_track_id: result.target_track_id,
        resolved_conflicts,
    }))
}

async fn api_identity_conflicts_bulk_merge_plan(
    State(context): State<Arc<AppContext>>,
    Query(query): Query<BulkMergeIdentityConflictsPlanQuery>,
) -> Result<Json<BulkMergeIdentityConflictsPlanDto>, ApiError> {
    let state = context.library.read().await;
    let rows = bulk_merge_identity_conflict_rows(
        &state,
        query.q.as_deref(),
        query.provider,
        query.impact.as_deref(),
    );

    Ok(Json(BulkMergeIdentityConflictsPlanDto {
        eligible_count: rows.len(),
        examples: rows
            .into_iter()
            .take(BULK_IDENTITY_CONFLICT_EXAMPLE_LIMIT)
            .collect(),
        warnings: bulk_merge_identity_conflict_warnings(),
    }))
}

async fn api_identity_conflicts_bulk_merge(
    State(context): State<Arc<AppContext>>,
    Json(request): Json<BulkMergeIdentityConflictsRequest>,
) -> Result<Json<BulkMergeIdentityConflictsResponse>, ApiError> {
    let mut state = context.library.write().await;
    let candidates = bulk_merge_identity_conflict_rows(
        &state,
        request.q.as_deref(),
        request.provider,
        request.impact.as_deref(),
    );
    let eligible_count = candidates.len();
    let max_merges = request
        .max_merges
        .unwrap_or(BULK_IDENTITY_CONFLICT_MERGE_LIMIT)
        .min(BULK_IDENTITY_CONFLICT_MERGE_LIMIT);
    let resolution = track_merge_conflict_resolution(request.conflict_resolution);
    let backup = tokio::task::spawn_blocking(storage::create_manual_library_backup)
        .await
        .context("Failed to join pre-merge backup task")?
        .map_err(ApiError::from)?;

    let mut merged_examples = Vec::new();
    let mut merged_count = 0_usize;
    let mut skipped_count = eligible_count.saturating_sub(max_merges);
    let mut resolved_provider_conflicts = 0_usize;
    let mut warnings = bulk_merge_identity_conflict_warnings();

    for candidate in candidates.into_iter().take(max_merges) {
        // Re-validate each candidate against the current (mutating) state, using
        // freshly built indexes because every merge changes the track set.
        let active_conflict = {
            let indexes = ConflictIndexes::build(&state);
            active_bulk_merge_identity_conflict(
                &state,
                &candidate.source_track.track_id,
                &candidate.conflict.owner_track.track_id,
                &candidate.conflict.provider,
                &candidate.conflict.provider_id,
                &indexes,
            )
        };
        let Some(active_conflict) = active_conflict else {
            skipped_count += 1;
            continue;
        };

        let result = match state.merge_track_into_resolving_conflicts(
            &active_conflict.source_track.track_id,
            &active_conflict.conflict.owner_track.track_id,
            resolution,
            Utc::now(),
        ) {
            Ok(result) => result,
            Err(error) => {
                skipped_count += 1;
                warnings.push(format!(
                    "Skipped {} because it could no longer be merged safely: {}",
                    active_conflict.source_track.title,
                    sanitize_error_message(&error.to_string())
                ));
                continue;
            }
        };

        let resolved_conflicts = resolved_provider_conflict_dtos(&result.resolved_conflicts);
        resolved_provider_conflicts += resolved_conflicts.len();
        merged_count += 1;
        if merged_examples.len() < BULK_IDENTITY_CONFLICT_EXAMPLE_LIMIT {
            merged_examples.push(BulkMergedIdentityConflictDto {
                source_track_id: result.source_track_id,
                target_track_id: result.target_track_id,
                title: active_conflict.source_track.title,
                provider: active_conflict.conflict.provider,
                provider_id: active_conflict.conflict.provider_id,
                resolved_conflicts,
            });
        }
    }

    state.validate().map_err(ApiError::from)?;
    persist_library(&state).await?;
    context.bump_library_version();
    drop(state);

    Ok(Json(BulkMergeIdentityConflictsResponse {
        message: format!(
            "Merged {merged_count} likely-same identity conflict(s). Provider accounts were not changed."
        ),
        eligible_count,
        merged_count,
        skipped_count,
        resolved_provider_conflicts,
        conflict_resolution: merge_conflict_resolution_key(request.conflict_resolution).to_string(),
        conflict_resolution_label: merge_conflict_resolution_label(request.conflict_resolution)
            .to_string(),
        pre_merge_backup_path: backup.path.display().to_string(),
        merged_examples,
        warnings,
    }))
}

fn track_merge_conflict_resolution(
    choice: MergeConflictResolutionChoice,
) -> TrackMergeConflictResolution {
    match choice {
        MergeConflictResolutionChoice::KeepSource => TrackMergeConflictResolution::KeepSource,
        MergeConflictResolutionChoice::KeepTarget => TrackMergeConflictResolution::KeepTarget,
    }
}

fn merge_conflict_resolution_key(choice: MergeConflictResolutionChoice) -> &'static str {
    match choice {
        MergeConflictResolutionChoice::KeepSource => "keep_source",
        MergeConflictResolutionChoice::KeepTarget => "keep_target",
    }
}

fn merge_conflict_resolution_label(choice: MergeConflictResolutionChoice) -> &'static str {
    match choice {
        MergeConflictResolutionChoice::KeepSource => "Keep source IDs",
        MergeConflictResolutionChoice::KeepTarget => "Keep candidate IDs",
    }
}

fn resolved_provider_conflict_dtos(
    conflicts: &[crate::domain::ResolvedTrackMergeConflict],
) -> Vec<ResolvedProviderConflictDto> {
    conflicts
        .iter()
        .map(|conflict| ResolvedProviderConflictDto {
            provider: conflict.provider_key.clone(),
            provider_name: provider_display_name(&conflict.provider_key),
            kept_provider_id: conflict.kept_provider_id.clone(),
            dropped_provider_id: conflict.dropped_provider_id.clone(),
            kept_from_source: conflict.kept_from_source,
        })
        .collect()
}

fn bulk_merge_identity_conflict_rows(
    state: &LibraryState,
    query: Option<&str>,
    provider: Option<ProviderKind>,
    impact: Option<&str>,
) -> Vec<TrackIdentityConflictQueueItemDto> {
    identity_conflict_rows_filtered(
        state,
        IdentityConflictFilters {
            query,
            provider,
            recommendation: Some("likely_same_recording"),
            impact,
        },
    )
}

fn bulk_merge_identity_conflict_warnings() -> Vec<String> {
    vec![
        "Bulk merge is limited to conflicts still classified as likely same recording.".to_string(),
        "A manual source-of-truth backup is created before any rows are merged.".to_string(),
        "Provider accounts are not changed by this operation.".to_string(),
    ]
}

fn active_bulk_merge_identity_conflict(
    state: &LibraryState,
    source_track_id: &str,
    target_track_id: &str,
    provider_key: &str,
    provider_id: &str,
    indexes: &ConflictIndexes<'_>,
) -> Option<TrackIdentityConflictQueueItemDto> {
    let source_track = state
        .tracks
        .iter()
        .find(|track| track.id == source_track_id)?;
    let source_track_dto = conflict_track_dto(source_track, indexes);
    identity_conflicts_for_track(source_track, indexes)
        .into_iter()
        .find(|conflict| {
            conflict.provider == provider_key
                && conflict.provider_id == provider_id
                && conflict.owner_track.track_id == target_track_id
                && conflict.evidence.recommendation.key == "likely_same_recording"
        })
        .map(|conflict| TrackIdentityConflictQueueItemDto {
            source_track: source_track_dto,
            conflict,
        })
}

async fn api_reject_track_identity_conflict(
    State(context): State<Arc<AppContext>>,
    Path(track_id): Path<String>,
    Json(request): Json<RejectTrackIdentityConflictRequest>,
) -> Result<Json<MessageResponse>, ApiError> {
    let provider_id = request.provider_id.trim().to_string();
    if provider_id.is_empty() {
        return Err(ApiError::bad_request("Provider ID cannot be empty."));
    }

    let mut state = context.library.write().await;
    let indexes = ConflictIndexes::build(&state);
    let track = state
        .tracks
        .iter()
        .find(|track| track.id == track_id)
        .ok_or_else(|| ApiError::not_found(format!("Unknown track '{track_id}'.")))?;
    let conflict = identity_conflicts_for_track(track, &indexes)
        .into_iter()
        .find(|conflict| {
            conflict.provider == request.provider.as_key()
                && conflict.provider_id == provider_id
                && conflict.owner_track.track_id == request.owner_track_id
        })
        .ok_or_else(|| {
            ApiError::bad_request(
                "That identity conflict is no longer active for this track.".to_string(),
            )
        })?;
    let provider_name = conflict.provider_name.clone();
    let candidate_provider_id = conflict.provider_id.clone();
    drop(indexes);
    let now = Utc::now();
    // Flip the typed conflict into a permanent rejected tombstone so the
    // candidate is never re-proposed by future identity syncs.
    if !state.reject_track_identity_conflict(
        &track_id,
        request.provider,
        &candidate_provider_id,
        now,
    ) {
        return Err(ApiError::bad_request(
            "That identity conflict is no longer active for this track.".to_string(),
        ));
    }
    persist_library(&state).await?;
    context.bump_library_version();
    drop(state);

    Ok(Json(MessageResponse::new(format!(
        "Marked {provider_name} candidate {candidate_provider_id} as not the same track. The source row remains unresolved for that provider."
    ))))
}

async fn api_delete_track(
    State(context): State<Arc<AppContext>>,
    Path(track_id): Path<String>,
) -> Result<Json<MessageResponse>, ApiError> {
    let connections = read_provider_connections().await?;
    let (mut working, linked_track_ids, affected_playlist_ids) = {
        let mut state = context.library.write().await;
        let linked_track_ids = track_provider_links(&state, &track_id)?;
        let affected_playlist_ids = playlist_ids_for_track(&state, &track_id);
        if !state.remove_track_everywhere(&track_id) {
            return Err(ApiError::not_found(format!("Unknown track '{track_id}'.")));
        }
        persist_library(&state).await?;
        context.bump_library_version();
        (state.clone(), linked_track_ids, affected_playlist_ids)
    };

    // Provider propagation runs off-lock against the detached clone.
    let mut warnings = propagate_saved_track_delete(&connections, &linked_track_ids).await;
    warnings.extend(
        propagate_playlist_subset_to_connected_providers(
            &connections,
            &mut working,
            &affected_playlist_ids,
        )
        .await,
    );

    // Reconcile the affected playlists' provider bookkeeping back into the
    // current state (scoped to those playlists, so concurrent edits elsewhere
    // survive), then persist.
    if !affected_playlist_ids.is_empty() {
        let synced = playlist_subset_state(&working, &affected_playlist_ids);
        let mut state = context.library.write().await;
        merge_subset_state(&mut state, &synced);
        persist_library(&state).await?;
        context.bump_library_version();
    }

    Ok(Json(MessageResponse::with_warnings(
        "Removed track from saved tracks and playlists.",
        warnings,
    )))
}

async fn api_playlists(
    State(context): State<Arc<AppContext>>,
    Query(query): Query<PlaylistsQuery>,
) -> Result<Json<PageResponse<PlaylistSummaryDto>>, ApiError> {
    let page = normalized_page(query.page);
    let (page_rows, total, track_ids) = {
        let state = context.library.read().await;
        let rows = playlist_summaries(&state, query.q.as_deref());
        let page_rows = paginate_vec(&rows, page, PAGE_SIZE);
        let playlist_ids = page_rows
            .iter()
            .map(|item| item.playlist_id.clone())
            .collect::<Vec<_>>();
        let track_ids = playlist_artwork_track_ids(&state, &playlist_ids);
        (page_rows, rows.len(), track_ids)
    };
    schedule_artwork_enrichment(&context, track_ids);
    Ok(Json(PageResponse::new(page_rows, total, page, PAGE_SIZE)))
}

async fn api_playlist_detail(
    State(context): State<Arc<AppContext>>,
    Path(playlist_id): Path<String>,
) -> Result<Json<PlaylistDetailDto>, ApiError> {
    let (detail, track_ids) = {
        let state = context.library.read().await;
        let track_ids = state
            .playlists
            .iter()
            .find(|playlist| playlist.id == playlist_id)
            .map(|playlist| {
                playlist
                    .entries
                    .iter()
                    .map(|entry| entry.track_id.clone())
                    .collect::<Vec<_>>()
            })
            .ok_or_else(|| ApiError::not_found(format!("Unknown playlist '{playlist_id}'.")))?;
        let detail = build_playlist_detail(&state, &playlist_id)?;
        (detail, track_ids)
    };
    schedule_artwork_enrichment(&context, track_ids);
    Ok(Json(detail))
}

async fn api_update_playlist(
    State(context): State<Arc<AppContext>>,
    Path(playlist_id): Path<String>,
    Json(request): Json<UpdatePlaylistRequest>,
) -> Result<Json<MessageResponse>, ApiError> {
    let mut state = context.library.write().await;
    state
        .update_playlist_details(
            &playlist_id,
            request.name,
            normalize_optional_text(request.description),
        )
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    persist_library(&state).await?;
    context.bump_library_version();
    Ok(Json(MessageResponse::new(
        "Updated canonical playlist details.",
    )))
}

async fn api_delete_playlist(
    State(context): State<Arc<AppContext>>,
    Path(playlist_id): Path<String>,
) -> Result<Json<MessageResponse>, ApiError> {
    let connections = read_provider_connections().await?;
    let linked_playlist_ids = {
        let mut state = context.library.write().await;
        let linked_playlist_ids = playlist_provider_links(&state, &playlist_id)?;
        if !state.remove_playlist(&playlist_id) {
            return Err(ApiError::not_found(format!(
                "Unknown playlist '{playlist_id}'."
            )));
        }
        persist_library(&state).await?;
        context.bump_library_version();
        linked_playlist_ids
    };
    let warnings = propagate_playlist_delete(&connections, &linked_playlist_ids).await;
    Ok(Json(MessageResponse::with_warnings(
        "Deleted playlist from the canonical library.",
        warnings,
    )))
}

async fn api_delete_playlist_entry(
    State(context): State<Arc<AppContext>>,
    Path((playlist_id, entry_id)): Path<(String, String)>,
) -> Result<Json<MessageResponse>, ApiError> {
    let connections = read_provider_connections().await?;
    let mut working = {
        let mut state = context.library.write().await;
        if !state.remove_playlist_entry(&playlist_id, &entry_id) {
            return Err(ApiError::not_found(format!(
                "Unknown playlist entry '{entry_id}' for playlist '{playlist_id}'."
            )));
        }
        persist_library(&state).await?;
        context.bump_library_version();
        state.clone()
    };
    let warnings = propagate_playlist_subset_to_connected_providers(
        &connections,
        &mut working,
        std::slice::from_ref(&playlist_id),
    )
    .await;
    // Reconcile the affected playlist's provider bookkeeping back into current.
    let synced = playlist_subset_state(&working, std::slice::from_ref(&playlist_id));
    {
        let mut state = context.library.write().await;
        merge_subset_state(&mut state, &synced);
        persist_library(&state).await?;
        context.bump_library_version();
    }
    Ok(Json(MessageResponse::with_warnings(
        "Removed playlist entry from the canonical library.",
        warnings,
    )))
}

async fn api_schema() -> Result<Json<Vec<SchemaTableDto>>, ApiError> {
    Ok(Json(read_schema().await?))
}

async fn api_raw_table(
    Path(table_name): Path<String>,
    Query(query): Query<RawTablesQuery>,
) -> Result<Json<RawTableDto>, ApiError> {
    let page = normalized_page(query.page);
    Ok(Json(read_raw_table(&table_name, page).await?))
}

fn saved_track_provider_links(
    state: &LibraryState,
    saved_track_id: &str,
) -> Result<Vec<(ProviderKind, String)>, ApiError> {
    let saved_track = state
        .saved_tracks
        .iter()
        .find(|saved_track| saved_track.id == saved_track_id)
        .ok_or_else(|| ApiError::not_found(format!("Unknown saved track '{saved_track_id}'.")))?;
    track_provider_links(state, &saved_track.track_id)
}

fn track_provider_links(
    state: &LibraryState,
    track_id: &str,
) -> Result<Vec<(ProviderKind, String)>, ApiError> {
    let track = state
        .tracks
        .iter()
        .find(|track| track.id == track_id)
        .ok_or_else(|| ApiError::not_found(format!("Unknown track '{track_id}'.")))?;
    Ok(track
        .provider_links
        .iter()
        .filter_map(|(provider, link)| {
            ProviderKind::from_key(provider)
                .ok()
                .map(|provider| (provider, link.provider_id.clone()))
        })
        .collect())
}

fn playlist_provider_links(
    state: &LibraryState,
    playlist_id: &str,
) -> Result<Vec<(ProviderKind, String)>, ApiError> {
    let playlist = state
        .playlists
        .iter()
        .find(|playlist| playlist.id == playlist_id)
        .ok_or_else(|| ApiError::not_found(format!("Unknown playlist '{playlist_id}'.")))?;
    Ok(playlist
        .provider_links
        .iter()
        .filter_map(|(provider, link)| {
            ProviderKind::from_key(provider)
                .ok()
                .map(|provider| (provider, link.provider_id.clone()))
        })
        .collect())
}

fn playlist_ids_for_track(state: &LibraryState, track_id: &str) -> Vec<String> {
    state
        .playlists
        .iter()
        .filter(|playlist| {
            playlist
                .entries
                .iter()
                .any(|entry| entry.track_id == track_id)
        })
        .map(|playlist| playlist.id.clone())
        .collect()
}

async fn propagate_saved_track_delete(
    connections: &[ProviderConnection],
    linked_provider_ids: &[(ProviderKind, String)],
) -> Vec<String> {
    let mut warnings = Vec::new();
    for (provider, provider_track_id) in linked_provider_ids {
        let Some(connection) = connections
            .iter()
            .find(|connection| connection.provider == *provider)
        else {
            warnings.push(format!(
                "{} is not connected, so the saved-track deletion was not propagated there.",
                provider.display_name()
            ));
            continue;
        };

        match build_provider_from_connection(connection, ProviderCapability::Write).await {
            Ok(provider_client) => {
                if let Err(error) = provider_client.remove_saved_track(provider_track_id).await {
                    warnings.push(format!(
                        "Could not remove the saved track from {}: {error}",
                        provider.display_name()
                    ));
                }
            }
            Err(error) => warnings.push(format!(
                "Could not connect to {} to propagate saved-track deletion: {}",
                provider.display_name(),
                error.message
            )),
        }
    }
    warnings
}

async fn propagate_playlist_delete(
    connections: &[ProviderConnection],
    linked_provider_ids: &[(ProviderKind, String)],
) -> Vec<String> {
    let mut warnings = Vec::new();
    for (provider, provider_playlist_id) in linked_provider_ids {
        let Some(connection) = connections
            .iter()
            .find(|connection| connection.provider == *provider)
        else {
            warnings.push(format!(
                "{} is not connected, so the playlist deletion was not propagated there.",
                provider.display_name()
            ));
            continue;
        };

        match build_provider_from_connection(connection, ProviderCapability::Write).await {
            Ok(provider_client) => {
                if let Err(error) = provider_client.delete_playlist(provider_playlist_id).await {
                    warnings.push(format!(
                        "Could not delete the playlist on {}: {error}",
                        provider.display_name()
                    ));
                }
            }
            Err(error) => warnings.push(format!(
                "Could not connect to {} to propagate playlist deletion: {}",
                provider.display_name(),
                error.message
            )),
        }
    }
    warnings
}

async fn propagate_playlist_subset_to_connected_providers(
    connections: &[ProviderConnection],
    state: &mut LibraryState,
    playlist_ids: &[String],
) -> Vec<String> {
    if playlist_ids.is_empty() {
        return Vec::new();
    }

    let mut warnings = Vec::new();
    for provider in ProviderKind::all().iter().copied() {
        let provider_key = provider.as_key();
        let linked_here = playlist_ids.iter().any(|playlist_id| {
            state.playlists.iter().any(|playlist| {
                playlist.id == *playlist_id && playlist.provider_links.contains_key(provider_key)
            })
        });
        if !linked_here {
            continue;
        }

        let Some(connection) = connections
            .iter()
            .find(|connection| connection.provider == provider)
        else {
            warnings.push(format!(
                "{} is not connected, so playlist edits were not propagated there.",
                provider.display_name()
            ));
            continue;
        };

        let mut subset = playlist_subset_state(state, playlist_ids);
        if subset.playlists.is_empty() {
            continue;
        }

        let provider_client =
            match build_provider_from_connection(connection, ProviderCapability::Write).await {
                Ok(provider_client) => provider_client,
                Err(error) => {
                    warnings.push(format!(
                        "Could not connect to {} to propagate playlist edits: {}",
                        provider.display_name(),
                        error.message
                    ));
                    continue;
                }
            };
        if let Err(error) = provider_client.sync_library(&mut subset, true).await {
            merge_subset_state(state, &subset);
            warnings.push(format!(
                "Could not fully propagate playlist updates to {}: {error}",
                provider.display_name()
            ));
        } else {
            merge_subset_state(state, &subset);
        }
    }

    warnings
}

fn playlist_subset_state(state: &LibraryState, playlist_ids: &[String]) -> LibraryState {
    let playlist_set = playlist_ids
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let playlists = state
        .playlists
        .iter()
        .filter(|playlist| playlist_set.contains(&playlist.id))
        .cloned()
        .collect::<Vec<_>>();
    let referenced_track_ids = playlists
        .iter()
        .flat_map(|playlist| playlist.entries.iter().map(|entry| entry.track_id.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    let tracks = state
        .tracks
        .iter()
        .filter(|track| referenced_track_ids.contains(&track.id))
        .cloned()
        .collect::<Vec<_>>();

    LibraryState {
        format_version: state.format_version,
        created_at: state.created_at,
        updated_at: Utc::now(),
        tracks,
        saved_tracks: Vec::new(),
        playlists,
    }
}

fn merge_subset_state(state: &mut LibraryState, subset: &LibraryState) {
    for subset_track in &subset.tracks {
        if let Some(track) = state
            .tracks
            .iter_mut()
            .find(|track| track.id == subset_track.id)
        {
            track.provider_links = subset_track.provider_links.clone();
            track.provider_artwork = subset_track.provider_artwork.clone();
            track.provider_state = subset_track.provider_state.clone();
        }
    }

    for subset_playlist in &subset.playlists {
        if let Some(playlist) = state
            .playlists
            .iter_mut()
            .find(|playlist| playlist.id == subset_playlist.id)
        {
            playlist.provider_links = subset_playlist.provider_links.clone();
            playlist.provider_state = subset_playlist.provider_state.clone();
            for subset_entry in &subset_playlist.entries {
                if let Some(entry) = playlist
                    .entries
                    .iter_mut()
                    .find(|entry| entry.id == subset_entry.id)
                {
                    entry.provider_state = subset_entry.provider_state.clone();
                }
            }
        }
    }

    state.touch();
}

/// Write-through persist of the in-memory canonical state. Callers hold the
/// library write lock, mutate `*guard`, then call this before dropping the lock
/// so persistence stays ordered with the in-memory copy. The change-guarded
/// [`storage::write_library_state`] skips redundant writes/backups, and the
/// blocking SQLite work runs on a blocking thread. This clones the state to move
/// it into the blocking task; the clone cost is paid only on actual mutations.
async fn persist_library(state: &LibraryState) -> Result<(), ApiError> {
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
async fn runtime_db<T, F>(operation: F) -> Result<T, ApiError>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .context("Failed to join runtime database task")?
        .map_err(ApiError::from)
}

async fn read_provider_connections() -> Result<Vec<ProviderConnection>, ApiError> {
    tokio::task::spawn_blocking(storage::list_provider_connections)
        .await
        .context("Failed to join provider connection read task")?
        .map_err(ApiError::from)
}

async fn build_provider_from_connection(
    connection: &ProviderConnection,
    capability: ProviderCapability,
) -> Result<Box<dyn StreamingProvider>, ApiError> {
    match &connection.config {
        ProviderConnectionConfig::Spotify(config) => Ok(Box::new(
            SpotifyProvider::from_connection(config, capability)
                .await
                .map_err(ApiError::from)?,
        )),
        ProviderConnectionConfig::YoutubeMusic(config) => Ok(Box::new(
            YoutubeMusicProvider::from_connection(config).map_err(ApiError::from)?,
        )),
    }
}

async fn build_connected_provider(
    provider: ProviderKind,
    capability: ProviderCapability,
) -> Result<Box<dyn StreamingProvider>, ApiError> {
    ensure_provider_not_cooling_down(provider).await?;
    ensure_provider_health_allows_operation(provider).await?;
    build_connected_provider_allowing_failed_health(provider, capability).await
}

async fn build_connected_provider_allowing_failed_health(
    provider: ProviderKind,
    capability: ProviderCapability,
) -> Result<Box<dyn StreamingProvider>, ApiError> {
    ensure_provider_not_cooling_down(provider).await?;
    let connection = runtime_db(move || storage::read_provider_connection(provider))
        .await?
        .ok_or_else(|| {
            ApiError::bad_request(format!(
                "{} is not connected in the app yet.",
                provider.display_name()
            ))
        })?;

    build_provider_from_connection(&connection, capability).await
}

/// Re-applies the provider-scoped results of a push/sync — which ran against a
/// detached clone (`result`) during off-lock network I/O — onto the current
/// canonical state, item by item and only for `provider`'s key. Sync's only
/// state mutations are provider-link and per-provider status upserts keyed by
/// stable IDs, so copying just those fields for items that still exist keeps
/// concurrent user edits to other dimensions (metadata, membership, artwork, the
/// other provider) intact. Items the user deleted meanwhile are absent from the
/// current state and skipped. Each provider-scoped field is set to the clone's
/// value or removed when the clone no longer carries it, so a reset-push (which
/// clears this provider's playlist dimension before re-pushing) is reflected
/// exactly.
fn reapply_provider_sync(
    current: &mut LibraryState,
    result: &LibraryState,
    provider: ProviderKind,
) {
    let key = provider.as_key();

    let result_tracks: HashMap<&str, &TrackEntity> = result
        .tracks
        .iter()
        .map(|track| (track.id.as_str(), track))
        .collect();
    for track in &mut current.tracks {
        let Some(source) = result_tracks.get(track.id.as_str()) else {
            continue;
        };
        reapply_map_entry(&mut track.provider_links, &source.provider_links, key);
        reapply_map_entry(&mut track.provider_state, &source.provider_state, key);
    }

    let result_saved: HashMap<&str, &SavedTrackEntry> = result
        .saved_tracks
        .iter()
        .map(|saved| (saved.id.as_str(), saved))
        .collect();
    for saved in &mut current.saved_tracks {
        let Some(source) = result_saved.get(saved.id.as_str()) else {
            continue;
        };
        reapply_map_entry(&mut saved.provider_state, &source.provider_state, key);
    }

    let result_playlists: HashMap<&str, &PlaylistEntity> = result
        .playlists
        .iter()
        .map(|playlist| (playlist.id.as_str(), playlist))
        .collect();
    for playlist in &mut current.playlists {
        let Some(source) = result_playlists.get(playlist.id.as_str()) else {
            continue;
        };
        reapply_map_entry(&mut playlist.provider_links, &source.provider_links, key);
        reapply_map_entry(&mut playlist.provider_state, &source.provider_state, key);
        let source_entries: HashMap<&str, &PlaylistEntry> = source
            .entries
            .iter()
            .map(|entry| (entry.id.as_str(), entry))
            .collect();
        for entry in &mut playlist.entries {
            let Some(source_entry) = source_entries.get(entry.id.as_str()) else {
                continue;
            };
            reapply_map_entry(&mut entry.provider_state, &source_entry.provider_state, key);
        }
    }

    current.touch();
}

/// Copies a single provider key's value from `source` into `target`, removing it
/// from `target` when `source` no longer carries that key.
fn reapply_map_entry<V: Clone>(
    target: &mut BTreeMap<String, V>,
    source: &BTreeMap<String, V>,
    key: &str,
) {
    match source.get(key) {
        Some(value) => {
            target.insert(key.to_string(), value.clone());
        }
        None => {
            target.remove(key);
        }
    }
}

async fn insert_operation(
    context: &Arc<AppContext>,
    mut operation: OperationRecord,
) -> Result<(), ApiError> {
    if !matches!(operation.kind, OperationKind::IdentityAll) {
        ensure_provider_not_cooling_down(operation.provider).await?;
    }
    if !matches!(
        operation.kind,
        OperationKind::Verify | OperationKind::IdentityAll
    ) {
        ensure_provider_health_allows_operation(operation.provider).await?;
    }
    let persisted = {
        let mut operations = context
            .operations
            .lock()
            .map_err(|_| ApiError::internal("Failed to inspect running operations."))?;
        if operations
            .values()
            .any(|operation| operation.status == OperationStatus::Running)
        {
            return Err(ApiError::bad_request(
                "Another provider operation is already running. Wait for it to finish first.",
            ));
        }
        operation.last_persisted_at = Some(Utc::now());
        operations.insert(operation.id.clone(), operation.clone());
        operation
    };
    runtime_db(move || persist_operation_blocking(&persisted)).await
}

/// Serializes and persists an operation record to `runtime.db`. Runs on a
/// blocking thread via [`runtime_db`] or [`tokio::task::spawn_blocking`]; never
/// call it directly from an async context.
fn persist_operation_blocking(operation: &OperationRecord) -> Result<()> {
    let payload_json = serde_json::to_string(operation)?;
    storage::save_ui_operation_json(
        &operation.id,
        operation_status_key(operation.status),
        &payload_json,
    )
}

fn persist_operation(operation: &OperationRecord) -> Result<(), ApiError> {
    persist_operation_blocking(operation).map_err(ApiError::from)
}

fn load_recovered_operations() -> Result<HashMap<String, OperationRecord>> {
    let mut operations = HashMap::new();
    for payload_json in storage::list_ui_operation_json()? {
        let mut operation: OperationRecord = serde_json::from_str(&payload_json)
            .context("Failed to parse persisted UI operation history")?;
        if operation.status == OperationStatus::Running {
            operation.status = OperationStatus::Failed;
            operation.stage = "Interrupted".to_string();
            operation.error = Some(
                "The app stopped before this operation finished. Review the canonical state and start the operation again."
                    .to_string(),
            );
            operation.finished_at = Some(Utc::now());
            persist_operation(&operation).map_err(|error| anyhow::anyhow!(error.message))?;
        }
        operations.insert(operation.id.clone(), operation);
    }
    Ok(operations)
}

fn operation_status_key(status: OperationStatus) -> &'static str {
    match status {
        OperationStatus::Running => "running",
        OperationStatus::Succeeded => "succeeded",
        OperationStatus::Failed => "failed",
    }
}

fn progress_handler_for_operation(
    context: Arc<AppContext>,
    operation_id: String,
) -> ProgressHandler {
    Arc::new(move |progress: ProviderProgress| {
        let persist = if let Ok(mut operations) = context.operations.lock() {
            if let Some(operation) = operations.get_mut(&operation_id) {
                operation.stage = progress.stage;
                operation.detail = progress.detail;
                operation.saved_tracks_done = progress.saved_tracks_done;
                operation.saved_tracks_total = progress.saved_tracks_total;
                operation.playlists_done = progress.playlists_done;
                operation.playlists_total = progress.playlists_total;
                operation.playlist_entries_done = progress.playlist_entries_done;
                operation.playlist_entries_total = progress.playlist_entries_total;
                let now = Utc::now();
                let should_persist = operation
                    .last_persisted_at
                    .map(|last| (now - last).num_seconds() >= 1)
                    .unwrap_or(true);
                if should_persist {
                    operation.last_persisted_at = Some(now);
                    Some(operation.clone())
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        if let Some(operation) = persist {
            // Persist on a blocking thread so the provider's network loop (which
            // invokes this synchronous callback) is never parked on SQLite.
            tokio::task::spawn_blocking(move || {
                if let Err(error) = persist_operation_blocking(&operation) {
                    eprintln!("Failed to persist operation progress: {error}");
                }
            });
        }
    })
}

async fn finish_operation(
    context: Arc<AppContext>,
    operation_id: &str,
    result: Result<MessageResponse, ApiError>,
) {
    let mut cooldown_to_save = None;
    let mut cooldown_to_clear = None;
    let mut health_to_save = None;
    let persisted = if let Ok(mut operations) = context.operations.lock() {
        if let Some(operation) = operations.get_mut(operation_id) {
            match result {
                Ok(response) => {
                    if !matches!(operation.kind, OperationKind::IdentityAll) {
                        cooldown_to_clear = Some(operation.provider);
                        health_to_save = Some(provider_health_ok(
                            operation.provider,
                            format!("{} operation succeeded.", operation.provider.display_name()),
                        ));
                    }
                    operation.status = OperationStatus::Succeeded;
                    operation.stage = "Done".to_string();
                    operation.message = Some(response.message);
                    operation.warnings = response.warnings;
                    operation.finished_at = Some(Utc::now());
                }
                Err(error) => {
                    if !matches!(operation.kind, OperationKind::IdentityAll) {
                        if let Some(source) = &error.source {
                            cooldown_to_save =
                                policy::cooldown_from_error(operation.provider, source);
                        }
                        if matches!(operation.kind, OperationKind::Verify)
                            || error
                                .source
                                .as_ref()
                                .is_some_and(policy::is_connection_health_failure)
                        {
                            health_to_save = Some(provider_health_failed(
                                operation.provider,
                                error.message.clone(),
                            ));
                        }
                    }
                    operation.status = OperationStatus::Failed;
                    operation.stage = "Failed".to_string();
                    operation.error = Some(error.message);
                    operation.finished_at = Some(Utc::now());
                    if let Some(cooldown) = &cooldown_to_save {
                        operation.warnings.push(format!(
                            "{} will be held until {} to avoid hammering the provider.",
                            operation.provider.display_name(),
                            cooldown.blocked_until.to_rfc3339()
                        ));
                    }
                }
            }
            operation.last_persisted_at = Some(Utc::now());
            Some(operation.clone())
        } else {
            None
        }
    } else {
        None
    };
    // The remaining work is all blocking runtime-database I/O; run it on a
    // blocking thread so this finaliser never parks the async worker on SQLite.
    let _ = tokio::task::spawn_blocking(move || {
        if let Some(provider) = cooldown_to_clear {
            if let Err(error) = storage::clear_provider_cooldown(provider) {
                eprintln!("Failed to clear provider cooldown: {error}");
            }
        }
        if let Some(cooldown) = cooldown_to_save {
            if let Err(error) = storage::save_provider_cooldown(&cooldown) {
                eprintln!("Failed to persist provider cooldown: {error}");
            }
        }
        if let Some(health) = health_to_save {
            if let Err(error) = storage::save_provider_health(&health) {
                eprintln!("Failed to persist provider health: {error}");
            }
        }
        if let Some(operation) = persisted {
            if let Err(error) = persist_operation_blocking(&operation) {
                eprintln!("Failed to persist finished operation: {error}");
            }
        }
    })
    .await;
}

fn operation_response(operation: &OperationRecord) -> OperationResponse {
    let (provider_key, provider_name) = if matches!(operation.kind, OperationKind::IdentityAll) {
        ("library".to_string(), "Library".to_string())
    } else {
        (
            operation.provider.as_key().to_string(),
            operation.provider.display_name().to_string(),
        )
    };

    OperationResponse {
        operation_id: operation.id.clone(),
        provider_key,
        provider_name,
        kind: operation.kind,
        status: operation.status,
        stage: operation.stage.clone(),
        detail: operation.detail.clone(),
        saved_tracks_done: operation.saved_tracks_done,
        saved_tracks_total: operation.saved_tracks_total,
        playlists_done: operation.playlists_done,
        playlists_total: operation.playlists_total,
        playlist_entries_done: operation.playlist_entries_done,
        playlist_entries_total: operation.playlist_entries_total,
        message: operation.message.clone(),
        warnings: operation.warnings.clone(),
        error: operation.error.clone(),
        started_at: operation.started_at.to_rfc3339(),
        finished_at: operation.finished_at.map(|at| at.to_rfc3339()),
    }
}

fn provider_connection_payloads(
    state: &LibraryState,
    connections: &[ProviderConnection],
    cooldowns: &[ProviderCooldown],
    healths: &[ProviderHealth],
) -> Vec<ProviderConnectionDto> {
    // The open-conflict count is provider-independent, so compute the whole
    // conflict set once and reuse it for every provider's preflight instead of
    // rescanning per provider on each `/api/providers` poll.
    let identity_conflicts = identity_conflict_rows(state, None).len();
    ProviderKind::all()
        .iter()
        .copied()
        .map(|provider| {
            let connection = connections
                .iter()
                .find(|connection| connection.provider == provider);
            let cooldown = cooldowns
                .iter()
                .find(|cooldown| cooldown.provider == provider);
            let health = healths.iter().find(|health| health.provider == provider);
            ProviderConnectionDto {
                key: provider.as_key().to_string(),
                name: provider.display_name().to_string(),
                connected: connection.is_some(),
                connected_at: connection.map(|connection| connection.connected_at.to_rfc3339()),
                updated_at: connection.map(|connection| connection.updated_at.to_rfc3339()),
                health_checked_at: health.map(|health| health.checked_at.to_rfc3339()),
                health_ok: health.map(|health| health.ok),
                health_message: health.and_then(|health| health.message.clone()),
                cooldown_until: cooldown.map(|cooldown| cooldown.blocked_until.to_rfc3339()),
                cooldown_reason: cooldown.map(|cooldown| cooldown.reason.clone()),
                preflight: provider_preflight_payload(
                    state,
                    provider,
                    connection,
                    cooldown,
                    health,
                    identity_conflicts,
                ),
            }
        })
        .collect()
}

fn backup_dto(backup: storage::LibraryBackup) -> BackupDto {
    BackupDto {
        file_name: backup.file_name,
        path: backup.path.display().to_string(),
        backup_type: backup.backup_type.to_string(),
        size_bytes: backup.size_bytes,
        modified_at: backup.modified_at.map(|value| value.to_rfc3339()),
    }
}

fn provider_preflight_payload(
    state: &LibraryState,
    provider: ProviderKind,
    connection: Option<&ProviderConnection>,
    cooldown: Option<&ProviderCooldown>,
    health: Option<&ProviderHealth>,
    identity_conflicts: usize,
) -> ProviderPreflightDto {
    let provider_key = provider.as_key();
    let track_index = build_track_index(state);
    let track_ids_linked = state
        .tracks
        .iter()
        .filter(|track| track.provider_links.contains_key(provider_key))
        .count();
    let saved_tracks_pushable = state
        .saved_tracks
        .iter()
        .filter(|entry| indexed_track_has_provider_link(&track_index, &entry.track_id, provider))
        .count();
    let playlist_entries_pushable = state
        .playlists
        .iter()
        .flat_map(|playlist| playlist.entries.iter())
        .filter(|entry| indexed_track_has_provider_link(&track_index, &entry.track_id, provider))
        .count();
    let saved_tracks_missing_identity = state
        .saved_tracks
        .len()
        .saturating_sub(saved_tracks_pushable);
    let playlist_entries_total = state.playlist_entry_count();
    let playlist_entries_missing_identity =
        playlist_entries_total.saturating_sub(playlist_entries_pushable);
    let track_ids_missing = state.tracks.len().saturating_sub(track_ids_linked);

    let mut blockers = Vec::new();
    if connection.is_none() {
        blockers.push(format!("{} is not linked.", provider.display_name()));
    }
    if let Some(cooldown) = cooldown {
        blockers.push(format!(
            "{} is cooling down until {}.",
            provider.display_name(),
            cooldown.blocked_until.to_rfc3339()
        ));
    }
    if let Some(health) = health {
        if !health.ok {
            blockers.push(format!(
                "Last {} connection check failed: {}",
                provider.display_name(),
                health
                    .message
                    .as_deref()
                    .unwrap_or("No detailed provider message was recorded.")
            ));
        }
    }
    if state.saved_tracks.is_empty() && playlist_entries_total == 0 {
        blockers.push(
            "The canonical library has no saved tracks or playlist entries to push.".to_string(),
        );
    }

    let mut warnings = Vec::new();
    if saved_tracks_missing_identity > 0 {
        warnings.push(format!(
            "{saved_tracks_missing_identity} saved tracks do not have a {} ID and will be skipped during push.",
            provider.display_name()
        ));
    }
    if playlist_entries_missing_identity > 0 {
        warnings.push(format!(
            "{playlist_entries_missing_identity} playlist entries do not have a {} ID and will be skipped during push.",
            provider.display_name()
        ));
    }
    if track_ids_missing > 0 {
        warnings.push(format!(
            "{track_ids_missing} canonical tracks are still missing a {} identity.",
            provider.display_name()
        ));
    }
    if provider == ProviderKind::YoutubeMusic {
        warnings.push(
            "YouTube Music browser headers can expire; relink if a pull or push reports an authentication error."
                .to_string(),
        );
    }

    let mut reset_blockers = Vec::new();
    if provider.supports_library_reset() {
        if saved_tracks_missing_identity > 0 || playlist_entries_missing_identity > 0 {
            reset_blockers.push(format!(
                "Reset & Push is blocked because {} saved tracks and {} playlist entries would be skipped after purging {}.",
                saved_tracks_missing_identity,
                playlist_entries_missing_identity,
                provider.display_name()
            ));
        }
        if identity_conflicts > 0 {
            reset_blockers.push(format!(
                "Reset & Push is blocked while {identity_conflicts} identity conflicts need merge review."
            ));
        }
    }

    let can_pull = connection.is_some()
        && cooldown.is_none()
        && health.map(|health| health.ok).unwrap_or(true);
    let can_push = can_pull
        && blockers.is_empty()
        && (saved_tracks_pushable > 0 || playlist_entries_pushable > 0);
    let can_reset_push = can_push && provider.supports_library_reset() && reset_blockers.is_empty();
    ProviderPreflightDto {
        can_pull,
        can_push,
        can_reset_push,
        blockers,
        reset_blockers,
        warnings,
        saved_tracks_total: state.saved_tracks.len(),
        saved_tracks_pushable,
        saved_tracks_missing_identity,
        playlists_total: state.playlists.len(),
        linked_playlists: state
            .playlists
            .iter()
            .filter(|playlist| playlist.provider_links.contains_key(provider_key))
            .count(),
        playlist_entries_total,
        playlist_entries_pushable,
        playlist_entries_missing_identity,
        track_ids_total: state.tracks.len(),
        track_ids_linked,
        track_ids_missing,
    }
}

fn provider_push_plan_payload(
    state: &LibraryState,
    provider: ProviderKind,
    connection: Option<&ProviderConnection>,
    cooldown: Option<&ProviderCooldown>,
    health: Option<&ProviderHealth>,
) -> ProviderPushPlanDto {
    let identity_conflicts = identity_conflict_rows(state, None).len();
    ProviderPushPlanDto {
        provider: provider.as_key().to_string(),
        provider_name: provider.display_name().to_string(),
        preflight: provider_preflight_payload(
            state,
            provider,
            connection,
            cooldown,
            health,
            identity_conflicts,
        ),
        saved_tracks: push_saved_track_plan_section(state, provider),
        playlist_entries: push_playlist_entry_plan_section(state, provider),
        playlists: push_playlist_plan_section(state, provider),
    }
}

fn push_saved_track_plan_section(
    state: &LibraryState,
    provider: ProviderKind,
) -> PushPlanSectionDto {
    let track_index = build_track_index(state);
    let indexes = ConflictIndexes::build(state);
    let skipped_examples = state
        .saved_tracks
        .iter()
        .filter_map(|entry| {
            let track = track_index.get(entry.track_id.as_str())?;
            if track.provider_links.contains_key(provider.as_key()) {
                return None;
            }
            Some(conflict_track_dto(track, &indexes))
        })
        .take(10)
        .collect::<Vec<_>>();
    let pushable = state
        .saved_tracks
        .iter()
        .filter(|entry| indexed_track_has_provider_link(&track_index, &entry.track_id, provider))
        .count();

    PushPlanSectionDto {
        total: state.saved_tracks.len(),
        pushable,
        skipped_missing_identity: state.saved_tracks.len().saturating_sub(pushable),
        skipped_examples,
    }
}

fn push_playlist_entry_plan_section(
    state: &LibraryState,
    provider: ProviderKind,
) -> PushPlanSectionDto {
    let track_index = build_track_index(state);
    let indexes = ConflictIndexes::build(state);
    let total = state.playlist_entry_count();
    let mut pushable = 0;
    let mut skipped_examples = Vec::new();

    for entry in state
        .playlists
        .iter()
        .flat_map(|playlist| playlist.entries.iter())
    {
        let Some(track) = track_index.get(entry.track_id.as_str()) else {
            continue;
        };
        if track.provider_links.contains_key(provider.as_key()) {
            pushable += 1;
        } else if skipped_examples.len() < 10 {
            skipped_examples.push(conflict_track_dto(track, &indexes));
        }
    }

    PushPlanSectionDto {
        total,
        pushable,
        skipped_missing_identity: total.saturating_sub(pushable),
        skipped_examples,
    }
}

fn push_playlist_plan_section(
    state: &LibraryState,
    provider: ProviderKind,
) -> PushPlaylistPlanSectionDto {
    let track_index = build_track_index(state);
    let provider_key = provider.as_key();
    let mut examples = Vec::new();
    let mut linked = 0;

    for playlist in &state.playlists {
        let playlist_linked = playlist.provider_links.contains_key(provider_key);
        if playlist_linked {
            linked += 1;
        }
        let missing_entries = playlist
            .entries
            .iter()
            .filter(|entry| {
                !indexed_track_has_provider_link(&track_index, &entry.track_id, provider)
            })
            .count();

        if examples.len() < 10 && (!playlist_linked || missing_entries > 0) {
            examples.push(PushPlaylistPlanItemDto {
                playlist_id: playlist.id.clone(),
                name: playlist.name.clone(),
                entry_count: playlist.entries.len(),
                linked: playlist_linked,
                missing_entries,
            });
        }
    }

    PushPlaylistPlanSectionDto {
        total: state.playlists.len(),
        linked,
        unlinked: state.playlists.len().saturating_sub(linked),
        examples,
    }
}

fn random_state() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

fn app_notice_redirect(notice: Option<String>) -> String {
    match notice {
        Some(notice) => {
            let encoded =
                url::form_urlencoded::byte_serialize(notice.as_bytes()).collect::<String>();
            format!("/app/overview?notice={encoded}")
        }
        None => "/app/overview".to_string(),
    }
}

fn overview_payload(state: &LibraryState) -> OverviewResponse {
    let mut canonical_only = 0;
    let mut multi_provider = 0;
    let mut unmatched_tracks = 0;
    let mut provider_only_counts = BTreeMap::<String, usize>::new();
    let track_index = build_track_index(state);

    for track in &state.tracks {
        match track_coverage_key(track).as_str() {
            "multi-provider" => multi_provider += 1,
            "canonical-only" => canonical_only += 1,
            value if value.ends_with("-only") => {
                *provider_only_counts.entry(value.to_string()).or_default() += 1;
            }
            _ => canonical_only += 1,
        }

        if track
            .provider_state
            .values()
            .any(|status| status.state == SyncState::Unmatched)
        {
            unmatched_tracks += 1;
        }
    }

    let provider_metrics = ProviderKind::all()
        .iter()
        .copied()
        .map(|provider| {
            let provider_key = provider.as_key();
            let linked_tracks = state
                .tracks
                .iter()
                .filter(|track| track.provider_links.contains_key(provider_key))
                .count();
            let pushable_saved_tracks = state
                .saved_tracks
                .iter()
                .filter(|entry| {
                    indexed_track_has_provider_link(&track_index, &entry.track_id, provider)
                })
                .count();
            let pushable_playlist_entries = state
                .playlists
                .iter()
                .flat_map(|playlist| playlist.entries.iter())
                .filter(|entry| {
                    indexed_track_has_provider_link(&track_index, &entry.track_id, provider)
                })
                .count();

            ProviderStatsDto {
                key: provider_key.to_string(),
                name: provider.display_name().to_string(),
                linked_tracks,
                missing_track_ids: state.tracks.len().saturating_sub(linked_tracks),
                unmatched_tracks: state
                    .tracks
                    .iter()
                    .filter(|track| {
                        track
                            .provider_state
                            .get(provider_key)
                            .map(|status| status.state == SyncState::Unmatched)
                            .unwrap_or(false)
                    })
                    .count(),
                synced_saved_tracks: state
                    .saved_tracks
                    .iter()
                    .filter(|entry| {
                        entry
                            .provider_state
                            .get(provider_key)
                            .map(|status| status.state == SyncState::Synced)
                            .unwrap_or(false)
                    })
                    .count(),
                pushable_saved_tracks,
                saved_tracks_missing_identity: state
                    .saved_tracks
                    .len()
                    .saturating_sub(pushable_saved_tracks),
                unmatched_saved_tracks: state
                    .saved_tracks
                    .iter()
                    .filter(|entry| {
                        entry
                            .provider_state
                            .get(provider_key)
                            .map(|status| status.state == SyncState::Unmatched)
                            .unwrap_or(false)
                    })
                    .count(),
                linked_playlists: state
                    .playlists
                    .iter()
                    .filter(|playlist| playlist.provider_links.contains_key(provider_key))
                    .count(),
                pushable_playlist_entries,
                playlist_entries_missing_identity: state
                    .playlist_entry_count()
                    .saturating_sub(pushable_playlist_entries),
                unmatched_playlist_entries: state
                    .playlists
                    .iter()
                    .flat_map(|playlist| playlist.entries.iter())
                    .filter(|entry| {
                        entry
                            .provider_state
                            .get(provider_key)
                            .map(|status| status.state == SyncState::Unmatched)
                            .unwrap_or(false)
                    })
                    .count(),
            }
        })
        .collect();

    let provider_only_counts = ProviderKind::all()
        .iter()
        .copied()
        .map(|provider| ProviderOnlyCountDto {
            key: format!("{}-only", provider.as_key()),
            name: provider.display_name().to_string(),
            count: *provider_only_counts
                .get(&format!("{}-only", provider.as_key()))
                .unwrap_or(&0),
        })
        .collect();

    OverviewResponse {
        library_updated_at: state.updated_at.to_rfc3339(),
        tracks: state.tracks.len(),
        saved_tracks: state.saved_tracks.len(),
        playlists: state.playlists.len(),
        playlist_entries: state.playlist_entry_count(),
        canonical_only,
        multi_provider,
        unmatched_tracks,
        identity_conflicts: identity_conflict_rows(state, None).len(),
        provider_only_counts,
        provider_metrics,
    }
}

fn saved_track_rows(state: &LibraryState, query: Option<&str>) -> Vec<SavedTrackItemDto> {
    let normalized_query = normalized_query(query);
    let track_index = build_track_index(state);
    let mut rows = Vec::new();

    for saved_track in &state.saved_tracks {
        let Some(track) = track_index.get(saved_track.track_id.as_str()) else {
            continue;
        };
        let coverage = coverage_dto(track);
        let row = SavedTrackItemDto {
            saved_track_id: saved_track.id.clone(),
            track_id: track.id.clone(),
            title: track.metadata.title.clone(),
            artists: track.metadata.artists.clone(),
            artist_summary: track.metadata.artist_summary(),
            album: track.metadata.album.clone(),
            subtitle: track_subtitle(&track.metadata),
            duration_seconds: track.metadata.duration_seconds,
            duration_label: format_duration(track.metadata.duration_seconds),
            isrc: track.metadata.isrc.clone(),
            added_at: saved_track.added_at.clone(),
            added_label: format_date(saved_track.added_at.as_deref())
                .unwrap_or_else(|| "Unknown".to_string()),
            coverage,
            providers: provider_badges(&track.provider_links),
            status_pills: summarized_status_pills(&[
                &track.provider_state,
                &saved_track.provider_state,
            ]),
            artwork_url: preferred_artwork_url(track),
        };

        if query_matches(
            normalized_query.as_deref(),
            &[
                &row.title,
                &row.artist_summary,
                row.album.as_deref().unwrap_or(""),
                row.isrc.as_deref().unwrap_or(""),
                &row.coverage.label,
            ],
        ) {
            rows.push(row);
        }
    }

    rows.sort_by(|left, right| {
        // Order newest-added first, parsing the stored timestamps to real
        // datetimes so mixed formats (RFC3339, date-only) sort chronologically
        // rather than lexically. Rows without a parseable date sort last.
        compare_added_at_desc(left.added_at.as_deref(), right.added_at.as_deref())
            .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
    });
    rows
}

/// Compares two optional `added_at` strings for a newest-first ordering, parsing
/// them to `DateTime` so ordering is chronological. Unparseable/missing values
/// sort after any real date.
fn compare_added_at_desc(left: Option<&str>, right: Option<&str>) -> Ordering {
    let left = left.and_then(crate::domain::mutate::parse_added_at);
    let right = right.and_then(crate::domain::mutate::parse_added_at);
    match (left, right) {
        (Some(left), Some(right)) => right.cmp(&left),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn track_rows(
    state: &LibraryState,
    query: Option<&str>,
    coverage: Option<&str>,
) -> Vec<TrackListItemDto> {
    let query_filter = normalized_query(query);
    let saved_counts = build_saved_track_counts(state);
    let playlist_ref_counts = build_playlist_reference_counts(state);
    let coverage_filter = normalized_query(coverage);

    let mut rows = state
        .tracks
        .iter()
        .filter_map(|track| {
            let coverage_key = track_coverage_key(track);
            if !coverage_matches(&coverage_key, track, coverage_filter.as_deref()) {
                return None;
            }

            let row = TrackListItemDto {
                track_id: track.id.clone(),
                title: track.metadata.title.clone(),
                artists: track.metadata.artists.clone(),
                artist_summary: track.metadata.artist_summary(),
                album: track.metadata.album.clone(),
                subtitle: track_subtitle(&track.metadata),
                duration_seconds: track.metadata.duration_seconds,
                duration_label: format_duration(track.metadata.duration_seconds),
                isrc: track.metadata.isrc.clone(),
                coverage: coverage_dto(track),
                providers: provider_badges(&track.provider_links),
                status_pills: summarized_status_pills(&[&track.provider_state]),
                saved_count: *saved_counts.get(&track.id).unwrap_or(&0),
                playlist_refs: *playlist_ref_counts.get(&track.id).unwrap_or(&0),
                artwork_url: preferred_artwork_url(track),
            };

            if query_matches(
                query_filter.as_deref(),
                &[
                    &row.title,
                    &row.artist_summary,
                    row.album.as_deref().unwrap_or(""),
                    row.isrc.as_deref().unwrap_or(""),
                    &row.coverage.label,
                ],
            ) {
                Some(row)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    rows.sort_by(|left, right| {
        left.title
            .to_lowercase()
            .cmp(&right.title.to_lowercase())
            .then_with(|| {
                left.artist_summary
                    .to_lowercase()
                    .cmp(&right.artist_summary.to_lowercase())
            })
    });
    rows
}

fn identity_conflict_rows(
    state: &LibraryState,
    query: Option<&str>,
) -> Vec<TrackIdentityConflictQueueItemDto> {
    identity_conflict_rows_filtered(
        state,
        IdentityConflictFilters {
            query,
            ..Default::default()
        },
    )
}

#[derive(Clone, Copy, Default)]
struct IdentityConflictFilters<'a> {
    query: Option<&'a str>,
    provider: Option<ProviderKind>,
    recommendation: Option<&'a str>,
    impact: Option<&'a str>,
}

fn identity_conflict_rows_filtered(
    state: &LibraryState,
    filters: IdentityConflictFilters<'_>,
) -> Vec<TrackIdentityConflictQueueItemDto> {
    let query_filter = normalized_query(filters.query);
    let recommendation_filter = normalized_filter_token(filters.recommendation);
    let impact_filter = normalized_filter_token(filters.impact);
    // Build the per-request lookup maps once so the conflict scan below runs in
    // roughly linear time instead of doing linear owner lookups and count scans
    // per conflict.
    let indexes = ConflictIndexes::build(state);
    let mut rows = Vec::new();

    for track in &state.tracks {
        let source_track = conflict_track_dto(track, &indexes);
        for conflict in identity_conflicts_for_track(track, &indexes) {
            if filters
                .provider
                .map(|provider| conflict.provider != provider.as_key())
                .unwrap_or(false)
            {
                continue;
            }
            if !identity_conflict_recommendation_matches(
                &conflict,
                recommendation_filter.as_deref(),
            ) {
                continue;
            }
            if !identity_conflict_impact_matches(&conflict.evidence, impact_filter.as_deref()) {
                continue;
            }
            if query_matches(
                query_filter.as_deref(),
                &[
                    &source_track.title,
                    &source_track.artist_summary,
                    source_track.album.as_deref().unwrap_or(""),
                    &conflict.provider_name,
                    &conflict.provider_id,
                    &conflict.owner_track.title,
                    &conflict.owner_track.artist_summary,
                    conflict.owner_track.album.as_deref().unwrap_or(""),
                    &conflict.evidence.recommendation.key,
                    &conflict.evidence.recommendation.label,
                    &conflict.evidence.recommendation.detail,
                    &conflict.message,
                ],
            ) {
                rows.push(TrackIdentityConflictQueueItemDto {
                    source_track: source_track.clone(),
                    conflict,
                });
            }
        }
    }

    rows.sort_by(compare_identity_conflict_rows);
    rows
}

fn compare_identity_conflict_rows(
    left: &TrackIdentityConflictQueueItemDto,
    right: &TrackIdentityConflictQueueItemDto,
) -> Ordering {
    identity_conflict_recommendation_priority(&left.conflict.evidence.recommendation.key)
        .cmp(&identity_conflict_recommendation_priority(
            &right.conflict.evidence.recommendation.key,
        ))
        .then_with(|| {
            identity_conflict_library_impact(&right.conflict.evidence)
                .cmp(&identity_conflict_library_impact(&left.conflict.evidence))
        })
        .then_with(|| {
            right
                .conflict
                .evidence
                .metadata_similarity
                .partial_cmp(&left.conflict.evidence.metadata_similarity)
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| {
            left.conflict
                .evidence
                .duration_delta_seconds
                .unwrap_or(u32::MAX)
                .cmp(
                    &right
                        .conflict
                        .evidence
                        .duration_delta_seconds
                        .unwrap_or(u32::MAX),
                )
        })
        .then_with(|| {
            left.source_track
                .title
                .to_lowercase()
                .cmp(&right.source_track.title.to_lowercase())
        })
        .then_with(|| {
            left.source_track
                .artist_summary
                .to_lowercase()
                .cmp(&right.source_track.artist_summary.to_lowercase())
        })
        .then_with(|| left.conflict.provider.cmp(&right.conflict.provider))
        .then_with(|| {
            left.conflict
                .owner_track
                .track_id
                .cmp(&right.conflict.owner_track.track_id)
        })
}

fn identity_conflict_recommendation_priority(key: &str) -> u8 {
    match key {
        "likely_same_recording" => 0,
        "needs_manual_review" => 1,
        "likely_different_recording" => 2,
        _ => 3,
    }
}

fn identity_conflict_library_impact(evidence: &TrackIdentityConflictEvidenceDto) -> usize {
    evidence.source_saved_tracks
        + evidence.source_playlist_entries
        + evidence.candidate_saved_tracks
        + evidence.candidate_playlist_entries
}

fn identity_conflict_recommendation_matches(
    conflict: &TrackIdentityConflictDto,
    filter: Option<&str>,
) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    normalized_filter_token(Some(&conflict.evidence.recommendation.key)).as_deref() == Some(filter)
}

fn identity_conflict_impact_matches(
    evidence: &TrackIdentityConflictEvidenceDto,
    filter: Option<&str>,
) -> bool {
    match filter {
        None => true,
        Some("library_impact") | Some("push_blocking") | Some("affects_library") => {
            identity_conflict_library_impact(evidence) > 0
        }
        Some("source_impact") => {
            evidence.source_saved_tracks + evidence.source_playlist_entries > 0
        }
        Some("candidate_impact") => {
            evidence.candidate_saved_tracks + evidence.candidate_playlist_entries > 0
        }
        _ => true,
    }
}

fn normalized_filter_token(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_lowercase().replace('-', "_"))
}

fn identity_gap_rows(
    state: &LibraryState,
    provider_filter: Option<ProviderKind>,
    query: Option<&str>,
) -> Vec<TrackIdentityGapQueueItemDto> {
    let query_filter = normalized_query(query);
    let providers = provider_filter
        .map(|provider| vec![provider])
        .unwrap_or_else(|| ProviderKind::all().to_vec());
    let indexes = ConflictIndexes::build(state);
    let mut rows = Vec::new();

    for track in &state.tracks {
        let track_dto = conflict_track_dto(track, &indexes);
        for provider in &providers {
            if track.provider_links.contains_key(provider.as_key()) {
                continue;
            }
            if !query_matches(
                query_filter.as_deref(),
                &[
                    &track_dto.title,
                    &track_dto.artist_summary,
                    track_dto.album.as_deref().unwrap_or(""),
                    provider.display_name(),
                ],
            ) {
                continue;
            }

            let push_blocking = track_dto.saved_count > 0 || track_dto.playlist_refs > 0;
            rows.push(TrackIdentityGapQueueItemDto {
                provider: provider.as_key().to_string(),
                provider_name: provider.display_name().to_string(),
                track: track_dto.clone(),
                push_blocking,
            });
        }
    }

    rows.sort_by(|left, right| {
        right
            .push_blocking
            .cmp(&left.push_blocking)
            .then_with(|| right.track.saved_count.cmp(&left.track.saved_count))
            .then_with(|| right.track.playlist_refs.cmp(&left.track.playlist_refs))
            .then_with(|| left.provider.cmp(&right.provider))
            .then_with(|| {
                left.track
                    .title
                    .to_lowercase()
                    .cmp(&right.track.title.to_lowercase())
            })
            .then_with(|| {
                left.track
                    .artist_summary
                    .to_lowercase()
                    .cmp(&right.track.artist_summary.to_lowercase())
            })
    });
    rows
}

/// Per-request lookup maps shared across the identity-conflict/gap builders.
///
/// The conflict path is called once per row and previously did a linear owner
/// lookup and two linear count scans *per conflict*, making a page of conflicts
/// roughly O(n²). Building these once and threading them through keeps the whole
/// path linear:
/// * `saved_counts` / `playlist_ref_counts` — how many saved rows / playlist
///   entries reference each canonical track (used for the DTO impact figures).
/// * `owner_by_provider_id` — the track that currently owns a given
///   `(provider_key, provider_id)`, replacing the per-conflict linear search for
///   the conflict's owner. Provider IDs are unique across tracks (enforced by
///   [`LibraryState::validate`]), so each key maps to exactly one track.
struct ConflictIndexes<'a> {
    saved_counts: BTreeMap<String, usize>,
    playlist_ref_counts: BTreeMap<String, usize>,
    owner_by_provider_id: HashMap<(&'a str, &'a str), &'a TrackEntity>,
}

impl<'a> ConflictIndexes<'a> {
    fn build(state: &'a LibraryState) -> Self {
        let mut owner_by_provider_id = HashMap::new();
        for track in &state.tracks {
            for (provider_key, link) in &track.provider_links {
                owner_by_provider_id
                    .insert((provider_key.as_str(), link.provider_id.as_str()), track);
            }
        }
        Self {
            saved_counts: build_saved_track_counts(state),
            playlist_ref_counts: build_playlist_reference_counts(state),
            owner_by_provider_id,
        }
    }

    fn saved_count(&self, track_id: &str) -> usize {
        self.saved_counts.get(track_id).copied().unwrap_or(0)
    }

    fn playlist_ref_count(&self, track_id: &str) -> usize {
        self.playlist_ref_counts.get(track_id).copied().unwrap_or(0)
    }
}

fn build_track_detail(state: &LibraryState, track_id: &str) -> Result<TrackDetailDto, ApiError> {
    let track = state
        .tracks
        .iter()
        .find(|track| track.id == track_id)
        .ok_or_else(|| ApiError::not_found(format!("Unknown track '{track_id}'.")))?;
    let indexes = ConflictIndexes::build(state);
    let saved_count = indexes.saved_count(&track.id);
    let playlist_refs = indexes.playlist_ref_count(&track.id);

    Ok(TrackDetailDto {
        track_id: track.id.clone(),
        title: track.metadata.title.clone(),
        artists: track.metadata.artists.clone(),
        artist_summary: track.metadata.artist_summary(),
        album: track.metadata.album.clone(),
        duration_seconds: track.metadata.duration_seconds,
        duration_label: format_duration(track.metadata.duration_seconds),
        isrc: track.metadata.isrc.clone(),
        coverage: coverage_dto(track),
        providers: provider_badges(&track.provider_links),
        provider_status: provider_status_details(&track.provider_state),
        identity_conflicts: identity_conflicts_for_track(track, &indexes),
        saved_count,
        playlist_refs,
        artwork_url: preferred_artwork_url(track),
    })
}

fn identity_conflicts_for_track(
    track: &TrackEntity,
    indexes: &ConflictIndexes<'_>,
) -> Vec<TrackIdentityConflictDto> {
    track
        .open_identity_conflicts()
        .filter_map(|conflict| build_identity_conflict_dto(track, conflict, indexes))
        .collect()
}

/// Builds the API DTO for one open typed conflict, resolving the owner track
/// that currently holds the disputed provider ID. Returns `None` when no track
/// owns that ID any more (the conflict is stale and no longer actionable).
fn build_identity_conflict_dto(
    track: &TrackEntity,
    conflict: &TrackIdentityConflict,
    indexes: &ConflictIndexes<'_>,
) -> Option<TrackIdentityConflictDto> {
    let provider = conflict.provider;
    let provider_id = conflict.candidate_provider_id.clone();
    // The disputed provider ID belongs to exactly one track (provider IDs are
    // unique); a conflict against the track's own ID is impossible for an open
    // conflict, so skip it defensively if the index points back at `track`.
    let owner = indexes
        .owner_by_provider_id
        .get(&(provider.as_key(), provider_id.as_str()))
        .copied()
        .filter(|owner| owner.id != track.id)?;

    let message = format!(
        "{} identity '{}' is already linked to {}. Review before merging or reject the candidate.",
        provider.display_name(),
        provider_id,
        owner.metadata.display_label()
    );

    Some(TrackIdentityConflictDto {
        provider: provider.as_key().to_string(),
        provider_name: provider.display_name().to_string(),
        provider_id,
        owner_track: conflict_track_dto(owner, indexes),
        conflicting_provider_links: provider_link_conflicts(track, owner),
        evidence: identity_conflict_evidence(track, owner, conflict.confidence, indexes),
        message,
    })
}

fn conflict_track_dto(track: &TrackEntity, indexes: &ConflictIndexes<'_>) -> ConflictTrackDto {
    ConflictTrackDto {
        track_id: track.id.clone(),
        title: track.metadata.title.clone(),
        artist_summary: track.metadata.artist_summary(),
        album: track.metadata.album.clone(),
        coverage: coverage_dto(track),
        providers: provider_badges(&track.provider_links),
        saved_count: indexes.saved_count(&track.id),
        playlist_refs: indexes.playlist_ref_count(&track.id),
        artwork_url: preferred_artwork_url(track),
    }
}

fn provider_link_conflicts(
    source: &TrackEntity,
    target: &TrackEntity,
) -> Vec<ProviderLinkConflictDto> {
    source
        .provider_links
        .iter()
        .filter_map(|(provider_key, source_link)| {
            let target_link = target.provider_links.get(provider_key)?;
            if target_link.provider_id == source_link.provider_id {
                return None;
            }
            Some(ProviderLinkConflictDto {
                provider: provider_key.clone(),
                provider_name: provider_display_name(provider_key),
                source_provider_id: source_link.provider_id.clone(),
                target_provider_id: target_link.provider_id.clone(),
            })
        })
        .collect()
}

fn identity_conflict_evidence(
    source: &TrackEntity,
    candidate: &TrackEntity,
    provider_confidence: Option<f64>,
    indexes: &ConflictIndexes<'_>,
) -> TrackIdentityConflictEvidenceDto {
    let similarity = metadata_similarity(&source.metadata, &candidate.metadata);
    let duration_delta_seconds = match (
        source.metadata.duration_seconds,
        candidate.metadata.duration_seconds,
    ) {
        (Some(source_duration), Some(candidate_duration)) => {
            Some(source_duration.abs_diff(candidate_duration))
        }
        _ => None,
    };
    let recommendation =
        identity_conflict_recommendation(similarity, duration_delta_seconds, provider_confidence);

    TrackIdentityConflictEvidenceDto {
        provider_confidence,
        metadata_similarity: similarity,
        duration_delta_seconds,
        source_saved_tracks: indexes.saved_count(&source.id),
        source_playlist_entries: indexes.playlist_ref_count(&source.id),
        candidate_saved_tracks: indexes.saved_count(&candidate.id),
        candidate_playlist_entries: indexes.playlist_ref_count(&candidate.id),
        recommendation,
    }
}

fn identity_conflict_recommendation(
    metadata_similarity: f64,
    duration_delta_seconds: Option<u32>,
    provider_confidence: Option<f64>,
) -> TrackIdentityConflictRecommendationDto {
    let close_duration = duration_delta_seconds
        .map(|delta| delta <= 5)
        .unwrap_or(false);
    let incompatible_duration = duration_delta_seconds
        .map(|delta| delta >= 45)
        .unwrap_or(false);
    let provider_high_confidence = provider_confidence
        .map(|confidence| confidence >= 0.98)
        .unwrap_or(false);

    if close_duration
        && (metadata_similarity >= 0.97
            || (provider_high_confidence && metadata_similarity >= 0.94))
    {
        return TrackIdentityConflictRecommendationDto {
            key: "likely_same_recording".to_string(),
            label: "Likely same recording".to_string(),
            detail: "Metadata and duration are strong. Verify the provider IDs, then merge with the provider identity you trust.".to_string(),
        };
    }

    if metadata_similarity < 0.86 || (incompatible_duration && metadata_similarity < 0.95) {
        return TrackIdentityConflictRecommendationDto {
            key: "likely_different_recording".to_string(),
            label: "Likely different recording".to_string(),
            detail: "The rows differ enough that an automatic merge would be unsafe. Inspect both provider tracks or mark the candidate as not the same track.".to_string(),
        };
    }

    TrackIdentityConflictRecommendationDto {
        key: "needs_manual_review".to_string(),
        label: "Needs manual review".to_string(),
        detail: "The evidence is mixed. Compare album, version, duration, and provider pages before merging or rejecting.".to_string(),
    }
}

fn playlist_summaries(state: &LibraryState, query: Option<&str>) -> Vec<PlaylistSummaryDto> {
    let normalized_query = normalized_query(query);
    let track_index = build_track_index(state);
    let mut rows = state
        .playlists
        .iter()
        .filter_map(|playlist| {
            let row = PlaylistSummaryDto {
                playlist_id: playlist.id.clone(),
                name: playlist.name.clone(),
                description: playlist.description.clone(),
                entry_count: playlist.entries.len(),
                providers: provider_badges(&playlist.provider_links),
                status_pills: summarized_status_pills(&[&playlist.provider_state]),
                artwork_url: playlist
                    .entries
                    .first()
                    .and_then(|entry| track_index.get(entry.track_id.as_str()))
                    .and_then(|track| preferred_artwork_url(track)),
            };

            if query_matches(
                normalized_query.as_deref(),
                &[&row.name, row.description.as_deref().unwrap_or("")],
            ) {
                Some(row)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    rows.sort_by_key(|left| left.name.to_lowercase());
    rows
}

fn build_playlist_detail(
    state: &LibraryState,
    playlist_id: &str,
) -> Result<PlaylistDetailDto, ApiError> {
    let track_index = build_track_index(state);
    let playlist = state
        .playlists
        .iter()
        .find(|playlist| playlist.id == playlist_id)
        .ok_or_else(|| ApiError::not_found(format!("Unknown playlist '{playlist_id}'.")))?;

    let playlist_summary = PlaylistSummaryDto {
        playlist_id: playlist.id.clone(),
        name: playlist.name.clone(),
        description: playlist.description.clone(),
        entry_count: playlist.entries.len(),
        providers: provider_badges(&playlist.provider_links),
        status_pills: summarized_status_pills(&[&playlist.provider_state]),
        artwork_url: playlist
            .entries
            .first()
            .and_then(|entry| track_index.get(entry.track_id.as_str()))
            .and_then(|track| preferred_artwork_url(track)),
    };

    let entries = playlist
        .entries
        .iter()
        .filter_map(|entry| {
            let track = track_index.get(entry.track_id.as_str())?;
            Some(PlaylistEntryDto {
                entry_id: entry.id.clone(),
                track_id: track.id.clone(),
                title: track.metadata.title.clone(),
                artists: track.metadata.artists.clone(),
                artist_summary: track.metadata.artist_summary(),
                album: track.metadata.album.clone(),
                subtitle: track_subtitle(&track.metadata),
                added_at: entry.added_at.clone(),
                added_label: format_date(entry.added_at.as_deref())
                    .unwrap_or_else(|| "Unknown".to_string()),
                coverage: coverage_dto(track),
                providers: provider_badges(&track.provider_links),
                status_pills: summarized_status_pills(&[
                    &entry.provider_state,
                    &track.provider_state,
                ]),
                artwork_url: preferred_artwork_url(track),
            })
        })
        .collect::<Vec<_>>();

    Ok(PlaylistDetailDto {
        playlist: playlist_summary,
        entries,
    })
}

fn build_track_index(state: &LibraryState) -> BTreeMap<&str, &TrackEntity> {
    state
        .tracks
        .iter()
        .map(|track| (track.id.as_str(), track))
        .collect()
}

fn indexed_track_has_provider_link(
    track_index: &BTreeMap<&str, &TrackEntity>,
    track_id: &str,
    provider: ProviderKind,
) -> bool {
    track_index
        .get(track_id)
        .map(|track| track.provider_links.contains_key(provider.as_key()))
        .unwrap_or(false)
}

fn build_saved_track_counts(state: &LibraryState) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for entry in &state.saved_tracks {
        *counts.entry(entry.track_id.clone()).or_insert(0) += 1;
    }
    counts
}

fn build_playlist_reference_counts(state: &LibraryState) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for playlist in &state.playlists {
        for entry in &playlist.entries {
            *counts.entry(entry.track_id.clone()).or_insert(0) += 1;
        }
    }
    counts
}

fn playlist_artwork_track_ids(state: &LibraryState, playlist_ids: &[String]) -> Vec<String> {
    state
        .playlists
        .iter()
        .filter(|playlist| playlist_ids.iter().any(|id| id == &playlist.id))
        .filter_map(|playlist| playlist.entries.first().map(|entry| entry.track_id.clone()))
        .collect()
}

fn provider_badges<T>(links: &BTreeMap<String, T>) -> Vec<ProviderBadgeDto>
where
    T: ProviderLinkLike,
{
    links
        .iter()
        .map(|(provider, link)| ProviderBadgeDto {
            key: provider.to_string(),
            label: provider_display_name(provider),
            source: link.source_label().to_string(),
            provider_id: link.provider_id().to_string(),
        })
        .collect()
}

trait ProviderLinkLike {
    fn provider_id(&self) -> &str;
    fn source_label(&self) -> &str;
}

impl ProviderLinkLike for crate::domain::ProviderTrackLink {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    fn source_label(&self) -> &str {
        self.source.as_str()
    }
}

impl ProviderLinkLike for crate::domain::ProviderPlaylistLink {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    fn source_label(&self) -> &str {
        self.source.as_str()
    }
}

fn provider_status_details(
    statuses: &BTreeMap<String, SyncStatusRecord>,
) -> Vec<ProviderStatusDetailDto> {
    statuses
        .iter()
        .map(|(provider, status)| ProviderStatusDetailDto {
            provider: provider_display_name(provider),
            state: status.state.as_str().to_string(),
            message: status.message.clone(),
            provider_item_id: status.provider_item_id.clone(),
            confidence: status.confidence,
            last_attempt_at: status.last_attempt_at.map(|value| value.to_rfc3339()),
            last_success_at: status.last_success_at.map(|value| value.to_rfc3339()),
            last_seen_at: status.last_seen_at.map(|value| value.to_rfc3339()),
        })
        .collect()
}

fn summarized_status_pills(
    status_groups: &[&BTreeMap<String, SyncStatusRecord>],
) -> Vec<StatusPillDto> {
    let statuses = status_groups
        .iter()
        .flat_map(|group| {
            group
                .iter()
                .map(|(provider, status)| (provider.as_str(), status))
        })
        .collect::<Vec<_>>();

    let mut problem_pills = statuses
        .iter()
        .filter(|(_, status)| {
            matches!(
                status.state,
                SyncState::Unmatched | SyncState::Error | SyncState::Missing
            )
        })
        .map(|(provider, status)| StatusPillDto {
            key: status.state.as_str().to_string(),
            label: compact_status_label(status.state).to_string(),
            title: status.message.clone().unwrap_or_else(|| {
                format!(
                    "{} status on {}",
                    status.state,
                    provider_display_name(provider)
                )
            }),
        })
        .collect::<Vec<_>>();

    if !problem_pills.is_empty() {
        problem_pills.truncate(2);
        return problem_pills;
    }

    if statuses
        .iter()
        .any(|(_, status)| status.state == SyncState::Synced)
    {
        return vec![StatusPillDto {
            key: "synced".to_string(),
            label: "Synced".to_string(),
            title: "At least one provider has a synced status for this item.".to_string(),
        }];
    }

    vec![StatusPillDto {
        key: "local".to_string(),
        label: "Local".to_string(),
        title: "No provider sync state has been recorded yet.".to_string(),
    }]
}

fn compact_status_label(state: SyncState) -> &'static str {
    match state {
        SyncState::Unmatched => "Unmatched",
        SyncState::Error => "Error",
        SyncState::Missing => "Missing",
        SyncState::Skipped => "Skipped",
        SyncState::Synced => "Synced",
        SyncState::Pending => "Pending",
    }
}

fn coverage_dto(track: &TrackEntity) -> CoverageDto {
    let key = track_coverage_key(track);
    CoverageDto {
        short_label: compact_coverage_label(&key).to_string(),
        label: coverage_label(&key),
        key,
    }
}

fn compact_coverage_label(key: &str) -> String {
    match key {
        "multi-provider" => "Multi".to_string(),
        "canonical-only" => "Local".to_string(),
        value if value.ends_with("-only") => provider_display_name(value.trim_end_matches("-only")),
        _ => "Local".to_string(),
    }
}

fn track_subtitle(metadata: &TrackMetadata) -> String {
    let mut parts = Vec::new();
    let artist_summary = metadata.artist_summary();
    if !artist_summary.trim().is_empty() {
        parts.push(artist_summary);
    }
    if let Some(album) = metadata
        .album
        .as_deref()
        .filter(|album| !album.trim().is_empty())
    {
        parts.push(album.trim().to_string());
    }
    parts.join(" • ")
}

fn provider_display_name(key: &str) -> String {
    ProviderKind::from_key(key)
        .map(|provider| provider.display_name().to_string())
        .unwrap_or_else(|_| key.to_string())
}

fn track_coverage_key(track: &TrackEntity) -> String {
    match track.provider_links.len() {
        0 => "canonical-only".to_string(),
        1 => format!(
            "{}-only",
            track
                .provider_links
                .keys()
                .next()
                .expect("single provider link should exist")
        ),
        _ => "multi-provider".to_string(),
    }
}

fn coverage_label(key: &str) -> String {
    match key {
        "multi-provider" => "Multi-provider".to_string(),
        "canonical-only" => "Canonical only".to_string(),
        value if value.ends_with("-only") => {
            format!(
                "{} only",
                provider_display_name(value.trim_end_matches("-only"))
            )
        }
        _ => "Canonical only".to_string(),
    }
}

fn coverage_matches(key: &str, track: &TrackEntity, filter: Option<&str>) -> bool {
    match filter {
        None | Some("") => true,
        Some("missing-spotify") => !track
            .provider_links
            .contains_key(ProviderKind::Spotify.as_key()),
        Some("missing-youtube-music") => !track
            .provider_links
            .contains_key(ProviderKind::YoutubeMusic.as_key()),
        Some("missing-any-provider") => ProviderKind::all()
            .iter()
            .any(|provider| !track.provider_links.contains_key(provider.as_key())),
        Some("identity-conflicts") => track_has_identity_conflict(track),
        Some("unmatched") => track
            .provider_state
            .values()
            .any(|status| status.state == SyncState::Unmatched),
        Some(value) => key == value,
    }
}

fn track_has_identity_conflict(track: &TrackEntity) -> bool {
    track.open_identity_conflicts().next().is_some()
}

/// Resolved artwork for one track: which provider it came from, the URL, and
/// its dimensions.
type ResolvedArtwork = (ProviderKind, String, Option<u32>, Option<u32>);

/// Schedules a debounced background pass that fills in missing artwork for the
/// given candidate track IDs. Browse handlers call this after building a page;
/// it never blocks the request. At most one pass runs at a time — if one is
/// already in flight the call is a no-op, and a later browse schedules the next.
fn schedule_artwork_enrichment(context: &Arc<AppContext>, requested: Vec<String>) {
    if requested.is_empty() {
        return;
    }
    // Debounce via the one-permit semaphore: acquire before spawning and hold
    // the permit for the whole pass.
    let Ok(permit) = context.artwork_semaphore.clone().try_acquire_owned() else {
        return;
    };
    let context = context.clone();
    tokio::spawn(async move {
        let _permit = permit;
        run_artwork_enrichment(context, requested).await;
    });
}

/// One background artwork pass: pick the tracks that still need artwork (honoring
/// the negative cache), fetch with bounded concurrency and no lock held, then
/// apply the results onto the current state under a single write lock. Does not
/// bump `library_version` — artwork is self-healing bookkeeping, not a user edit,
/// so it must not trip a concurrent operation's edit detection.
async fn run_artwork_enrichment(context: Arc<AppContext>, requested: Vec<String>) {
    let targets = {
        let state = context.library.read().await;
        collect_artwork_targets(&context, &state, &requested)
    };
    if targets.is_empty() {
        return;
    }

    // Fetch artwork concurrently (bounded), never holding the library lock.
    let permits = Arc::new(Semaphore::new(ARTWORK_FETCH_CONCURRENCY));
    let mut tasks = tokio::task::JoinSet::new();
    for (track_id, spotify_id, youtube_id) in targets {
        let client = context.http_client.clone();
        let permits = permits.clone();
        tasks.spawn(async move {
            let _permit = permits.acquire_owned().await.ok();
            let artwork = resolve_track_artwork(&client, spotify_id, youtube_id).await;
            (track_id, artwork)
        });
    }

    let mut resolved: Vec<(String, ResolvedArtwork)> = Vec::new();
    let mut misses: Vec<String> = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((track_id, Some(artwork))) => resolved.push((track_id, artwork)),
            Ok((track_id, None)) => misses.push(track_id),
            Err(error) => eprintln!("Artwork enrichment task failed: {error}"),
        }
    }

    if !resolved.is_empty() {
        let now = Utc::now();
        let mut state = context.library.write().await;
        let mut changed = false;
        for (track_id, (provider, url, width, height)) in &resolved {
            let still_missing = state
                .tracks
                .iter()
                .find(|track| track.id == *track_id)
                .map(|track| preferred_artwork(track).is_none())
                .unwrap_or(false);
            if still_missing {
                state.upsert_track_artwork(track_id, *provider, url.clone(), *width, *height, now);
                changed = true;
            }
        }
        if changed {
            if let Err(error) = persist_library(&state).await {
                eprintln!("Failed to persist enriched artwork: {}", error.message);
            }
        }
    }

    if !misses.is_empty() {
        let now = Instant::now();
        if let Ok(mut cache) = context.artwork_negative_cache.lock() {
            for track_id in misses {
                cache.insert(track_id, now);
            }
        }
    }
}

/// Selects, from the requested track IDs, those that still lack artwork, have a
/// provider link to fetch from, and are not in the negative cache. Bounded to
/// [`ARTWORK_ENRICHMENT_BATCH`] so one browse request cannot schedule an
/// unbounded run of external lookups.
fn collect_artwork_targets(
    context: &Arc<AppContext>,
    state: &LibraryState,
    requested: &[String],
) -> Vec<(String, Option<String>, Option<String>)> {
    let cache = context.artwork_negative_cache.lock().ok();
    let now = Instant::now();
    let mut seen = std::collections::HashSet::new();
    let mut targets = Vec::new();
    for track_id in requested {
        if targets.len() >= ARTWORK_ENRICHMENT_BATCH {
            break;
        }
        if !seen.insert(track_id.as_str()) {
            continue;
        }
        let Some(track) = state.tracks.iter().find(|track| track.id == *track_id) else {
            continue;
        };
        if preferred_artwork(track).is_some() {
            continue;
        }
        if let Some(cache) = cache.as_ref() {
            if let Some(last) = cache.get(track_id) {
                if negative_cache_is_fresh(*last, now) {
                    continue;
                }
            }
        }
        let spotify_id = track
            .provider_links
            .get(ProviderKind::Spotify.as_key())
            .map(|link| link.provider_id.clone());
        let youtube_id = track
            .provider_links
            .get(ProviderKind::YoutubeMusic.as_key())
            .map(|link| link.provider_id.clone());
        if spotify_id.is_none() && youtube_id.is_none() {
            continue;
        }
        targets.push((track_id.clone(), spotify_id, youtube_id));
    }
    targets
}

/// Whether a negative-cache entry recorded at `last` is still fresh at `now`
/// (younger than [`ARTWORK_NEGATIVE_CACHE_TTL`]), meaning the artwork-less track
/// should be skipped this pass to avoid refetch storms.
fn negative_cache_is_fresh(last: Instant, now: Instant) -> bool {
    now.duration_since(last) < ARTWORK_NEGATIVE_CACHE_TTL
}

/// Resolves artwork for one track: prefers a Spotify oembed lookup (network),
/// falling back to the deterministic YouTube thumbnail. Returns `None` only when
/// there is no artwork to be had (no YouTube link and Spotify yielded nothing),
/// which the caller records in the negative cache.
async fn resolve_track_artwork(
    client: &reqwest::Client,
    spotify_id: Option<String>,
    youtube_id: Option<String>,
) -> Option<ResolvedArtwork> {
    if let Some(spotify_id) = spotify_id {
        match fetch_spotify_oembed_artwork(client, &spotify_id).await {
            Ok(Some((url, width, height))) => {
                return Some((ProviderKind::Spotify, url, width, height))
            }
            Ok(None) => {}
            Err(error) => eprintln!(
                "Artwork lookup failed for Spotify track {spotify_id}: {}",
                error.message
            ),
        }
    }
    if let Some(youtube_id) = youtube_id {
        return Some((
            ProviderKind::YoutubeMusic,
            youtube_thumbnail_url(&youtube_id),
            Some(480),
            Some(360),
        ));
    }
    None
}

async fn fetch_spotify_oembed_artwork(
    client: &reqwest::Client,
    provider_id: &str,
) -> Result<Option<(String, Option<u32>, Option<u32>)>, ApiError> {
    let response = client
        .get("https://open.spotify.com/oembed")
        .query(&[("url", format!("spotify:track:{provider_id}"))])
        .send()
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;

    if !response.status().is_success() {
        return Ok(None);
    }

    let payload: SpotifyOembedResponse = response
        .json()
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    Ok(payload
        .thumbnail_url
        .map(|url| (url, payload.thumbnail_width, payload.thumbnail_height)))
}

fn youtube_thumbnail_url(video_id: &str) -> String {
    format!("https://i.ytimg.com/vi/{video_id}/hqdefault.jpg")
}

fn preferred_artwork_url(track: &TrackEntity) -> Option<String> {
    preferred_artwork(track).map(|artwork| artwork.url.clone())
}

fn preferred_artwork(track: &TrackEntity) -> Option<&ProviderTrackArtwork> {
    track.provider_artwork.values().max_by_key(|artwork| {
        u64::from(artwork.width.unwrap_or(0)) * u64::from(artwork.height.unwrap_or(0))
    })
}

async fn read_schema() -> Result<Vec<SchemaTableDto>, ApiError> {
    tokio::task::spawn_blocking(move || {
        let connection = open_read_only(&storage::library_state_path())?;
        let mut tables = Vec::new();
        for name in list_tables(&connection)? {
            tables.push(SchemaTableDto {
                row_count: count_rows(&connection, &name)? as usize,
                columns: load_columns(&connection, &name)?,
                name,
            });
        }
        Ok::<_, anyhow::Error>(tables)
    })
    .await
    .context("Failed to join schema read task")?
    .map_err(ApiError::from)
}

async fn read_raw_table(table_name: &str, page: usize) -> Result<RawTableDto, ApiError> {
    let table_name = table_name.to_string();
    tokio::task::spawn_blocking(move || {
        let connection = open_read_only(&storage::library_state_path())?;
        ensure_table_exists(&connection, &table_name)?;
        let columns = load_columns(&connection, &table_name)?;
        let total_rows = count_rows(&connection, &table_name)? as usize;
        let query = format!(
            "SELECT * FROM {} LIMIT ?1 OFFSET ?2",
            quote_identifier(&table_name)
        );
        let mut statement = connection.prepare(&query)?;
        let mut rows = statement.query(params![
            RAW_PAGE_SIZE as i64,
            ((page - 1) * RAW_PAGE_SIZE) as i64
        ])?;
        let mut rendered_rows = Vec::new();
        while let Some(row) = rows.next()? {
            let mut cells = Vec::new();
            for (index, column) in columns.iter().enumerate() {
                cells.push(raw_table_value_to_string(
                    &table_name,
                    &column.name,
                    row.get_ref(index)?,
                ));
            }
            rendered_rows.push(cells);
        }
        Ok::<_, anyhow::Error>(RawTableDto {
            name: table_name,
            columns,
            rows: rendered_rows,
            total_rows,
            page,
            page_size: RAW_PAGE_SIZE,
            total_pages: total_pages(total_rows, RAW_PAGE_SIZE),
        })
    })
    .await
    .context("Failed to join raw table read task")?
    .map_err(ApiError::from)
}

fn open_read_only(database_path: &FsPath) -> Result<Connection> {
    Connection::open_with_flags(
        database_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("Failed to open {}", database_path.display()))
}

fn list_tables(connection: &Connection) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT name
         FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    )?;
    let rows = statement.query_map([], |row| row.get(0))?;
    let mut tables = Vec::new();
    for row in rows {
        tables.push(row?);
    }
    Ok(tables)
}

fn ensure_table_exists(connection: &Connection, table_name: &str) -> Result<()> {
    let exists = connection
        .query_row(
            "SELECT 1
             FROM sqlite_master
             WHERE type = 'table' AND name = ?1
             LIMIT 1",
            params![table_name],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some();

    if exists {
        Ok(())
    } else {
        anyhow::bail!("Unknown table '{table_name}'")
    }
}

fn load_columns(connection: &Connection, table_name: &str) -> Result<Vec<SchemaColumnDto>> {
    let pragma = format!("PRAGMA table_info({})", quote_identifier(table_name));
    let mut statement = connection.prepare(&pragma)?;
    let rows = statement.query_map([], |row| {
        Ok(SchemaColumnDto {
            name: row.get(1)?,
            data_type: row.get::<_, String>(2)?,
        })
    })?;
    let mut columns = Vec::new();
    for row in rows {
        columns.push(row?);
    }
    Ok(columns)
}

fn count_rows(connection: &Connection, table_name: &str) -> Result<i64> {
    let query = format!("SELECT COUNT(*) FROM {}", quote_identifier(table_name));
    Ok(connection.query_row(&query, [], |row| row.get(0))?)
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn value_ref_to_string(value: ValueRef<'_>) -> String {
    match value {
        ValueRef::Null => "null".to_string(),
        ValueRef::Integer(value) => value.to_string(),
        ValueRef::Real(value) => value.to_string(),
        ValueRef::Text(value) => String::from_utf8_lossy(value).to_string(),
        ValueRef::Blob(value) => format!("<{} bytes>", value.len()),
    }
}

fn raw_table_value_to_string(table_name: &str, column_name: &str, value: ValueRef<'_>) -> String {
    if table_name == "provider_connections" && column_name == "config_json" {
        return "<redacted provider credentials>".to_string();
    }
    value_ref_to_string(value)
}

fn normalized_page(page: Option<usize>) -> usize {
    page.unwrap_or(1).max(1)
}

fn normalized_query(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase)
}

fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_manual_provider_track_id(
    provider: ProviderKind,
    raw_value: &str,
) -> Result<String, ApiError> {
    let value = raw_value.trim();
    if value.is_empty() {
        return Err(ApiError::bad_request("Provider track ID is required."));
    }

    let provider_id = match provider {
        ProviderKind::Spotify => parse_spotify_track_id(value)?,
        ProviderKind::YoutubeMusic => parse_youtube_music_video_id(value)?,
    };

    Ok(provider_id)
}

fn parse_spotify_track_id(value: &str) -> Result<String, ApiError> {
    let provider_id = if let Some(rest) = value.strip_prefix("spotify:track:") {
        rest.split(':').next().unwrap_or(rest).to_string()
    } else if value.starts_with("http://") || value.starts_with("https://") {
        let url = url::Url::parse(value)
            .map_err(|_| ApiError::bad_request("Spotify URL is not valid."))?;
        let host = url.host_str().unwrap_or_default();
        if host != "open.spotify.com" {
            return Err(ApiError::bad_request(
                "Spotify URL must be from open.spotify.com.",
            ));
        }
        let segments = url
            .path_segments()
            .map(|segments| segments.collect::<Vec<_>>())
            .unwrap_or_default();
        let Some(track_index) = segments.iter().position(|segment| *segment == "track") else {
            return Err(ApiError::bad_request(
                "Spotify URL must point to a track, not an album, artist, or playlist.",
            ));
        };
        segments
            .get(track_index + 1)
            .copied()
            .unwrap_or_default()
            .to_string()
    } else {
        value.to_string()
    };

    if provider_id.len() != 22
        || !provider_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return Err(ApiError::bad_request(
            "Spotify track IDs must be 22 base62 characters or a Spotify track URL.",
        ));
    }

    Ok(provider_id)
}

fn parse_youtube_music_video_id(value: &str) -> Result<String, ApiError> {
    let provider_id = if value.starts_with("http://") || value.starts_with("https://") {
        let url = url::Url::parse(value)
            .map_err(|_| ApiError::bad_request("YouTube Music URL is not valid."))?;
        let host = url.host_str().unwrap_or_default();
        if host == "youtu.be" {
            url.path_segments()
                .and_then(|mut segments| segments.next())
                .unwrap_or_default()
                .to_string()
        } else if matches!(
            host,
            "music.youtube.com" | "www.youtube.com" | "youtube.com" | "m.youtube.com"
        ) {
            url.query_pairs()
                .find(|(key, _)| key == "v")
                .map(|(_, value)| value.into_owned())
                .unwrap_or_default()
        } else {
            return Err(ApiError::bad_request(
                "YouTube Music URL must be from music.youtube.com, youtube.com, or youtu.be.",
            ));
        }
    } else {
        value.to_string()
    };

    if !(3..=64).contains(&provider_id.len())
        || !provider_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(ApiError::bad_request(
            "YouTube Music video IDs must use only letters, numbers, '-' or '_'.",
        ));
    }

    Ok(provider_id)
}

fn query_matches(query: Option<&str>, parts: &[&str]) -> bool {
    match query {
        None => true,
        Some(query) => parts.iter().any(|part| part.to_lowercase().contains(query)),
    }
}

fn paginate_vec<T: Clone>(rows: &[T], page: usize, page_size: usize) -> Vec<T> {
    let start = (page - 1) * page_size;
    rows.iter().skip(start).take(page_size).cloned().collect()
}

fn total_pages(total_rows: usize, page_size: usize) -> usize {
    if total_rows == 0 {
        0
    } else {
        total_rows.div_ceil(page_size)
    }
}

fn format_duration(duration_seconds: Option<u32>) -> String {
    match duration_seconds {
        None => "--:--".to_string(),
        Some(value) => format!("{}:{:02}", value / 60, value % 60),
    }
}

fn format_date(value: Option<&str>) -> Option<String> {
    let value = value?;
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed.format("%b %-d, %Y").to_string());
    }
    Some(value.to_string())
}

fn ensure_frontend_dist() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let frontend_dir = manifest_dir.join("frontend");
    let source_dist_dir = frontend_dir.join("dist");

    for dist_dir in frontend_dist_candidates(&manifest_dir) {
        if dist_dir.join("index.html").exists() {
            return Ok(dist_dir);
        }
    }

    build_frontend(&manifest_dir, &frontend_dir)?;

    if source_dist_dir.join("index.html").exists() {
        Ok(source_dist_dir)
    } else {
        anyhow::bail!(
            "Frontend bundle missing at {} even after build.",
            source_dist_dir.display()
        )
    }
}

fn frontend_dist_candidates(manifest_dir: &FsPath) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(configured) = std::env::var_os("SPOTI_DUMP_FRONTEND_DIST") {
        candidates.push(PathBuf::from(configured));
    }
    if let Ok(executable) = std::env::current_exe() {
        if let Some(parent) = executable.parent() {
            candidates.push(parent.join("frontend").join("dist"));
        }
    }
    candidates.push(manifest_dir.join("frontend").join("dist"));
    candidates
}

fn build_frontend(manifest_dir: &FsPath, frontend_dir: &FsPath) -> Result<()> {
    let path = build_frontend_path(manifest_dir);
    let npm_binary = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let status = Command::new(npm_binary)
        .current_dir(frontend_dir)
        .arg("run")
        .arg("build")
        .env("PATH", path)
        .status()
        .with_context(|| format!("Failed to build frontend in {}", frontend_dir.display()))?;

    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("Frontend build failed with status {status}")
    }
}

fn build_frontend_path(manifest_dir: &FsPath) -> OsString {
    let mut paths = Vec::new();
    let bundled_node = manifest_dir.join(".tools").join("node").join("bin");
    if bundled_node.exists() {
        paths.push(bundled_node);
    }
    if let Some(existing) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(paths).unwrap_or_else(|_| std::env::var_os("PATH").unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::Utc;
    use rusqlite::types::ValueRef;

    use crate::domain::{
        LibraryState, LinkSource, PlaylistEntity, PlaylistEntry, ProviderConnection,
        ProviderConnectionConfig, ProviderKind, ProviderTrackLink, SavedTrackEntry,
        SpotifyConnectionConfig, TrackEntity, TrackMetadata,
    };

    use super::{
        bulk_merge_identity_conflict_rows, compare_added_at_desc, coverage_matches,
        identity_conflict_rows, identity_conflict_rows_filtered, identity_gap_rows,
        is_request_origin_allowed, negative_cache_is_fresh, normalize_manual_provider_track_id,
        provider_health_failed, provider_preflight_payload, provider_push_plan_payload,
        raw_table_value_to_string, reapply_provider_sync, saved_track_rows,
        IdentityConflictFilters,
    };
    use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method};
    use std::time::{Duration, Instant};

    /// Builds a `HeaderMap` from `(name, value)` pairs for the guard tests.
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

    #[test]
    fn redacts_provider_credentials_from_raw_database_browser() {
        assert_eq!(
            raw_table_value_to_string(
                "provider_connections",
                "config_json",
                ValueRef::Text(br#"{"refresh_token":"secret"}"#),
            ),
            "<redacted provider credentials>"
        );
        assert_eq!(
            raw_table_value_to_string("tracks", "title", ValueRef::Text(b"Sirius")),
            "Sirius"
        );
    }

    #[test]
    fn normalizes_manual_spotify_track_ids_and_rejects_non_track_urls() {
        assert_eq!(
            normalize_manual_provider_track_id(
                ProviderKind::Spotify,
                "https://open.spotify.com/track/2ZrSFxVUAvbgVMqYTkfx3B?si=abc"
            )
            .unwrap(),
            "2ZrSFxVUAvbgVMqYTkfx3B"
        );
        assert_eq!(
            normalize_manual_provider_track_id(
                ProviderKind::Spotify,
                "spotify:track:2ZrSFxVUAvbgVMqYTkfx3B"
            )
            .unwrap(),
            "2ZrSFxVUAvbgVMqYTkfx3B"
        );
        assert!(normalize_manual_provider_track_id(
            ProviderKind::Spotify,
            "https://open.spotify.com/playlist/2ZrSFxVUAvbgVMqYTkfx3B"
        )
        .is_err());
    }

    #[test]
    fn normalizes_manual_youtube_music_video_ids() {
        assert_eq!(
            normalize_manual_provider_track_id(
                ProviderKind::YoutubeMusic,
                "https://music.youtube.com/watch?v=O3FrSTTpZ_U&list=RDAMVM"
            )
            .unwrap(),
            "O3FrSTTpZ_U"
        );
        assert_eq!(
            normalize_manual_provider_track_id(
                ProviderKind::YoutubeMusic,
                "https://youtu.be/O3FrSTTpZ_U"
            )
            .unwrap(),
            "O3FrSTTpZ_U"
        );
        assert!(normalize_manual_provider_track_id(
            ProviderKind::YoutubeMusic,
            "https://example.com/watch?v=O3FrSTTpZ_U"
        )
        .is_err());
    }

    #[test]
    fn coverage_filters_find_missing_provider_identities() {
        let now = Utc::now();
        let canonical_only = test_track("track-canonical", "Canonical Only");
        let spotify_only = test_track_with_link(
            "track-spotify",
            "Spotify Only",
            ProviderKind::Spotify,
            "spotify-track-1",
            now,
        );
        let youtube_only = test_track_with_link(
            "track-youtube",
            "YouTube Only",
            ProviderKind::YoutubeMusic,
            "youtube-video-1",
            now,
        );
        let mut conflict = test_track("track-conflict", "Conflict");
        assert!(conflict.record_identity_conflict(
            ProviderKind::Spotify,
            "spotify-candidate",
            Some(0.9),
            now,
        ));
        let mut multi_provider = test_track_with_link(
            "track-both",
            "Multi-provider",
            ProviderKind::Spotify,
            "spotify-track-2",
            now,
        );
        multi_provider.provider_links.insert(
            ProviderKind::YoutubeMusic.as_key().to_string(),
            ProviderTrackLink {
                provider_id: "youtube-video-2".to_string(),
                source: LinkSource::Export,
                confidence: Some(1.0),
                linked_at: now,
                last_seen_at: Some(now),
            },
        );

        assert!(coverage_matches(
            "canonical-only",
            &canonical_only,
            Some("missing-any-provider")
        ));
        assert!(coverage_matches(
            "spotify-only",
            &spotify_only,
            Some("missing-youtube-music")
        ));
        assert!(coverage_matches(
            "youtube-music-only",
            &youtube_only,
            Some("missing-spotify")
        ));
        assert!(coverage_matches(
            "canonical-only",
            &conflict,
            Some("identity-conflicts")
        ));
        assert!(!coverage_matches(
            "multi-provider",
            &multi_provider,
            Some("missing-any-provider")
        ));
        assert!(!coverage_matches(
            "spotify-only",
            &spotify_only,
            Some("identity-conflicts")
        ));
        assert!(!coverage_matches(
            "spotify-only",
            &spotify_only,
            Some("missing-spotify")
        ));
    }

    #[test]
    fn identity_conflict_queue_includes_source_owner_and_provider_id_differences() {
        let now = Utc::now();
        let mut source = test_track_with_link(
            "track-source",
            "Conflict",
            ProviderKind::Spotify,
            "spotify-source",
            now,
        );
        source.metadata.album = Some("Same Album".to_string());
        source.metadata.duration_seconds = Some(180);
        assert!(source.record_identity_conflict(
            ProviderKind::YoutubeMusic,
            "youtube-owner",
            Some(0.99),
            now,
        ));

        let mut owner = test_track_with_link(
            "track-owner",
            "Conflict",
            ProviderKind::Spotify,
            "spotify-owner",
            now,
        );
        owner.metadata.album = Some("Same Album".to_string());
        owner.metadata.duration_seconds = Some(182);
        owner.provider_links.insert(
            ProviderKind::YoutubeMusic.as_key().to_string(),
            ProviderTrackLink {
                provider_id: "youtube-owner".to_string(),
                source: LinkSource::Export,
                confidence: Some(1.0),
                linked_at: now,
                last_seen_at: Some(now),
            },
        );

        let mut state = LibraryState::new();
        state.tracks.push(source);
        state.tracks.push(owner);

        let rows = identity_conflict_rows(&state, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source_track.track_id, "track-source");
        assert_eq!(
            rows[0].conflict.provider,
            ProviderKind::YoutubeMusic.as_key()
        );
        assert_eq!(rows[0].conflict.provider_id, "youtube-owner");
        assert_eq!(rows[0].conflict.owner_track.track_id, "track-owner");
        assert_eq!(rows[0].conflict.conflicting_provider_links.len(), 1);
        assert_eq!(
            rows[0].conflict.conflicting_provider_links[0].source_provider_id,
            "spotify-source"
        );
        assert_eq!(
            rows[0].conflict.conflicting_provider_links[0].target_provider_id,
            "spotify-owner"
        );
        assert_eq!(rows[0].conflict.evidence.provider_confidence, Some(0.99));
        assert_eq!(rows[0].conflict.evidence.duration_delta_seconds, Some(2));
        assert_eq!(
            rows[0].conflict.evidence.recommendation.key,
            "likely_same_recording"
        );
        assert!(rows[0].conflict.evidence.metadata_similarity >= 0.97);
    }

    #[test]
    fn identity_conflict_queue_omits_rejected_candidates() {
        let now = Utc::now();
        let mut source = test_track_with_link(
            "track-source",
            "Conflict",
            ProviderKind::Spotify,
            "spotify-source",
            now,
        );
        // The candidate was reviewed and rejected: a typed tombstone, which the
        // conflict queue and the identity-conflict coverage filter both ignore.
        assert!(source.record_identity_conflict(
            ProviderKind::YoutubeMusic,
            "youtube-owner",
            None,
            now,
        ));
        assert!(source.reject_identity_conflict(ProviderKind::YoutubeMusic, "youtube-owner", now));

        let mut owner = test_track_with_link(
            "track-owner",
            "Conflict",
            ProviderKind::Spotify,
            "spotify-owner",
            now,
        );
        owner.provider_links.insert(
            ProviderKind::YoutubeMusic.as_key().to_string(),
            ProviderTrackLink {
                provider_id: "youtube-owner".to_string(),
                source: LinkSource::Export,
                confidence: Some(1.0),
                linked_at: now,
                last_seen_at: Some(now),
            },
        );

        let mut state = LibraryState::new();
        state.tracks.push(source);
        state.tracks.push(owner);

        assert!(identity_conflict_rows(&state, None).is_empty());
        assert!(!coverage_matches(
            "spotify-only",
            &state.tracks[0],
            Some("identity-conflicts")
        ));
        // Re-detecting the rejected candidate must not resurrect it.
        assert!(!state.tracks[0].record_identity_conflict(
            ProviderKind::YoutubeMusic,
            "youtube-owner",
            None,
            now,
        ));
        assert!(identity_conflict_rows(&state, None).is_empty());
    }

    #[test]
    fn identity_conflict_evidence_flags_likely_different_recordings() {
        let now = Utc::now();
        let mut source = test_track_with_link(
            "track-source",
            "Short Theme",
            ProviderKind::Spotify,
            "spotify-source",
            now,
        );
        source.metadata.duration_seconds = Some(90);
        assert!(source.record_identity_conflict(
            ProviderKind::YoutubeMusic,
            "youtube-owner",
            Some(0.88),
            now,
        ));

        let mut owner = test_track_with_link(
            "track-owner",
            "Long Theme",
            ProviderKind::Spotify,
            "spotify-owner",
            now,
        );
        owner.metadata.duration_seconds = Some(240);
        owner.provider_links.insert(
            ProviderKind::YoutubeMusic.as_key().to_string(),
            ProviderTrackLink {
                provider_id: "youtube-owner".to_string(),
                source: LinkSource::Export,
                confidence: Some(1.0),
                linked_at: now,
                last_seen_at: Some(now),
            },
        );

        let mut state = LibraryState::new();
        state.tracks.push(source);
        state.tracks.push(owner);

        let rows = identity_conflict_rows(&state, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].conflict.evidence.duration_delta_seconds, Some(150));
        assert_eq!(
            rows[0].conflict.evidence.recommendation.key,
            "likely_different_recording"
        );
    }

    #[test]
    fn identity_conflict_queue_filters_and_prioritizes_by_review_evidence() {
        let now = Utc::now();
        let mut state = LibraryState::new();

        let (manual_source, manual_owner) = identity_conflict_pair(IdentityConflictPairFixture {
            source_id: "track-manual-source",
            source_title: "Alpha Theme",
            owner_id: "track-manual-owner",
            owner_title: "Alpha Theme Deluxe",
            candidate_provider: ProviderKind::YoutubeMusic,
            candidate_provider_id: "youtube-manual",
            source_duration_seconds: Some(120),
            owner_duration_seconds: Some(121),
            confidence: Some(0.80),
            now,
        });
        let (different_source, different_owner) =
            identity_conflict_pair(IdentityConflictPairFixture {
                source_id: "track-different-source",
                source_title: "Beta Short",
                owner_id: "track-different-owner",
                owner_title: "Gamma Long",
                candidate_provider: ProviderKind::YoutubeMusic,
                candidate_provider_id: "youtube-different",
                source_duration_seconds: Some(90),
                owner_duration_seconds: Some(240),
                confidence: Some(0.80),
                now,
            });
        let (mut likely_source, mut likely_owner) =
            identity_conflict_pair(IdentityConflictPairFixture {
                source_id: "track-likely-source",
                source_title: "Zulu Same",
                owner_id: "track-likely-owner",
                owner_title: "Zulu Same",
                candidate_provider: ProviderKind::YoutubeMusic,
                candidate_provider_id: "youtube-likely",
                source_duration_seconds: Some(180),
                owner_duration_seconds: Some(181),
                confidence: Some(0.99),
                now,
            });
        likely_source.metadata.album = Some("Same Album".to_string());
        likely_owner.metadata.album = Some("Same Album".to_string());

        state.tracks.push(manual_source);
        state.tracks.push(manual_owner);
        state.tracks.push(different_source);
        state.tracks.push(different_owner);
        state.tracks.push(likely_source);
        state.tracks.push(likely_owner);
        state.saved_tracks.push(SavedTrackEntry {
            id: "saved-likely-source".to_string(),
            track_id: "track-likely-source".to_string(),
            added_at: None,
            provider_state: BTreeMap::new(),
        });

        let rows = identity_conflict_rows(&state, None);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].source_track.track_id, "track-likely-source");
        assert_eq!(
            rows[0].conflict.evidence.recommendation.key,
            "likely_same_recording"
        );

        let likely_rows = identity_conflict_rows_filtered(
            &state,
            IdentityConflictFilters {
                recommendation: Some("likely-same-recording"),
                ..Default::default()
            },
        );
        assert_eq!(likely_rows.len(), 1);
        assert_eq!(likely_rows[0].source_track.track_id, "track-likely-source");

        let impact_rows = identity_conflict_rows_filtered(
            &state,
            IdentityConflictFilters {
                impact: Some("library_impact"),
                ..Default::default()
            },
        );
        assert_eq!(impact_rows.len(), 1);
        assert_eq!(impact_rows[0].source_track.track_id, "track-likely-source");

        let spotify_candidate_rows = identity_conflict_rows_filtered(
            &state,
            IdentityConflictFilters {
                provider: Some(ProviderKind::Spotify),
                ..Default::default()
            },
        );
        assert!(spotify_candidate_rows.is_empty());

        let bulk_rows = bulk_merge_identity_conflict_rows(
            &state,
            None,
            Some(ProviderKind::YoutubeMusic),
            Some("library-impact"),
        );
        assert_eq!(bulk_rows.len(), 1);
        assert_eq!(bulk_rows[0].source_track.track_id, "track-likely-source");
    }

    #[test]
    fn identity_gap_queue_filters_provider_and_prioritizes_push_blocking_rows() {
        let now = Utc::now();
        let mut state = LibraryState::new();
        state.tracks.push(test_track_with_link(
            "unused-spotify-only",
            "Unused",
            ProviderKind::Spotify,
            "spotify-unused",
            now,
        ));
        state.tracks.push(test_track_with_link(
            "saved-spotify-only",
            "Saved Missing",
            ProviderKind::Spotify,
            "spotify-saved",
            now,
        ));
        state.tracks.push(test_track_with_link(
            "playlist-youtube-only",
            "Playlist Missing",
            ProviderKind::YoutubeMusic,
            "youtube-playlist",
            now,
        ));
        state.saved_tracks.push(SavedTrackEntry {
            id: "saved-1".to_string(),
            track_id: "saved-spotify-only".to_string(),
            added_at: None,
            provider_state: BTreeMap::new(),
        });
        state.playlists.push(PlaylistEntity {
            id: "playlist-1".to_string(),
            name: "Favorites".to_string(),
            description: None,
            provider_links: BTreeMap::new(),
            provider_state: BTreeMap::new(),
            entries: vec![PlaylistEntry {
                id: "entry-1".to_string(),
                track_id: "playlist-youtube-only".to_string(),
                added_at: None,
                provider_state: BTreeMap::new(),
            }],
        });

        let youtube_gaps = identity_gap_rows(&state, Some(ProviderKind::YoutubeMusic), None);
        assert_eq!(youtube_gaps.len(), 2);
        assert_eq!(youtube_gaps[0].track.track_id, "saved-spotify-only");
        assert!(youtube_gaps[0].push_blocking);
        assert_eq!(youtube_gaps[1].track.track_id, "unused-spotify-only");
        assert!(!youtube_gaps[1].push_blocking);

        let spotify_gaps = identity_gap_rows(&state, Some(ProviderKind::Spotify), None);
        assert_eq!(spotify_gaps.len(), 1);
        assert_eq!(spotify_gaps[0].track.track_id, "playlist-youtube-only");
        assert!(spotify_gaps[0].push_blocking);

        let searched = identity_gap_rows(&state, None, Some("playlist"));
        assert_eq!(searched.len(), 1);
        assert_eq!(searched[0].provider, ProviderKind::Spotify.as_key());
    }

    #[test]
    fn preflight_counts_pushable_and_missing_provider_identities() {
        let now = Utc::now();
        let mut linked_track = test_track("track-linked", "Sirius");
        linked_track.provider_links.insert(
            ProviderKind::Spotify.as_key().to_string(),
            ProviderTrackLink {
                provider_id: "spotify-track-1".to_string(),
                source: LinkSource::Export,
                confidence: Some(1.0),
                linked_at: now,
                last_seen_at: Some(now),
            },
        );

        let mut state = LibraryState::new();
        state.tracks.push(linked_track);
        state
            .tracks
            .push(test_track("track-missing", "No Spotify ID"));
        state.saved_tracks.push(SavedTrackEntry {
            id: "saved-linked".to_string(),
            track_id: "track-linked".to_string(),
            added_at: None,
            provider_state: BTreeMap::new(),
        });
        state.saved_tracks.push(SavedTrackEntry {
            id: "saved-missing".to_string(),
            track_id: "track-missing".to_string(),
            added_at: None,
            provider_state: BTreeMap::new(),
        });
        state.playlists.push(PlaylistEntity {
            id: "playlist-1".to_string(),
            name: "Favorites".to_string(),
            description: None,
            provider_links: BTreeMap::new(),
            provider_state: BTreeMap::new(),
            entries: vec![
                PlaylistEntry {
                    id: "entry-linked".to_string(),
                    track_id: "track-linked".to_string(),
                    added_at: None,
                    provider_state: BTreeMap::new(),
                },
                PlaylistEntry {
                    id: "entry-missing".to_string(),
                    track_id: "track-missing".to_string(),
                    added_at: None,
                    provider_state: BTreeMap::new(),
                },
            ],
        });
        let connection = ProviderConnection {
            provider: ProviderKind::Spotify,
            connected_at: now,
            updated_at: now,
            config: ProviderConnectionConfig::Spotify(SpotifyConnectionConfig {
                client_id: "client".to_string(),
                client_secret: "secret".to_string(),
                refresh_token: "refresh".to_string(),
            }),
        };

        let identity_conflicts = identity_conflict_rows(&state, None).len();
        let preflight = provider_preflight_payload(
            &state,
            ProviderKind::Spotify,
            Some(&connection),
            None,
            None,
            identity_conflicts,
        );

        assert!(preflight.can_push);
        assert!(!preflight.can_reset_push);
        assert_eq!(preflight.saved_tracks_total, 2);
        assert_eq!(preflight.saved_tracks_pushable, 1);
        assert_eq!(preflight.saved_tracks_missing_identity, 1);
        assert_eq!(preflight.playlist_entries_total, 2);
        assert_eq!(preflight.playlist_entries_pushable, 1);
        assert_eq!(preflight.playlist_entries_missing_identity, 1);
        assert_eq!(preflight.track_ids_missing, 1);
        assert!(preflight.blockers.is_empty());
        assert!(preflight
            .warnings
            .iter()
            .any(|warning| warning.contains("saved tracks")));
        assert!(preflight
            .reset_blockers
            .iter()
            .any(|blocker| blocker.contains("would be skipped after purging Spotify")));

        let failed_health =
            provider_health_failed(ProviderKind::Spotify, "Stored Spotify token is invalid.");
        let blocked = provider_preflight_payload(
            &state,
            ProviderKind::Spotify,
            Some(&connection),
            None,
            Some(&failed_health),
            identity_conflicts,
        );
        assert!(!blocked.can_pull);
        assert!(!blocked.can_push);
        assert!(blocked
            .blockers
            .iter()
            .any(|blocker| blocker.contains("connection check failed")));
    }

    #[test]
    fn push_plan_matches_preflight_and_lists_skipped_identity_examples() {
        let now = Utc::now();
        let mut linked_track = test_track("track-linked", "Sirius");
        linked_track.provider_links.insert(
            ProviderKind::Spotify.as_key().to_string(),
            ProviderTrackLink {
                provider_id: "spotify-track-1".to_string(),
                source: LinkSource::Export,
                confidence: Some(1.0),
                linked_at: now,
                last_seen_at: Some(now),
            },
        );

        let mut state = LibraryState::new();
        state.tracks.push(linked_track);
        state
            .tracks
            .push(test_track("track-missing", "No Spotify ID"));
        state.saved_tracks.push(SavedTrackEntry {
            id: "saved-linked".to_string(),
            track_id: "track-linked".to_string(),
            added_at: None,
            provider_state: BTreeMap::new(),
        });
        state.saved_tracks.push(SavedTrackEntry {
            id: "saved-missing".to_string(),
            track_id: "track-missing".to_string(),
            added_at: None,
            provider_state: BTreeMap::new(),
        });
        state.playlists.push(PlaylistEntity {
            id: "playlist-1".to_string(),
            name: "Favorites".to_string(),
            description: None,
            provider_links: BTreeMap::new(),
            provider_state: BTreeMap::new(),
            entries: vec![
                PlaylistEntry {
                    id: "entry-linked".to_string(),
                    track_id: "track-linked".to_string(),
                    added_at: None,
                    provider_state: BTreeMap::new(),
                },
                PlaylistEntry {
                    id: "entry-missing".to_string(),
                    track_id: "track-missing".to_string(),
                    added_at: None,
                    provider_state: BTreeMap::new(),
                },
            ],
        });
        let connection = ProviderConnection {
            provider: ProviderKind::Spotify,
            connected_at: now,
            updated_at: now,
            config: ProviderConnectionConfig::Spotify(SpotifyConnectionConfig {
                client_id: "client".to_string(),
                client_secret: "secret".to_string(),
                refresh_token: "refresh".to_string(),
            }),
        };

        let plan = provider_push_plan_payload(
            &state,
            ProviderKind::Spotify,
            Some(&connection),
            None,
            None,
        );

        assert!(plan.preflight.can_push);
        assert_eq!(plan.saved_tracks.total, 2);
        assert_eq!(plan.saved_tracks.pushable, 1);
        assert_eq!(plan.saved_tracks.skipped_missing_identity, 1);
        assert_eq!(
            plan.saved_tracks.skipped_examples[0].track_id,
            "track-missing"
        );
        assert_eq!(plan.playlist_entries.total, 2);
        assert_eq!(plan.playlist_entries.pushable, 1);
        assert_eq!(plan.playlist_entries.skipped_missing_identity, 1);
        assert_eq!(plan.playlists.total, 1);
        assert_eq!(plan.playlists.linked, 0);
        assert_eq!(plan.playlists.unlinked, 1);
        assert_eq!(plan.playlists.examples[0].missing_entries, 1);
    }

    #[test]
    fn compare_added_at_desc_orders_chronologically_not_lexically() {
        use std::cmp::Ordering;
        // Lexically "2023-09..." > "2023-10..." ('9' > '1'); chronologically the
        // October timestamp is newer and must sort first (newest-first).
        assert_eq!(
            compare_added_at_desc(Some("2023-10-01T00:00:00Z"), Some("2023-09-01T00:00:00Z")),
            Ordering::Less
        );
        // Mixed formats (date-only vs RFC3339) still compare by instant.
        assert_eq!(
            compare_added_at_desc(Some("2024-01-02"), Some("2024-01-01T00:00:00Z")),
            Ordering::Less
        );
        // Missing / unparseable dates sort after any real date.
        assert_eq!(
            compare_added_at_desc(Some("2024-01-01T00:00:00Z"), None),
            Ordering::Less
        );
        assert_eq!(
            compare_added_at_desc(None, Some("2024-01-01T00:00:00Z")),
            Ordering::Greater
        );
        assert_eq!(compare_added_at_desc(None, None), Ordering::Equal);
    }

    #[test]
    fn saved_track_rows_sort_newest_added_first_across_date_formats() {
        let mut state = LibraryState::new();
        state.tracks.push(test_track("track-a", "Alpha"));
        state.tracks.push(test_track("track-b", "Bravo"));
        state.tracks.push(test_track("track-c", "Charlie"));
        // Out of order, mixed formats. Lexically "2023-09..." sorts after
        // "2023-10...", so a string sort would put track-a before track-b.
        state.saved_tracks.push(saved_entry(
            "saved-a",
            "track-a",
            Some("2023-09-15T00:00:00Z"),
        ));
        state
            .saved_tracks
            .push(saved_entry("saved-b", "track-b", Some("2023-10-01")));
        state
            .saved_tracks
            .push(saved_entry("saved-c", "track-c", None));

        let rows = saved_track_rows(&state, None);
        let ordered: Vec<&str> = rows.iter().map(|row| row.track_id.as_str()).collect();
        // Newest real date first (October), then September, then the undated row.
        assert_eq!(ordered, ["track-b", "track-a", "track-c"]);
    }

    #[test]
    fn artwork_negative_cache_gate_skips_only_within_ttl() {
        let last = Instant::now();
        // A fresh miss (within the 1h TTL) is skipped.
        assert!(negative_cache_is_fresh(
            last,
            last + Duration::from_secs(30)
        ));
        assert!(negative_cache_is_fresh(
            last,
            last + Duration::from_secs(60 * 59)
        ));
        // Once older than the TTL the track becomes eligible for a refetch.
        assert!(!negative_cache_is_fresh(
            last,
            last + Duration::from_secs(60 * 60 + 1)
        ));
        assert!(!negative_cache_is_fresh(
            last,
            last + Duration::from_secs(3 * 60 * 60)
        ));
    }

    #[test]
    fn provider_sync_reapply_preserves_concurrent_user_edits() {
        let now = Utc::now();
        // Base track: linked to YouTube Music, no Spotify link yet.
        let base_track =
            test_track_with_link("track-1", "Song", ProviderKind::YoutubeMusic, "yt-1", now);

        // The background push (run against a detached clone) resolves Spotify.
        let mut working = LibraryState::new();
        working.tracks.push(base_track.clone());
        working.tracks[0].provider_links.insert(
            ProviderKind::Spotify.as_key().to_string(),
            ProviderTrackLink {
                provider_id: "sp-1".to_string(),
                source: LinkSource::Match,
                confidence: Some(1.0),
                linked_at: now,
                last_seen_at: Some(now),
            },
        );

        // Meanwhile the user edits the live state: renames the track.
        let mut current = LibraryState::new();
        current.tracks.push(base_track);
        current.tracks[0].metadata.title = "Renamed by user".to_string();

        reapply_provider_sync(&mut current, &working, ProviderKind::Spotify);

        // The push result (Spotify link) landed,
        assert!(current.tracks[0]
            .provider_links
            .contains_key(ProviderKind::Spotify.as_key()));
        // the pre-existing YouTube link survived,
        assert!(current.tracks[0]
            .provider_links
            .contains_key(ProviderKind::YoutubeMusic.as_key()));
        // and the concurrent user rename was not clobbered.
        assert_eq!(current.tracks[0].metadata.title, "Renamed by user");
    }

    #[test]
    fn provider_sync_reapply_removes_links_dropped_by_a_reset() {
        let now = Utc::now();
        // Current state has a Spotify link; the reset clone dropped it (post-purge).
        let mut current = LibraryState::new();
        current.tracks.push(test_track_with_link(
            "track-1",
            "Song",
            ProviderKind::Spotify,
            "sp-1",
            now,
        ));
        let mut working = current.clone();
        working.tracks[0]
            .provider_links
            .remove(ProviderKind::Spotify.as_key());

        reapply_provider_sync(&mut current, &working, ProviderKind::Spotify);
        assert!(!current.tracks[0]
            .provider_links
            .contains_key(ProviderKind::Spotify.as_key()));
    }

    fn saved_entry(id: &str, track_id: &str, added_at: Option<&str>) -> SavedTrackEntry {
        SavedTrackEntry {
            id: id.to_string(),
            track_id: track_id.to_string(),
            added_at: added_at.map(str::to_string),
            provider_state: BTreeMap::new(),
        }
    }

    fn test_track(id: &str, title: &str) -> TrackEntity {
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

    struct IdentityConflictPairFixture<'a> {
        source_id: &'a str,
        source_title: &'a str,
        owner_id: &'a str,
        owner_title: &'a str,
        candidate_provider: ProviderKind,
        candidate_provider_id: &'a str,
        source_duration_seconds: Option<u32>,
        owner_duration_seconds: Option<u32>,
        confidence: Option<f64>,
        now: chrono::DateTime<Utc>,
    }

    fn identity_conflict_pair(
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

    fn test_track_with_link(
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
