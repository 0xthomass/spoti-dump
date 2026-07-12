export type PageResponse<T> = {
  items: T[]
  total: number
  page: number
  page_size: number
  total_pages: number
}

export type ProviderMetric = {
  key: string
  name: string
  linked_tracks: number
  missing_track_ids: number
  unmatched_tracks: number
  synced_saved_tracks: number
  pushable_saved_tracks: number
  saved_tracks_missing_identity: number
  unmatched_saved_tracks: number
  linked_playlists: number
  pushable_playlist_entries: number
  playlist_entries_missing_identity: number
  unmatched_playlist_entries: number
}

export type Overview = {
  library_updated_at: string
  tracks: number
  saved_tracks: number
  playlists: number
  playlist_entries: number
  canonical_only: number
  multi_provider: number
  unmatched_tracks: number
  identity_conflicts: number
  provider_only_counts: ProviderOnlyCount[]
  provider_metrics: ProviderMetric[]
}

export type HealthResponse = {
  status: string
  database_path: string
  integrity_check: string
  tracks: number
  saved_tracks: number
  playlists: number
  playlist_entries: number
  durable_operation_history: boolean
}

export type ProviderOnlyCount = {
  key: string
  name: string
  count: number
}

export type ActionResponse = {
  message: string
  warnings: string[]
}

export type ApplyIdentityResponse = {
  message: string
  result: string
  provider: string
  provider_id: string
  track_id: string
}

export type OperationStartResponse = {
  operation_id: string
}

export type OperationStatus = 'running' | 'succeeded' | 'failed'
export type OperationKind =
  | 'verify'
  | 'pull'
  | 'push'
  | 'reset_push'
  | 'identity'
  | 'identity_all'

export type OperationSnapshot = {
  operation_id: string
  provider_key: string
  provider_name: string
  kind: OperationKind
  status: OperationStatus
  stage: string
  detail: string | null
  saved_tracks_done: number
  saved_tracks_total: number | null
  playlists_done: number
  playlists_total: number | null
  playlist_entries_done: number
  playlist_entries_total: number | null
  message: string | null
  warnings: string[]
  error: string | null
  started_at: string
  finished_at: string | null
}

export type ProviderConnectionState = {
  key: string
  name: string
  connected: boolean
  connected_at: string | null
  updated_at: string | null
  health_checked_at: string | null
  health_ok: boolean | null
  health_message: string | null
  cooldown_until: string | null
  cooldown_reason: string | null
  preflight: ProviderPreflight
}

export type ProvidersResponse = {
  spotify_redirect_uri: string
  providers: ProviderConnectionState[]
}

export type BackupItem = {
  file_name: string
  path: string
  backup_type: string
  size_bytes: number
  modified_at: string | null
}

export type BackupsResponse = {
  automatic_backup_dir: string
  manual_backup_dir: string
  backups: BackupItem[]
}

export type CreateBackupResponse = {
  message: string
  backup: BackupItem
}

export type RestoreBackupResponse = {
  message: string
  restored_backup: BackupItem
  pre_restore_backup: BackupItem
}

export type ProviderPreflight = {
  can_pull: boolean
  can_push: boolean
  can_reset_push: boolean
  blockers: string[]
  reset_blockers: string[]
  warnings: string[]
  saved_tracks_total: number
  saved_tracks_pushable: number
  saved_tracks_missing_identity: number
  playlists_total: number
  linked_playlists: number
  playlist_entries_total: number
  playlist_entries_pushable: number
  playlist_entries_missing_identity: number
  track_ids_total: number
  track_ids_linked: number
  track_ids_missing: number
}

export type ProviderPushPlan = {
  provider: string
  provider_name: string
  preflight: ProviderPreflight
  saved_tracks: PushPlanSection
  playlist_entries: PushPlanSection
  playlists: PushPlaylistPlanSection
}

export type PushPlanSection = {
  total: number
  pushable: number
  skipped_missing_identity: number
  skipped_examples: ConflictTrack[]
}

export type PushPlaylistPlanSection = {
  total: number
  linked: number
  unlinked: number
  examples: PushPlaylistPlanItem[]
}

export type PushPlaylistPlanItem = {
  playlist_id: string
  name: string
  entry_count: number
  linked: boolean
  missing_entries: number
}

export type ProviderBadge = {
  key: string
  label: string
  source: string
  provider_id: string
}

export type StatusPill = {
  key: string
  label: string
  title: string
}

