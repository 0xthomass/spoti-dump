use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::Utc;

use crate::matching::cleaned_title;
use crate::model::{LibraryState, LinkSource, ProviderKind, SyncStatusRecord, TrackMetadata};
use crate::provider::{ProgressHandler, ProviderProgress, StreamingProvider};
use crate::state::TrackIdentityApplyResult;

const SPOTIFY_IDENTITY_SEARCHES_PER_RUN: usize = 75;
const YOUTUBE_MUSIC_IDENTITY_SEARCHES_PER_RUN: usize = 150;

#[derive(Clone, Debug, Default)]
pub struct IdentityReconcileSummary {
    pub provider: ProviderKind,
    pub tracks_scanned: usize,
    pub tracks_missing_provider_id: usize,
    pub provider_searches: usize,
    pub provider_links_added: usize,
    pub tracks_merged: usize,
    pub already_linked: usize,
    pub unmatched: usize,
    pub merge_conflicts: usize,
    pub invalid_metadata: usize,
    pub duplicate_saved_tracks_removed: usize,
    pub rate_limited: bool,
    pub unprocessed_due_rate_limit: usize,
    pub unprocessed_due_safety_limit: usize,
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct IdentityReconcileOptions {
    pub max_provider_searches: Option<usize>,
}

pub async fn reconcile_provider_identities(
    provider: &dyn StreamingProvider,
    state: &mut LibraryState,
    progress: Option<ProgressHandler>,
) -> Result<IdentityReconcileSummary> {
    reconcile_provider_identities_with_options(
        provider,
        state,
        progress,
        IdentityReconcileOptions::default(),
    )
    .await
}

pub async fn reconcile_provider_identities_with_options(
    provider: &dyn StreamingProvider,
    state: &mut LibraryState,
    progress: Option<ProgressHandler>,
    options: IdentityReconcileOptions,
) -> Result<IdentityReconcileSummary> {
    let provider_kind = provider.kind();
    let now = Utc::now();
    let mut summary = IdentityReconcileSummary {
        provider: provider_kind,
        tracks_scanned: state.tracks.len(),
        duplicate_saved_tracks_removed: state.consolidate_duplicate_saved_tracks(),
        ..Default::default()
    };

    let candidates = state
        .tracks
        .iter()
        .filter(|track| !track.provider_links.contains_key(provider_kind.as_key()))
        .map(|track| (track.id.clone(), track.metadata.clone()))
        .collect::<Vec<_>>();
    summary.tracks_missing_provider_id = candidates.len();
    let provider_search_budget = options
        .max_provider_searches
        .unwrap_or_else(|| default_provider_search_budget(provider_kind));

    let mut cache = HashMap::<String, Option<(String, f64)>>::new();
    for (index, (track_id, metadata)) in candidates.into_iter().enumerate() {
        emit_progress(
            progress.as_ref(),
            ProviderProgress {
                stage: "Resolving identities".to_string(),
                detail: Some(metadata.display_label()),
                saved_tracks_done: index,
                saved_tracks_total: Some(summary.tracks_missing_provider_id),
                ..Default::default()
            },
        );

        if !state.tracks.iter().any(|track| track.id == track_id) {
            continue;
        }
        if state
            .tracks
            .iter()
            .find(|track| track.id == track_id)
            .and_then(|track| track.provider_links.get(provider_kind.as_key()))
            .is_some()
        {
            continue;
        }
        if !is_searchable_metadata(&metadata) {
            summary.invalid_metadata += 1;
            let message = format!(
                "Cannot resolve {} identity because canonical metadata is missing a searchable title.",
                provider_kind.display_name()
            );
            summary
                .warnings
                .push(format!("{message} Track ID: {track_id}."));
            state.set_track_status(
                &track_id,
                provider_kind,
                SyncStatusRecord::error(message, now),
            );
            continue;
        }

        let cache_key = metadata_identity_key(&metadata);
        let resolved = if let Some(cached) = cache.get(&cache_key) {
            cached.clone()
        } else {
            if summary.provider_searches >= provider_search_budget {
                summary.unprocessed_due_safety_limit =
                    summary.tracks_missing_provider_id.saturating_sub(index);
                summary.warnings.push(format!(
                    "{} identity sync paused after {} provider identity lookups to avoid triggering provider rate limits. Re-run identity sync to continue from the remaining {} tracks.",
                    provider_kind.display_name(),
                    summary.provider_searches,
                    summary.unprocessed_due_safety_limit
                ));
                break;
            }

            summary.provider_searches += 1;
            match provider.resolve_track_identity(&metadata).await {
                Ok(resolved) => {
                    cache.insert(cache_key, resolved.clone());
                    resolved
                }
                Err(error) if is_rate_limit_error(&error) => {
                    summary.rate_limited = true;
                    summary.unprocessed_due_rate_limit =
                        summary.tracks_missing_provider_id.saturating_sub(index);
                    summary.warnings.push(format!(
                        "{} identity search was rate-limited after {} of {} missing tracks. Re-run identity sync later.",
                        provider_kind.display_name(),
                        index,
                        summary.tracks_missing_provider_id
                    ));
                    break;
                }
                Err(error) if is_invalid_argument_error(&error) => {
                    summary.invalid_metadata += 1;
                    let message = format!(
                        "Cannot resolve {} identity for {} because the provider rejected the generated search query: {error}",
                        provider_kind.display_name(),
                        metadata.display_label()
                    );
                    summary.warnings.push(message.clone());
                    state.set_track_status(
                        &track_id,
                        provider_kind,
                        SyncStatusRecord::error(message, now),
                    );
                    continue;
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "Failed to resolve {} identity for {}",
                            provider_kind.display_name(),
                            metadata.display_label()
                        )
                    });
                }
            }
        };

        match resolved {
            Some((provider_id, confidence)) => {
                let result = match state.apply_track_identity(
                    &track_id,
                    provider_kind,
                    provider_id.clone(),
                    LinkSource::Match,
                    Some(confidence),
                    now,
                ) {
                    Ok(result) => result,
                    Err(error) if is_identity_merge_conflict(&error) => {
                        summary.merge_conflicts += 1;
                        let message = format!(
                            "Skipped {} identity '{}' for {} because it would merge tracks with conflicting provider IDs: {error}",
                            provider_kind.display_name(),
                            provider_id,
                            metadata.display_label()
                        );
                        summary.warnings.push(message.clone());
                        state.set_track_status(
                            &track_id,
                            provider_kind,
                            SyncStatusRecord::error_with_provider_item_id(
                                message,
                                provider_id.clone(),
                                Some(confidence),
                                now,
                            ),
                        );
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                match &result {
                    TrackIdentityApplyResult::Linked { .. } => summary.provider_links_added += 1,
                    TrackIdentityApplyResult::AlreadyLinked { .. } => summary.already_linked += 1,
                    TrackIdentityApplyResult::Merged { .. } => summary.tracks_merged += 1,
                }
                state.set_track_status(
                    result.track_id(),
                    provider_kind,
                    SyncStatusRecord::synced(
                        Some(provider_id),
                        Some(confidence),
                        Some(format!(
                            "Resolved during {} identity sync",
                            provider_kind.display_name()
                        )),
                        now,
                    ),
                );
            }
            None => {
                summary.unmatched += 1;
                state.set_track_status(
                    &track_id,
                    provider_kind,
                    SyncStatusRecord::unmatched(
                        format!(
                            "No {} identity match during library reconciliation for {}",
                            provider_kind.display_name(),
                            metadata.display_label()
                        ),
                        now,
                    ),
                );
            }
        }
    }

    summary.duplicate_saved_tracks_removed += state.consolidate_duplicate_saved_tracks();
    state.validate()?;
    emit_progress(
        progress.as_ref(),
        ProviderProgress {
            stage: "Identity sync complete".to_string(),
            saved_tracks_done: summary
                .tracks_missing_provider_id
                .saturating_sub(summary.unprocessed_due_rate_limit)
                .saturating_sub(summary.unprocessed_due_safety_limit),
            saved_tracks_total: Some(summary.tracks_missing_provider_id),
            ..Default::default()
        },
    );
    Ok(summary)
}

