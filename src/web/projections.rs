use std::cmp::Ordering;
use std::collections::BTreeMap;

use chrono::DateTime;

use crate::domain::{
    LibraryState, ProviderKind, ProviderTrackArtwork, SyncState, SyncStatusRecord, TrackEntity,
    TrackMetadata,
};

use super::conflicts::*;
use super::dto::*;
use super::error::ApiError;

pub(crate) fn overview_payload(state: &LibraryState) -> OverviewResponse {
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

pub(crate) fn saved_track_rows(
    state: &LibraryState,
    query: Option<&str>,
) -> Vec<SavedTrackItemDto> {
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
pub(crate) fn compare_added_at_desc(left: Option<&str>, right: Option<&str>) -> Ordering {
    let left = left.and_then(crate::domain::mutate::parse_added_at);
    let right = right.and_then(crate::domain::mutate::parse_added_at);
    match (left, right) {
        (Some(left), Some(right)) => right.cmp(&left),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

pub(crate) fn track_rows(
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

pub(crate) fn build_track_detail(
    state: &LibraryState,
    track_id: &str,
) -> Result<TrackDetailDto, ApiError> {
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

pub(crate) fn playlist_summaries(
    state: &LibraryState,
    query: Option<&str>,
) -> Vec<PlaylistSummaryDto> {
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

pub(crate) fn build_playlist_detail(
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

pub(crate) fn build_track_index(state: &LibraryState) -> BTreeMap<&str, &TrackEntity> {
    state
        .tracks
        .iter()
        .map(|track| (track.id.as_str(), track))
        .collect()
}

pub(crate) fn indexed_track_has_provider_link(
    track_index: &BTreeMap<&str, &TrackEntity>,
    track_id: &str,
    provider: ProviderKind,
) -> bool {
    track_index
        .get(track_id)
        .map(|track| track.provider_links.contains_key(provider.as_key()))
        .unwrap_or(false)
}

pub(crate) fn build_saved_track_counts(state: &LibraryState) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for entry in &state.saved_tracks {
        *counts.entry(entry.track_id.clone()).or_insert(0) += 1;
    }
    counts
}

pub(crate) fn build_playlist_reference_counts(state: &LibraryState) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for playlist in &state.playlists {
        for entry in &playlist.entries {
            *counts.entry(entry.track_id.clone()).or_insert(0) += 1;
        }
    }
    counts
}

pub(crate) fn playlist_artwork_track_ids(
    state: &LibraryState,
    playlist_ids: &[String],
) -> Vec<String> {
    state
        .playlists
        .iter()
        .filter(|playlist| playlist_ids.iter().any(|id| id == &playlist.id))
        .filter_map(|playlist| playlist.entries.first().map(|entry| entry.track_id.clone()))
        .collect()
}

pub(crate) fn provider_badges<T>(links: &BTreeMap<String, T>) -> Vec<ProviderBadgeDto>
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

pub(crate) trait ProviderLinkLike {
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

pub(crate) fn provider_status_details(
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

pub(crate) fn summarized_status_pills(
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

pub(crate) fn compact_status_label(state: SyncState) -> &'static str {
    match state {
        SyncState::Unmatched => "Unmatched",
        SyncState::Error => "Error",
        SyncState::Missing => "Missing",
        SyncState::Skipped => "Skipped",
        SyncState::Synced => "Synced",
        SyncState::Pending => "Pending",
    }
}

pub(crate) fn coverage_dto(track: &TrackEntity) -> CoverageDto {
    let key = track_coverage_key(track);
    CoverageDto {
        short_label: compact_coverage_label(&key).to_string(),
        label: coverage_label(&key),
        key,
    }
}

pub(crate) fn compact_coverage_label(key: &str) -> String {
    match key {
        "multi-provider" => "Multi".to_string(),
        "canonical-only" => "Local".to_string(),
        value if value.ends_with("-only") => provider_display_name(value.trim_end_matches("-only")),
        _ => "Local".to_string(),
    }
}

pub(crate) fn track_subtitle(metadata: &TrackMetadata) -> String {
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

pub(crate) fn provider_display_name(key: &str) -> String {
    ProviderKind::from_key(key)
        .map(|provider| provider.display_name().to_string())
        .unwrap_or_else(|_| key.to_string())
}

pub(crate) fn track_coverage_key(track: &TrackEntity) -> String {
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

pub(crate) fn coverage_label(key: &str) -> String {
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

pub(crate) fn coverage_matches(key: &str, track: &TrackEntity, filter: Option<&str>) -> bool {
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

pub(crate) fn preferred_artwork_url(track: &TrackEntity) -> Option<String> {
    preferred_artwork(track).map(|artwork| artwork.url.clone())
}

pub(crate) fn preferred_artwork(track: &TrackEntity) -> Option<&ProviderTrackArtwork> {
    track.provider_artwork.values().max_by_key(|artwork| {
        u64::from(artwork.width.unwrap_or(0)) * u64::from(artwork.height.unwrap_or(0))
    })
}

pub(crate) fn normalized_page(page: Option<usize>) -> usize {
    page.unwrap_or(1).max(1)
}

pub(crate) fn normalized_query(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase)
}

pub(crate) fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn query_matches(query: Option<&str>, parts: &[&str]) -> bool {
    match query {
        None => true,
        Some(query) => parts.iter().any(|part| part.to_lowercase().contains(query)),
    }
}

pub(crate) fn paginate_vec<T: Clone>(rows: &[T], page: usize, page_size: usize) -> Vec<T> {
    let start = (page - 1) * page_size;
    rows.iter().skip(start).take(page_size).cloned().collect()
}

pub(crate) fn format_duration(duration_seconds: Option<u32>) -> String {
    match duration_seconds {
        None => "--:--".to_string(),
        Some(value) => format!("{}:{:02}", value / 60, value % 60),
    }
}

pub(crate) fn format_date(value: Option<&str>) -> Option<String> {
    let value = value?;
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed.format("%b %-d, %Y").to_string());
    }
    Some(value.to_string())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use crate::domain::{LibraryState, LinkSource, ProviderKind, ProviderTrackLink};
    use crate::web::test_support::{saved_entry, test_track, test_track_with_link};

    use super::{compare_added_at_desc, coverage_matches, saved_track_rows};

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
}
