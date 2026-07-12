use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};

use crate::matching::metadata_similarity;

use super::library::{LibraryState, SavedTrackEntry};
use super::mutate::{merge_metadata, new_canonical_id, preferred_dimension, upsert_provider_link};
use super::playlist::{PlaylistEntity, PlaylistEntry};
use super::provider::ProviderKind;
use super::snapshot::{ObservedPlaylist, ObservedTrack, ProviderLibrarySnapshot};
use super::sync::{MergeSummary, SyncStatusRecord};
use super::track::{LinkSource, ProviderTrackArtwork, TrackEntity};

const TRACK_MATCH_THRESHOLD: f64 = 0.94;

/// Merges a provider snapshot into the canonical state.
///
/// This is a union/append-only operation: it creates and updates tracks,
/// saved tracks, playlists, and playlist entries, but it NEVER deletes
/// anything. Items that disappeared from the provider since the last export
/// stay in the canonical state untouched; reconciling removals is a separate,
/// explicit operation.
///
/// When an observation matches an existing track (by ISRC or metadata
/// similarity) that already holds a different provider ID for the same
/// provider, the established link is preserved: an open
/// `TrackIdentityConflict` is recorded on the track instead, no synced status
/// claiming the conflicting ID is written, and a warning is added to the
/// summary.
pub fn merge_provider_snapshot(
    state: &mut LibraryState,
    snapshot: ProviderLibrarySnapshot,
) -> MergeSummary {
    let mut summary = MergeSummary {
        warnings: snapshot.warnings,
        ..Default::default()
    };
    let provider = snapshot.provider;
    let observed_at = snapshot.captured_at;
    let mut indexes = MergeIndexes::build(state, provider);

    for saved_track in snapshot.saved_tracks {
        let outcome = upsert_track_from_observation(
            state,
            &mut indexes,
            provider,
            &saved_track.track,
            LinkSource::Export,
            Some(1.0),
            observed_at,
        );
        if outcome.created {
            summary.tracks_created += 1;
        }

        let saved_index =
            ensure_saved_track(state, &mut indexes, &outcome.track_id, saved_track.added_at);
        if outcome.conflicting_provider_id.is_some() {
            push_conflict_warning(&mut summary, state, provider, &outcome);
        } else {
            let status = SyncStatusRecord::synced(
                saved_track.track.provider_id.clone(),
                Some(1.0),
                Some("Observed in provider export".to_string()),
                observed_at,
            );
            state.tracks[outcome.index]
                .provider_state
                .insert(provider.as_key().to_string(), status.clone());
            state.saved_tracks[saved_index]
                .provider_state
                .insert(provider.as_key().to_string(), status);
        }
        summary.saved_tracks_seen += 1;
    }

    for playlist in snapshot.playlists {
        let playlist_id = upsert_playlist_from_observation(state, provider, &playlist, observed_at);
        summary.playlists_seen += 1;

        let observed_entries = collect_playlist_observations(
            state,
            &mut indexes,
            provider,
            &playlist,
            observed_at,
            &mut summary,
        );
        summary.playlist_entries_seen += observed_entries.len();
        merge_playlist_entries(state, &playlist_id, provider, observed_entries, observed_at);
    }

    state.touch();
    summary
}

/// Lookup tables over the canonical state for one merge run. Tracks and saved
/// tracks are only appended during a merge, so the stored indices stay valid;
/// the maps are maintained as new rows and links are created.
struct MergeIndexes {
    track_by_provider_id: HashMap<String, usize>,
    track_by_isrc: HashMap<String, usize>,
    saved_by_track_id: HashMap<String, usize>,
}

impl MergeIndexes {
    fn build(state: &LibraryState, provider: ProviderKind) -> Self {
        let key = provider.as_key();
        let mut track_by_provider_id = HashMap::new();
        let mut track_by_isrc = HashMap::new();
        for (index, track) in state.tracks.iter().enumerate() {
            if let Some(link) = track.provider_links.get(key) {
                track_by_provider_id
                    .entry(link.provider_id.clone())
                    .or_insert(index);
            }
            if let Some(isrc) = &track.metadata.isrc {
                track_by_isrc.entry(isrc.clone()).or_insert(index);
            }
        }

        let mut saved_by_track_id = HashMap::with_capacity(state.saved_tracks.len());
        for (index, saved_track) in state.saved_tracks.iter().enumerate() {
            saved_by_track_id
                .entry(saved_track.track_id.clone())
                .or_insert(index);
        }

        Self {
            track_by_provider_id,
            track_by_isrc,
            saved_by_track_id,
        }
    }
}

