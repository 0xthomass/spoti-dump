use serde::{Deserialize, Serialize};

use crate::domain::ProviderKind;
use crate::web::operations::{OperationKind, OperationStatus};

#[derive(Serialize)]
pub(crate) struct HealthResponse {
    pub(crate) status: &'static str,
    pub(crate) database_path: String,
    pub(crate) integrity_check: String,
    pub(crate) tracks: usize,
    pub(crate) saved_tracks: usize,
    pub(crate) playlists: usize,
    pub(crate) playlist_entries: usize,
    pub(crate) durable_operation_history: bool,
}

#[derive(Serialize)]
pub(crate) struct OperationStartResponse {
    pub(crate) operation_id: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct OperationResponse {
    pub(crate) operation_id: String,
    pub(crate) provider_key: String,
    pub(crate) provider_name: String,
    pub(crate) kind: OperationKind,
    pub(crate) status: OperationStatus,
    pub(crate) stage: String,
    pub(crate) detail: Option<String>,
    pub(crate) saved_tracks_done: usize,
    pub(crate) saved_tracks_total: Option<usize>,
    pub(crate) playlists_done: usize,
    pub(crate) playlists_total: Option<usize>,
    pub(crate) playlist_entries_done: usize,
    pub(crate) playlist_entries_total: Option<usize>,
    pub(crate) message: Option<String>,
    pub(crate) warnings: Vec<String>,
    pub(crate) error: Option<String>,
    pub(crate) started_at: String,
    pub(crate) finished_at: Option<String>,
}

#[derive(Default, Deserialize)]
pub(crate) struct SavedTracksQuery {
    pub(crate) q: Option<String>,
    pub(crate) page: Option<usize>,
}

#[derive(Default, Deserialize)]
pub(crate) struct TracksQuery {
    pub(crate) q: Option<String>,
    pub(crate) coverage: Option<String>,
    pub(crate) page: Option<usize>,
}

#[derive(Default, Deserialize)]
pub(crate) struct IdentityConflictsQuery {
    pub(crate) q: Option<String>,
    pub(crate) provider: Option<ProviderKind>,
    pub(crate) recommendation: Option<String>,
    pub(crate) impact: Option<String>,
    pub(crate) page: Option<usize>,
}

#[derive(Default, Deserialize)]
pub(crate) struct IdentityGapsQuery {
    pub(crate) provider: Option<ProviderKind>,
    pub(crate) q: Option<String>,
    pub(crate) page: Option<usize>,
}

#[derive(Default, Deserialize)]
pub(crate) struct PlaylistsQuery {
    pub(crate) q: Option<String>,
    pub(crate) page: Option<usize>,
}

#[derive(Deserialize)]
pub(crate) struct UpdateTrackRequest {
    pub(crate) title: String,
    pub(crate) artists: Vec<String>,
    pub(crate) album: Option<String>,
    pub(crate) duration_seconds: Option<u32>,
    pub(crate) isrc: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct UpdatePlaylistRequest {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct MessageResponse {
    pub(crate) message: String,
    pub(crate) warnings: Vec<String>,
}

impl MessageResponse {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            warnings: Vec::new(),
        }
    }