fn emit_progress(progress: Option<&ProgressHandler>, update: ProviderProgress) {
    if let Some(callback) = progress {
        callback(update);
    }
}

fn metadata_identity_key(metadata: &TrackMetadata) -> String {
    let artists = metadata
        .artists
        .iter()
        .map(|artist| normalize_key_part(artist))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{}|{}|{}|{}|{}",
        normalize_key_part(&cleaned_title(&metadata.title)),
        artists,
        metadata
            .album
            .as_deref()
            .map(normalize_key_part)
            .unwrap_or_default(),
        metadata
            .duration_seconds
            .map(|duration| duration.to_string())
            .unwrap_or_default(),
        metadata
            .isrc
            .as_deref()
            .map(normalize_key_part)
            .unwrap_or_default()
    )
}

fn is_searchable_metadata(metadata: &TrackMetadata) -> bool {
    !cleaned_title(&metadata.title).trim().is_empty()
}

fn normalize_key_part(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn default_provider_search_budget(provider: ProviderKind) -> usize {
    match provider {
        ProviderKind::Spotify => SPOTIFY_IDENTITY_SEARCHES_PER_RUN,
        ProviderKind::YoutubeMusic => YOUTUBE_MUSIC_IDENTITY_SEARCHES_PER_RUN,
    }
}

fn is_rate_limit_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let message = cause.to_string();
        message.contains("429 Too Many Requests") || message.contains("rate limit")
    })
}