struct ObservedTrackOutcome {
    track_id: String,
    index: usize,
    created: bool,
    /// The observed provider ID that could not be applied because the matched
    /// track already holds a different link for this provider.
    conflicting_provider_id: Option<String>,
    /// True when the conflict above was newly recorded on the track (rather
    /// than already present from an earlier observation or review).
    conflict_recorded: bool,
}

fn push_conflict_warning(
    summary: &mut MergeSummary,
    state: &LibraryState,
    provider: ProviderKind,
    outcome: &ObservedTrackOutcome,
) {
    if !outcome.conflict_recorded {
        return;
    }
    let Some(candidate) = &outcome.conflicting_provider_id else {
        return;
    };
    let track = &state.tracks[outcome.index];
    summary.warnings.push(format!(
        "Observed {} ID '{}' for {} conflicts with the established link; recorded an identity conflict for review.",
        provider.display_name(),
        candidate,
        track.metadata.display_label()
    ));
}

fn collect_playlist_observations(
    state: &mut LibraryState,
    indexes: &mut MergeIndexes,
    provider: ProviderKind,
    playlist: &ObservedPlaylist,
    observed_at: DateTime<Utc>,
    summary: &mut MergeSummary,
) -> Vec<(String, Option<String>, Option<String>)> {
    let mut observations = Vec::with_capacity(playlist.tracks.len());
    for track in &playlist.tracks {
        let outcome = upsert_track_from_observation(
            state,
            indexes,
            provider,
            &track.track,
            LinkSource::Export,
            Some(1.0),
            observed_at,
        );
        let provider_item_id = if outcome.conflicting_provider_id.is_some() {
            push_conflict_warning(summary, state, provider, &outcome);
            None
        } else {
            state.tracks[outcome.index].provider_state.insert(
                provider.as_key().to_string(),
                SyncStatusRecord::synced(
                    track.track.provider_id.clone(),
                    Some(1.0),
                    Some("Observed in provider export".to_string()),
                    observed_at,
                ),
            );
            track.track.provider_id.clone()
        };
        observations.push((outcome.track_id, track.added_at.clone(), provider_item_id));
    }
    observations
}

fn merge_playlist_entries(
    state: &mut LibraryState,
    playlist_id: &str,
    provider: ProviderKind,
    observed_entries: Vec<(String, Option<String>, Option<String>)>,
    observed_at: DateTime<Utc>,
) {
    if let Some(playlist) = state
        .playlists
        .iter_mut()
        .find(|playlist| playlist.id == playlist_id)
    {
        let mut matched_entry_ids = Vec::new();

        for (track_id, added_at, provider_item_id) in observed_entries {
            if let Some(entry) = playlist
                .entries
                .iter_mut()
                .find(|entry| entry.track_id == track_id && !matched_entry_ids.contains(&entry.id))
            {
                entry.provider_state.insert(
                    provider.as_key().to_string(),
                    SyncStatusRecord::synced(
                        provider_item_id.clone(),
                        Some(1.0),
                        Some("Observed in provider export".to_string()),
                        observed_at,
                    ),
                );
                matched_entry_ids.push(entry.id.clone());
            } else {
                let entry_id = new_canonical_id("playlist-entry");
                let mut provider_state = BTreeMap::new();
                provider_state.insert(
                    provider.as_key().to_string(),
                    SyncStatusRecord::synced(
                        provider_item_id.clone(),
                        Some(1.0),
                        Some("Observed in provider export".to_string()),
                        observed_at,
                    ),
                );
                playlist.entries.push(PlaylistEntry {
                    id: entry_id.clone(),
                    track_id,
                    added_at,
                    provider_state,
                });
                matched_entry_ids.push(entry_id);
            }
        }
    }
}