    pub(crate) fn with_warnings(message: impl Into<String>, warnings: Vec<String>) -> Self {
        Self {
            message: message.into(),
            warnings,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct PageResponse<T> {
    pub(crate) items: Vec<T>,
    pub(crate) total: usize,
    pub(crate) page: usize,
    pub(crate) page_size: usize,
    pub(crate) total_pages: usize,
}

impl<T> PageResponse<T> {
    pub(crate) fn new(items: Vec<T>, total: usize, page: usize, page_size: usize) -> Self {
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
pub(crate) struct OverviewResponse {
    pub(crate) library_updated_at: String,
    pub(crate) tracks: usize,
    pub(crate) saved_tracks: usize,
    pub(crate) playlists: usize,
    pub(crate) playlist_entries: usize,
    pub(crate) canonical_only: usize,
    pub(crate) multi_provider: usize,
    pub(crate) unmatched_tracks: usize,
    pub(crate) identity_conflicts: usize,
    pub(crate) provider_only_counts: Vec<ProviderOnlyCountDto>,
    pub(crate) provider_metrics: Vec<ProviderStatsDto>,
}

#[derive(Serialize)]
pub(crate) struct ProviderOnlyCountDto {
    pub(crate) key: String,
    pub(crate) name: String,
    pub(crate) count: usize,
}

#[derive(Serialize)]
pub(crate) struct ProviderStatsDto {
    pub(crate) key: String,
    pub(crate) name: String,
    pub(crate) linked_tracks: usize,
    pub(crate) missing_track_ids: usize,
    pub(crate) unmatched_tracks: usize,
    pub(crate) synced_saved_tracks: usize,
    pub(crate) pushable_saved_tracks: usize,
    pub(crate) saved_tracks_missing_identity: usize,
    pub(crate) unmatched_saved_tracks: usize,
    pub(crate) linked_playlists: usize,
    pub(crate) pushable_playlist_entries: usize,
    pub(crate) playlist_entries_missing_identity: usize,
    pub(crate) unmatched_playlist_entries: usize,
}

#[derive(Clone, Serialize)]
pub(crate) struct ProviderPreflightDto {
    pub(crate) can_pull: bool,
    pub(crate) can_push: bool,
    pub(crate) can_reset_push: bool,
    pub(crate) blockers: Vec<String>,
    pub(crate) reset_blockers: Vec<String>,
    pub(crate) warnings: Vec<String>,
    pub(crate) saved_tracks_total: usize,
    pub(crate) saved_tracks_pushable: usize,
    pub(crate) saved_tracks_missing_identity: usize,
    pub(crate) playlists_total: usize,
    pub(crate) linked_playlists: usize,
    pub(crate) playlist_entries_total: usize,
    pub(crate) playlist_entries_pushable: usize,
    pub(crate) playlist_entries_missing_identity: usize,
    pub(crate) track_ids_total: usize,
    pub(crate) track_ids_linked: usize,
    pub(crate) track_ids_missing: usize,
}

#[derive(Serialize)]
pub(crate) struct ProviderPushPlanDto {
    pub(crate) provider: String,
    pub(crate) provider_name: String,
    pub(crate) preflight: ProviderPreflightDto,
    pub(crate) saved_tracks: PushPlanSectionDto,
    pub(crate) playlist_entries: PushPlanSectionDto,
    pub(crate) playlists: PushPlaylistPlanSectionDto,
}

#[derive(Serialize)]
pub(crate) struct PushPlanSectionDto {
    pub(crate) total: usize,
    pub(crate) pushable: usize,
    pub(crate) skipped_missing_identity: usize,
    pub(crate) skipped_examples: Vec<ConflictTrackDto>,
}

#[derive(Serialize)]
pub(crate) struct PushPlaylistPlanSectionDto {
    pub(crate) total: usize,
    pub(crate) linked: usize,
    pub(crate) unlinked: usize,
    pub(crate) examples: Vec<PushPlaylistPlanItemDto>,
}

#[derive(Serialize)]
pub(crate) struct PushPlaylistPlanItemDto {
    pub(crate) playlist_id: String,
    pub(crate) name: String,
    pub(crate) entry_count: usize,
    pub(crate) linked: bool,
    pub(crate) missing_entries: usize,
}

#[derive(Clone, Serialize)]
pub(crate) struct ProviderBadgeDto {
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) source: String,
    pub(crate) provider_id: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct StatusPillDto {
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) title: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct CoverageDto {
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) short_label: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct SavedTrackItemDto {
    pub(crate) saved_track_id: String,
    pub(crate) track_id: String,
    pub(crate) title: String,
    pub(crate) artists: Vec<String>,
    pub(crate) artist_summary: String,
    pub(crate) album: Option<String>,
    pub(crate) subtitle: String,
    pub(crate) duration_seconds: Option<u32>,
    pub(crate) duration_label: String,
    pub(crate) isrc: Option<String>,
    pub(crate) added_at: Option<String>,
    pub(crate) added_label: String,
    pub(crate) coverage: CoverageDto,
    pub(crate) providers: Vec<ProviderBadgeDto>,
    pub(crate) status_pills: Vec<StatusPillDto>,
    pub(crate) artwork_url: Option<String>,
}

#[derive(Clone, Serialize)]
pub(crate) struct TrackListItemDto {
    pub(crate) track_id: String,
    pub(crate) title: String,
    pub(crate) artists: Vec<String>,
    pub(crate) artist_summary: String,
    pub(crate) album: Option<String>,
    pub(crate) subtitle: String,
    pub(crate) duration_seconds: Option<u32>,
    pub(crate) duration_label: String,
    pub(crate) isrc: Option<String>,
    pub(crate) coverage: CoverageDto,
    pub(crate) providers: Vec<ProviderBadgeDto>,
    pub(crate) status_pills: Vec<StatusPillDto>,
    pub(crate) saved_count: usize,
    pub(crate) playlist_refs: usize,
    pub(crate) artwork_url: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct TrackDetailDto {
    pub(crate) track_id: String,
    pub(crate) title: String,
    pub(crate) artists: Vec<String>,
    pub(crate) artist_summary: String,
    pub(crate) album: Option<String>,
    pub(crate) duration_seconds: Option<u32>,
    pub(crate) duration_label: String,
    pub(crate) isrc: Option<String>,
    pub(crate) coverage: CoverageDto,
    pub(crate) providers: Vec<ProviderBadgeDto>,
    pub(crate) provider_status: Vec<ProviderStatusDetailDto>,
    pub(crate) identity_conflicts: Vec<TrackIdentityConflictDto>,
    pub(crate) saved_count: usize,
    pub(crate) playlist_refs: usize,
    pub(crate) artwork_url: Option<String>,
}

#[derive(Clone, Serialize)]
pub(crate) struct TrackIdentityConflictDto {
    pub(crate) provider: String,
    pub(crate) provider_name: String,
    pub(crate) provider_id: String,
    pub(crate) owner_track: ConflictTrackDto,
    pub(crate) conflicting_provider_links: Vec<ProviderLinkConflictDto>,
    pub(crate) evidence: TrackIdentityConflictEvidenceDto,
    pub(crate) message: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct ConflictTrackDto {
    pub(crate) track_id: String,
    pub(crate) title: String,
    pub(crate) artist_summary: String,
    pub(crate) album: Option<String>,
    pub(crate) coverage: CoverageDto,
    pub(crate) providers: Vec<ProviderBadgeDto>,
    pub(crate) saved_count: usize,
    pub(crate) playlist_refs: usize,
    pub(crate) artwork_url: Option<String>,
}

#[derive(Clone, Serialize)]
pub(crate) struct ProviderLinkConflictDto {
    pub(crate) provider: String,
    pub(crate) provider_name: String,
    pub(crate) source_provider_id: String,
    pub(crate) target_provider_id: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct TrackIdentityConflictEvidenceDto {
    pub(crate) provider_confidence: Option<f64>,
    pub(crate) metadata_similarity: f64,
    pub(crate) duration_delta_seconds: Option<u32>,
    pub(crate) source_saved_tracks: usize,
    pub(crate) source_playlist_entries: usize,
    pub(crate) candidate_saved_tracks: usize,
    pub(crate) candidate_playlist_entries: usize,
    pub(crate) recommendation: TrackIdentityConflictRecommendationDto,
}

#[derive(Clone, Serialize)]
pub(crate) struct TrackIdentityConflictRecommendationDto {
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) detail: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct TrackIdentityConflictQueueItemDto {
    pub(crate) source_track: ConflictTrackDto,
    pub(crate) conflict: TrackIdentityConflictDto,
}

#[derive(Clone, Serialize)]
pub(crate) struct TrackIdentityGapQueueItemDto {
    pub(crate) provider: String,
    pub(crate) provider_name: String,
    pub(crate) track: ConflictTrackDto,
    pub(crate) push_blocking: bool,
}

#[derive(Serialize)]
pub(crate) struct ProviderStatusDetailDto {
    pub(crate) provider: String,
    pub(crate) state: String,
    pub(crate) message: Option<String>,
    pub(crate) provider_item_id: Option<String>,
    pub(crate) confidence: Option<f64>,
    pub(crate) last_attempt_at: Option<String>,
    pub(crate) last_success_at: Option<String>,
    pub(crate) last_seen_at: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct ApplyTrackIdentityRequest {
    pub(crate) provider: ProviderKind,
    pub(crate) provider_id: String,
}

#[derive(Serialize)]
pub(crate) struct ApplyTrackIdentityResponse {
    pub(crate) message: String,
    pub(crate) result: String,
    pub(crate) provider: String,
    pub(crate) provider_id: String,
    pub(crate) track_id: String,
}

#[derive(Deserialize)]
pub(crate) struct MergeTrackRequest {
    pub(crate) target_track_id: String,
    pub(crate) conflict_resolution: MergeConflictResolutionChoice,
}

#[derive(Default, Deserialize)]
pub(crate) struct BulkMergeIdentityConflictsPlanQuery {
    pub(crate) q: Option<String>,
    pub(crate) provider: Option<ProviderKind>,
    pub(crate) impact: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct BulkMergeIdentityConflictsRequest {
    pub(crate) q: Option<String>,
    pub(crate) provider: Option<ProviderKind>,
    pub(crate) impact: Option<String>,
    pub(crate) conflict_resolution: MergeConflictResolutionChoice,
    pub(crate) max_merges: Option<usize>,
}

#[derive(Deserialize)]
pub(crate) struct RejectTrackIdentityConflictRequest {
    pub(crate) provider: ProviderKind,
    pub(crate) provider_id: String,
    pub(crate) owner_track_id: String,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MergeConflictResolutionChoice {
    KeepSource,
    KeepTarget,
}

#[derive(Serialize)]
pub(crate) struct MergeTrackResponse {
    pub(crate) message: String,
    pub(crate) source_track_id: String,
    pub(crate) target_track_id: String,
    pub(crate) resolved_conflicts: Vec<ResolvedProviderConflictDto>,
}

#[derive(Serialize)]
pub(crate) struct ResolvedProviderConflictDto {
    pub(crate) provider: String,
    pub(crate) provider_name: String,
    pub(crate) kept_provider_id: String,
    pub(crate) dropped_provider_id: String,
    pub(crate) kept_from_source: bool,
}

#[derive(Serialize)]
pub(crate) struct BulkMergeIdentityConflictsPlanDto {
    pub(crate) eligible_count: usize,
    pub(crate) examples: Vec<TrackIdentityConflictQueueItemDto>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct BulkMergeIdentityConflictsResponse {
    pub(crate) message: String,
    pub(crate) eligible_count: usize,
    pub(crate) merged_count: usize,
    pub(crate) skipped_count: usize,
    pub(crate) resolved_provider_conflicts: usize,
    pub(crate) conflict_resolution: String,
    pub(crate) conflict_resolution_label: String,
    pub(crate) pre_merge_backup_path: String,
    pub(crate) merged_examples: Vec<BulkMergedIdentityConflictDto>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct BulkMergedIdentityConflictDto {
    pub(crate) source_track_id: String,
    pub(crate) target_track_id: String,
    pub(crate) title: String,
    pub(crate) provider: String,
    pub(crate) provider_id: String,
    pub(crate) resolved_conflicts: Vec<ResolvedProviderConflictDto>,
}

#[derive(Clone, Serialize)]
pub(crate) struct PlaylistSummaryDto {
    pub(crate) playlist_id: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) entry_count: usize,
    pub(crate) providers: Vec<ProviderBadgeDto>,
    pub(crate) status_pills: Vec<StatusPillDto>,
    pub(crate) artwork_url: Option<String>,
}

#[derive(Clone, Serialize)]
pub(crate) struct PlaylistEntryDto {
    pub(crate) entry_id: String,
    pub(crate) track_id: String,
    pub(crate) title: String,
    pub(crate) artists: Vec<String>,
    pub(crate) artist_summary: String,
    pub(crate) album: Option<String>,
    pub(crate) subtitle: String,
    pub(crate) added_at: Option<String>,
    pub(crate) added_label: String,
    pub(crate) coverage: CoverageDto,
    pub(crate) providers: Vec<ProviderBadgeDto>,
    pub(crate) status_pills: Vec<StatusPillDto>,
    pub(crate) artwork_url: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct PlaylistDetailDto {
    pub(crate) playlist: PlaylistSummaryDto,
    pub(crate) entries: Vec<PlaylistEntryDto>,
}

#[derive(Serialize)]
pub(crate) struct BackupsResponse {
    pub(crate) automatic_backup_dir: String,
    pub(crate) manual_backup_dir: String,
    pub(crate) backups: Vec<BackupDto>,
}

#[derive(Serialize)]
pub(crate) struct CreateBackupResponse {
    pub(crate) message: String,
    pub(crate) backup: BackupDto,
}

#[derive(Deserialize)]
pub(crate) struct RestoreBackupRequest {
    pub(crate) backup_type: String,
    pub(crate) file_name: String,
}

#[derive(Serialize)]
pub(crate) struct RestoreBackupResponse {
    pub(crate) message: String,
    pub(crate) restored_backup: BackupDto,
    pub(crate) pre_restore_backup: BackupDto,
}

#[derive(Clone, Serialize)]
pub(crate) struct BackupDto {
    pub(crate) file_name: String,
    pub(crate) path: String,
    pub(crate) backup_type: String,
    pub(crate) size_bytes: u64,
    pub(crate) modified_at: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct ProvidersResponse {
    pub(crate) spotify_redirect_uri: String,
    pub(crate) providers: Vec<ProviderConnectionDto>,
}

#[derive(Serialize)]
pub(crate) struct ProviderConnectionDto {
    pub(crate) key: String,
    pub(crate) name: String,
    pub(crate) connected: bool,
    pub(crate) connected_at: Option<String>,
    pub(crate) updated_at: Option<String>,
    pub(crate) health_checked_at: Option<String>,
    pub(crate) health_ok: Option<bool>,
    pub(crate) health_message: Option<String>,
    pub(crate) cooldown_until: Option<String>,
    pub(crate) cooldown_reason: Option<String>,
    pub(crate) preflight: ProviderPreflightDto,
}

#[derive(Deserialize)]
pub(crate) struct SpotifyConnectStartRequest {
    pub(crate) client_id: String,
    pub(crate) client_secret: String,
}

#[derive(Serialize)]
pub(crate) struct SpotifyConnectStartResponse {
    pub(crate) authorization_url: String,
}

#[derive(Deserialize)]
pub(crate) struct YoutubeMusicConnectRequest {
    pub(crate) headers_json: String,
}

#[derive(Deserialize)]
pub(crate) struct SpotifyCallbackQuery {
    pub(crate) code: Option<String>,
    pub(crate) state: Option<String>,
    pub(crate) error: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct SpotifyOembedResponse {
    pub(crate) thumbnail_url: Option<String>,
    pub(crate) thumbnail_width: Option<u32>,
    pub(crate) thumbnail_height: Option<u32>,
}

pub(crate) fn total_pages(total_rows: usize, page_size: usize) -> usize {
    if total_rows == 0 {
        0
    } else {
        total_rows.div_ceil(page_size)
    }
}