fn is_identity_merge_conflict(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .to_string()
            .contains("Cannot merge tracks because provider")
    })
}

fn is_invalid_argument_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let message = cause.to_string();
        message.contains("INVALID_ARGUMENT")
            || message.contains("Request contains an invalid argument")
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use anyhow::Result;
    use async_trait::async_trait;
    use chrono::Utc;

    use super::{
        reconcile_provider_identities, reconcile_provider_identities_with_options,
        IdentityReconcileOptions,
    };
    use crate::model::{
        LibraryState, LinkSource, ProviderKind, ProviderLibrarySnapshot, PurgeReport, SyncState,
        SyncSummary, TrackEntity, TrackMetadata,
    };
    use crate::provider::{ProgressHandler, StreamingProvider};

    struct StaticIdentityProvider {
        provider: ProviderKind,
        provider_id: String,
    }

    struct EchoIdentityProvider {
        provider: ProviderKind,
    }

    #[async_trait]
    impl StreamingProvider for StaticIdentityProvider {
        fn kind(&self) -> ProviderKind {
            self.provider
        }

        async fn verify_connection(&self) -> Result<()> {
            Ok(())
        }

        async fn export_library_with_progress(
            &self,
            _progress: Option<ProgressHandler>,
        ) -> Result<ProviderLibrarySnapshot> {
            Ok(ProviderLibrarySnapshot {
                provider: self.provider,
                captured_at: Utc::now(),
                saved_tracks: Vec::new(),
                playlists: Vec::new(),
                warnings: Vec::new(),
            })
        }

        async fn sync_library_with_progress(
            &self,
            _state: &mut LibraryState,
            _force: bool,
            _progress: Option<ProgressHandler>,
        ) -> Result<SyncSummary> {
            Ok(SyncSummary::default())
        }

        async fn resolve_track_identity(
            &self,
            _metadata: &TrackMetadata,
        ) -> Result<Option<(String, f64)>> {
            Ok(Some((self.provider_id.clone(), 0.99)))
        }

        async fn purge_library(&self, _force: bool) -> Result<PurgeReport> {
            Ok(PurgeReport::default())
        }

        async fn remove_saved_track(&self, _provider_track_id: &str) -> Result<()> {
            Ok(())
        }

        async fn delete_playlist(&self, _provider_playlist_id: &str) -> Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl StreamingProvider for EchoIdentityProvider {
        fn kind(&self) -> ProviderKind {
            self.provider
        }

        async fn verify_connection(&self) -> Result<()> {
            Ok(())
        }

        async fn export_library_with_progress(
            &self,
            _progress: Option<ProgressHandler>,
        ) -> Result<ProviderLibrarySnapshot> {
            Ok(ProviderLibrarySnapshot {
                provider: self.provider,
                captured_at: Utc::now(),
                saved_tracks: Vec::new(),
                playlists: Vec::new(),
                warnings: Vec::new(),
            })
        }

        async fn sync_library_with_progress(
            &self,
            _state: &mut LibraryState,
            _force: bool,
            _progress: Option<ProgressHandler>,
        ) -> Result<SyncSummary> {
            Ok(SyncSummary::default())
        }

        async fn resolve_track_identity(
            &self,
            metadata: &TrackMetadata,
        ) -> Result<Option<(String, f64)>> {
            let title_key = metadata
                .title
                .chars()
                .map(|character| {
                    if character.is_ascii_alphanumeric() {
                        character.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .collect::<String>();
            Ok(Some((
                format!("{}-{title_key}", self.provider.as_key()),
                0.99,
            )))
        }

        async fn purge_library(&self, _force: bool) -> Result<PurgeReport> {
            Ok(PurgeReport::default())
        }

        async fn remove_saved_track(&self, _provider_track_id: &str) -> Result<()> {
            Ok(())
        }

        async fn delete_playlist(&self, _provider_playlist_id: &str) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn identity_reconcile_continues_when_merge_would_conflict() {
        let mut state = LibraryState::new();
        state.tracks.push(track_entity("owner", "Conflict Track"));
        state
            .tracks
            .push(track_entity("candidate", "Conflict Track"));
        let now = Utc::now();
        state.upsert_track_link(
            "owner",
            ProviderKind::Spotify,
            "spotify-owner",
            LinkSource::Export,
            Some(1.0),
            now,
        );
        state.upsert_track_link(
            "owner",
            ProviderKind::YoutubeMusic,
            "youtube-shared",
            LinkSource::Export,
            Some(1.0),
            now,
        );
        state.upsert_track_link(
            "candidate",
            ProviderKind::Spotify,
            "spotify-candidate",
            LinkSource::Export,
            Some(1.0),
            now,
        );

        let provider = StaticIdentityProvider {
            provider: ProviderKind::YoutubeMusic,
            provider_id: "youtube-shared".to_string(),
        };
        let summary = reconcile_provider_identities(&provider, &mut state, None)
            .await
            .unwrap();

        assert_eq!(summary.merge_conflicts, 1);
        assert_eq!(state.tracks.len(), 2);
        let candidate = state
            .tracks
            .iter()
            .find(|track| track.id == "candidate")
            .unwrap();
        assert!(!candidate
            .provider_links
            .contains_key(ProviderKind::YoutubeMusic.as_key()));
        assert_eq!(
            candidate
                .provider_state
                .get(ProviderKind::YoutubeMusic.as_key())
                .map(|status| status.state),
            Some(SyncState::Error)
        );
        state.validate().unwrap();
    }

    #[tokio::test]
    async fn identity_reconcile_flags_blank_metadata_without_provider_request_failure() {
        let mut state = LibraryState::new();
        state.tracks.push(TrackEntity {
            id: "blank".to_string(),
            metadata: TrackMetadata {
                title: "".to_string(),
                artists: vec!["".to_string()],
                album: None,
                duration_seconds: Some(0),
                isrc: None,
            },
            provider_links: BTreeMap::new(),
            provider_artwork: BTreeMap::new(),
            provider_state: BTreeMap::new(),
        });
        let provider = StaticIdentityProvider {
            provider: ProviderKind::YoutubeMusic,
            provider_id: "youtube-id".to_string(),
        };

        let summary = reconcile_provider_identities(&provider, &mut state, None)
            .await
            .unwrap();

        assert_eq!(summary.invalid_metadata, 1);
        assert!(!state.tracks[0]
            .provider_links
            .contains_key(ProviderKind::YoutubeMusic.as_key()));
        assert_eq!(
            state.tracks[0]
                .provider_state
                .get(ProviderKind::YoutubeMusic.as_key())
                .map(|status| status.state),
            Some(SyncState::Error)
        );
        state.validate().unwrap();
    }

    #[tokio::test]
    async fn identity_reconcile_pauses_after_search_budget_to_avoid_rate_limits() {
        let mut state = LibraryState::new();
        state.tracks.push(track_entity("track-1", "Track One"));
        state.tracks.push(track_entity("track-2", "Track Two"));
        state.tracks.push(track_entity("track-3", "Track Three"));
        let provider = EchoIdentityProvider {
            provider: ProviderKind::Spotify,
        };

        let summary = reconcile_provider_identities_with_options(
            &provider,
            &mut state,
            None,
            IdentityReconcileOptions {
                max_provider_searches: Some(2),
            },
        )
        .await
        .unwrap();

        assert_eq!(summary.provider_searches, 2);
        assert_eq!(summary.provider_links_added, 2);
        assert_eq!(summary.unprocessed_due_rate_limit, 0);
        assert_eq!(summary.unprocessed_due_safety_limit, 1);
        assert!(summary
            .warnings
            .iter()
            .any(|warning| warning.contains("paused after 2 provider identity lookups")));
        assert_eq!(
            state
                .tracks
                .iter()
                .filter(|track| track
                    .provider_links
                    .contains_key(ProviderKind::Spotify.as_key()))
                .count(),
            2
        );
        state.validate().unwrap();
    }

    fn track_entity(id: &str, title: &str) -> TrackEntity {
        TrackEntity {
            id: id.to_string(),
            metadata: TrackMetadata {
                title: title.to_string(),
                artists: vec!["Artist".to_string()],
                album: Some("Album".to_string()),
                duration_seconds: Some(180),
                isrc: None,
            },
            provider_links: BTreeMap::new(),
            provider_artwork: BTreeMap::new(),
            provider_state: BTreeMap::new(),
        }
    }
}