fn upsert_track_from_observation(
    state: &mut LibraryState,
    indexes: &mut MergeIndexes,
    provider: ProviderKind,
    observed: &ObservedTrack,
    source: LinkSource,
    confidence: Option<f64>,
    seen_at: DateTime<Utc>,
) -> ObservedTrackOutcome {
    if let Some(index) = find_existing_track_index(state, indexes, observed) {
        let track = &mut state.tracks[index];
        let had_isrc = track.metadata.isrc.is_some();
        merge_metadata(&mut track.metadata, &observed.metadata);
        if !had_isrc {
            if let Some(isrc) = &track.metadata.isrc {
                indexes.track_by_isrc.entry(isrc.clone()).or_insert(index);
            }
        }

        let mut conflicting_provider_id = None;
        let mut conflict_recorded = false;
        if let Some(provider_id) = &observed.provider_id {
            let links_to_other_id = track
                .provider_links
                .get(provider.as_key())
                .map(|link| link.provider_id != *provider_id)
                .unwrap_or(false);
            if links_to_other_id {
                conflict_recorded = track.record_identity_conflict(
                    provider,
                    provider_id.clone(),
                    confidence,
                    seen_at,
                );
                conflicting_provider_id = Some(provider_id.clone());
            } else {
                upsert_provider_link(
                    &mut track.provider_links,
                    provider,
                    provider_id.clone(),
                    source,
                    confidence,
                    seen_at,
                );
                indexes
                    .track_by_provider_id
                    .entry(provider_id.clone())
                    .or_insert(index);
            }
        }
        if conflicting_provider_id.is_none() {
            merge_observed_artwork(track, provider, observed, seen_at);
        }

        return ObservedTrackOutcome {
            track_id: track.id.clone(),
            index,
            created: false,
            conflicting_provider_id,
            conflict_recorded,
        };
    }

    let id = new_canonical_id("track");
    let index = state.tracks.len();
    let mut provider_links = BTreeMap::new();
    if let Some(provider_id) = &observed.provider_id {
        upsert_provider_link(
            &mut provider_links,
            provider,
            provider_id.clone(),
            source,
            confidence,
            seen_at,
        );
        indexes
            .track_by_provider_id
            .entry(provider_id.clone())
            .or_insert(index);
    }
    let mut provider_artwork = BTreeMap::new();
    if let Some(artwork) = &observed.artwork {
        provider_artwork.insert(
            provider.as_key().to_string(),
            ProviderTrackArtwork {
                url: artwork.url.clone(),
                width: artwork.width,
                height: artwork.height,
                last_seen_at: Some(seen_at),
            },
        );
    }
    if let Some(isrc) = &observed.metadata.isrc {
        indexes.track_by_isrc.entry(isrc.clone()).or_insert(index);
    }

    state.tracks.push(TrackEntity {
        id: id.clone(),
        metadata: observed.metadata.clone(),
        provider_links,
        provider_artwork,
        provider_state: Default::default(),
        identity_conflicts: Vec::new(),
    });

    ObservedTrackOutcome {
        track_id: id,
        index,
        created: true,
        conflicting_provider_id: None,
        conflict_recorded: false,
    }
}

fn find_existing_track_index(
    state: &LibraryState,
    indexes: &MergeIndexes,
    observed: &ObservedTrack,
) -> Option<usize> {
    if let Some(provider_id) = &observed.provider_id {
        if let Some(&index) = indexes.track_by_provider_id.get(provider_id) {
            return Some(index);
        }
    }

    if let Some(isrc) = &observed.metadata.isrc {
        if let Some(&index) = indexes.track_by_isrc.get(isrc) {
            return Some(index);
        }
    }

    let mut best_index = None;
    let mut best_score = 0.0;
    for (index, track) in state.tracks.iter().enumerate() {
        let score = metadata_similarity(&observed.metadata, &track.metadata);
        if score > best_score {
            best_score = score;
            best_index = Some(index);
        }
    }

    if best_score >= TRACK_MATCH_THRESHOLD {
        best_index
    } else {
        None
    }
}