export type Coverage = {
  key: string
  label: string
  short_label: string
}

export type SavedTrackItem = {
  saved_track_id: string
  track_id: string
  title: string
  artists: string[]
  artist_summary: string
  album: string | null
  subtitle: string
  duration_seconds: number | null
  duration_label: string
  isrc: string | null
  added_at: string | null
  added_label: string
  coverage: Coverage
  providers: ProviderBadge[]
  status_pills: StatusPill[]
  artwork_url: string | null
}

export type TrackItem = {
  track_id: string
  title: string
  artists: string[]
  artist_summary: string
  album: string | null
  subtitle: string
  duration_seconds: number | null
  duration_label: string
  isrc: string | null
  coverage: Coverage
  providers: ProviderBadge[]
  status_pills: StatusPill[]
  saved_count: number
  playlist_refs: number
  artwork_url: string | null
}

export type ProviderStatusDetail = {
  provider: string
  state: string
  message: string | null
  provider_item_id: string | null
  confidence: number | null
  last_attempt_at: string | null
  last_success_at: string | null
  last_seen_at: string | null
}

export type TrackDetail = {
  track_id: string
  title: string
  artists: string[]
  artist_summary: string
  album: string | null
  duration_seconds: number | null
  duration_label: string
  isrc: string | null
  coverage: Coverage
  providers: ProviderBadge[]
  provider_status: ProviderStatusDetail[]
  identity_conflicts: TrackIdentityConflict[]
  saved_count: number
  playlist_refs: number
  artwork_url: string | null
}

export type TrackIdentityConflict = {
  provider: string
  provider_name: string
  provider_id: string
  owner_track: ConflictTrack
  conflicting_provider_links: ProviderLinkConflict[]
  evidence: TrackIdentityConflictEvidence
  message: string
}

export type TrackIdentityConflictEvidence = {
  provider_confidence: number | null
  metadata_similarity: number
  duration_delta_seconds: number | null
  source_saved_tracks: number
  source_playlist_entries: number
  candidate_saved_tracks: number
  candidate_playlist_entries: number
  recommendation: TrackIdentityConflictRecommendation
}

export type TrackIdentityConflictRecommendation = {
  key: string
  label: string
  detail: string
}

export type ConflictTrack = {
  track_id: string
  title: string
  artist_summary: string
  album: string | null
  coverage: Coverage
  providers: ProviderBadge[]
  saved_count: number
  playlist_refs: number
  artwork_url: string | null
}

export type ProviderLinkConflict = {
  provider: string
  provider_name: string
  source_provider_id: string
  target_provider_id: string
}

export type MergeTrackResponse = {
  message: string
  source_track_id: string
  target_track_id: string
  resolved_conflicts: {
    provider: string
    provider_name: string
    kept_provider_id: string
    dropped_provider_id: string
    kept_from_source: boolean
  }[]
}

export type BulkMergeIdentityConflictsPlan = {
  eligible_count: number
  examples: IdentityConflictQueueItem[]
  warnings: string[]
}

export type BulkMergeIdentityConflictsResponse = {
  message: string
  eligible_count: number
  merged_count: number
  skipped_count: number
  resolved_provider_conflicts: number
  conflict_resolution: string
  conflict_resolution_label: string
  pre_merge_backup_path: string
  merged_examples: {
    source_track_id: string
    target_track_id: string
    title: string
    provider: string
    provider_id: string
    resolved_conflicts: {
      provider: string
      provider_name: string
      kept_provider_id: string
      dropped_provider_id: string
      kept_from_source: boolean
    }[]
  }[]
  warnings: string[]
}

export type IdentityConflictQueueItem = {
  source_track: ConflictTrack
  conflict: TrackIdentityConflict
}

export type IdentityGapQueueItem = {
  provider: string
  provider_name: string
  track: ConflictTrack
  push_blocking: boolean
}

export type PlaylistSummary = {
  playlist_id: string
  name: string
  description: string | null
  entry_count: number
  providers: ProviderBadge[]
  status_pills: StatusPill[]
  artwork_url: string | null
}

export type PlaylistEntry = {
  entry_id: string
  track_id: string
  title: string
  artists: string[]
  artist_summary: string
  album: string | null
  subtitle: string
  added_at: string | null
  added_label: string
  coverage: Coverage
  providers: ProviderBadge[]
  status_pills: StatusPill[]
  artwork_url: string | null
}

export type PlaylistDetail = {
  playlist: PlaylistSummary
  entries: PlaylistEntry[]
}
