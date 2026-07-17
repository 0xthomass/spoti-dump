use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

use crate::domain::{
    LibraryState, ProviderKind, TrackEntity, TrackIdentityConflict, TrackMergeConflictResolution,
};
use crate::matching::metadata_similarity;

use super::dto::*;
use super::projections::*;

pub(crate) fn track_merge_conflict_resolution(
    choice: MergeConflictResolutionChoice,
) -> TrackMergeConflictResolution {
    match choice {
        MergeConflictResolutionChoice::KeepSource => TrackMergeConflictResolution::KeepSource,
        MergeConflictResolutionChoice::KeepTarget => TrackMergeConflictResolution::KeepTarget,
    }
}

pub(crate) fn merge_conflict_resolution_key(choice: MergeConflictResolutionChoice) -> &'static str {
    match choice {
        MergeConflictResolutionChoice::KeepSource => "keep_source",
        MergeConflictResolutionChoice::KeepTarget => "keep_target",
    }
}

pub(crate) fn merge_conflict_resolution_label(
    choice: MergeConflictResolutionChoice,
) -> &'static str {
    match choice {
        MergeConflictResolutionChoice::KeepSource => "Keep source IDs",
        MergeConflictResolutionChoice::KeepTarget => "Keep candidate IDs",
    }
}

pub(crate) fn resolved_provider_conflict_dtos(
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

pub(crate) fn bulk_merge_identity_conflict_rows(
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

pub(crate) fn bulk_merge_identity_conflict_warnings() -> Vec<String> {
    vec![
        "Bulk merge is limited to conflicts still classified as likely same recording.".to_string(),
        "A manual source-of-truth backup is created before any rows are merged.".to_string(),
        "Provider accounts are not changed by this operation.".to_string(),
    ]
}

pub(crate) fn active_bulk_merge_identity_conflict(
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

pub(crate) fn identity_conflict_rows(
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
pub(crate) struct IdentityConflictFilters<'a> {
    pub(crate) query: Option<&'a str>,
    pub(crate) provider: Option<ProviderKind>,
    pub(crate) recommendation: Option<&'a str>,
    pub(crate) impact: Option<&'a str>,
}

pub(crate) fn identity_conflict_rows_filtered(
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

pub(crate) fn compare_identity_conflict_rows(
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

pub(crate) fn identity_conflict_recommendation_priority(key: &str) -> u8 {
    match key {
        "likely_same_recording" => 0,
        "needs_manual_review" => 1,
        "likely_different_recording" => 2,
        _ => 3,
    }
}

pub(crate) fn identity_conflict_library_impact(
    evidence: &TrackIdentityConflictEvidenceDto,
) -> usize {
    evidence.source_saved_tracks
        + evidence.source_playlist_entries
        + evidence.candidate_saved_tracks
        + evidence.candidate_playlist_entries
}

pub(crate) fn identity_conflict_recommendation_matches(
    conflict: &TrackIdentityConflictDto,
    filter: Option<&str>,
) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    normalized_filter_token(Some(&conflict.evidence.recommendation.key)).as_deref() == Some(filter)
}

pub(crate) fn identity_conflict_impact_matches(
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

pub(crate) fn normalized_filter_token(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_lowercase().replace('-', "_"))
}

pub(crate) fn identity_gap_rows(
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
pub(crate) struct ConflictIndexes<'a> {
    pub(crate) saved_counts: BTreeMap<String, usize>,
    pub(crate) playlist_ref_counts: BTreeMap<String, usize>,
    pub(crate) owner_by_provider_id: HashMap<(&'a str, &'a str), &'a TrackEntity>,
}

impl<'a> ConflictIndexes<'a> {
    pub(crate) fn build(state: &'a LibraryState) -> Self {
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

    pub(crate) fn saved_count(&self, track_id: &str) -> usize {
        self.saved_counts.get(track_id).copied().unwrap_or(0)
    }

    pub(crate) fn playlist_ref_count(&self, track_id: &str) -> usize {
        self.playlist_ref_counts.get(track_id).copied().unwrap_or(0)
    }
}

pub(crate) fn identity_conflicts_for_track(
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
pub(crate) fn build_identity_conflict_dto(
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

pub(crate) fn conflict_track_dto(
    track: &TrackEntity,
    indexes: &ConflictIndexes<'_>,
) -> ConflictTrackDto {
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

pub(crate) fn provider_link_conflicts(
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

pub(crate) fn identity_conflict_evidence(
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

pub(crate) fn identity_conflict_recommendation(
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

pub(crate) fn track_has_identity_conflict(track: &TrackEntity) -> bool {
    track.open_identity_conflicts().next().is_some()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::Utc;

    use crate::domain::{
        LibraryState, LinkSource, PlaylistEntity, PlaylistEntry, ProviderKind, ProviderTrackLink,
        SavedTrackEntry,
    };
    use crate::web::projections::coverage_matches;
    use crate::web::test_support::{
        identity_conflict_pair, test_track_with_link, IdentityConflictPairFixture,
    };

    use super::{
        bulk_merge_identity_conflict_rows, identity_conflict_rows, identity_conflict_rows_filtered,
        identity_gap_rows, IdentityConflictFilters,
    };

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
}