fn merge_observed_artwork(
    track: &mut TrackEntity,
    provider: ProviderKind,
    observed: &ObservedTrack,
    seen_at: DateTime<Utc>,
) {
    let Some(artwork) = &observed.artwork else {
        return;
    };

    let key = provider.as_key().to_string();
    if let Some(existing) = track.provider_artwork.get_mut(&key) {
        existing.url = artwork.url.clone();
        existing.width = preferred_dimension(existing.width, artwork.width);
        existing.height = preferred_dimension(existing.height, artwork.height);
        existing.last_seen_at = Some(seen_at);
    } else {
        track.provider_artwork.insert(
            key,
            ProviderTrackArtwork {
                url: artwork.url.clone(),
                width: artwork.width,
                height: artwork.height,
                last_seen_at: Some(seen_at),
            },
        );
    }
}

fn ensure_saved_track(
    state: &mut LibraryState,
    indexes: &mut MergeIndexes,
    track_id: &str,
    added_at: Option<String>,
) -> usize {
    if let Some(&index) = indexes.saved_by_track_id.get(track_id) {
        let saved_track = &mut state.saved_tracks[index];
        if saved_track.added_at.is_none() {
            saved_track.added_at = added_at;
        }
        return index;
    }

    let index = state.saved_tracks.len();
    state.saved_tracks.push(SavedTrackEntry {
        id: new_canonical_id("saved-track"),
        track_id: track_id.to_string(),
        added_at,
        provider_state: Default::default(),
    });
    indexes
        .saved_by_track_id
        .insert(track_id.to_string(), index);
    index
}

fn upsert_playlist_from_observation(
    state: &mut LibraryState,
    provider: ProviderKind,
    playlist: &ObservedPlaylist,
    observed_at: DateTime<Utc>,
) -> String {
    if let Some(index) = find_existing_playlist_index(state, provider, playlist) {
        let playlist_entity = &mut state.playlists[index];
        if playlist_entity.description.is_none() {
            playlist_entity.description = playlist.description.clone();
        }
        playlist_entity.name = playlist.name.clone();
        if let Some(provider_id) = &playlist.provider_id {
            upsert_provider_link(
                &mut playlist_entity.provider_links,
                provider,
                provider_id.clone(),
                LinkSource::Export,
                Some(1.0),
                observed_at,
            );
        }
        playlist_entity.provider_state.insert(
            provider.as_key().to_string(),
            SyncStatusRecord::synced(
                playlist.provider_id.clone(),
                Some(1.0),
                Some("Observed in provider export".to_string()),
                observed_at,
            ),
        );
        return playlist_entity.id.clone();
    }

    let id = new_canonical_id("playlist");
    let mut provider_links = BTreeMap::new();
    if let Some(provider_id) = &playlist.provider_id {
        upsert_provider_link(
            &mut provider_links,
            provider,
            provider_id.clone(),
            LinkSource::Export,
            Some(1.0),
            observed_at,
        );
    }
    let mut provider_state = BTreeMap::new();
    provider_state.insert(
        provider.as_key().to_string(),
        SyncStatusRecord::synced(
            playlist.provider_id.clone(),
            Some(1.0),
            Some("Observed in provider export".to_string()),
            observed_at,
        ),
    );

    state.playlists.push(PlaylistEntity {
        id: id.clone(),
        name: playlist.name.clone(),
        description: playlist.description.clone(),
        provider_links,
        provider_state,
        entries: Vec::new(),
    });

    id
}

fn find_existing_playlist_index(
    state: &LibraryState,
    provider: ProviderKind,
    observed: &ObservedPlaylist,
) -> Option<usize> {
    if let Some(provider_id) = &observed.provider_id {
        if let Some(index) = state.playlists.iter().position(|playlist| {
            playlist
                .provider_links
                .get(provider.as_key())
                .map(|link| link.provider_id.as_str())
                == Some(provider_id.as_str())
        }) {
            return Some(index);
        }
    }

    state
        .playlists
        .iter()
        .position(|playlist| playlist.name.eq_ignore_ascii_case(&observed.name))
}
