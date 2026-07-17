use std::collections::{BTreeMap, HashMap};

use chrono::Utc;

use crate::domain::{
    LibraryState, PlaylistEntity, PlaylistEntry, ProviderConnection, ProviderKind, SavedTrackEntry,
    TrackEntity,
};
use crate::provider::ProviderCapability;

use super::error::ApiError;
use super::providers::build_provider_from_connection;

pub(crate) fn saved_track_provider_links(
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

pub(crate) fn track_provider_links(
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

pub(crate) fn playlist_provider_links(
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

pub(crate) fn playlist_ids_for_track(state: &LibraryState, track_id: &str) -> Vec<String> {
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

pub(crate) async fn propagate_saved_track_delete(
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

pub(crate) async fn propagate_playlist_delete(
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

pub(crate) async fn propagate_playlist_subset_to_connected_providers(
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

pub(crate) fn playlist_subset_state(state: &LibraryState, playlist_ids: &[String]) -> LibraryState {
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

pub(crate) fn merge_subset_state(state: &mut LibraryState, subset: &LibraryState) {
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
pub(crate) fn reapply_provider_sync(
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
pub(crate) fn reapply_map_entry<V: Clone>(
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

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use crate::domain::{LibraryState, LinkSource, ProviderKind, ProviderTrackLink};
    use crate::web::test_support::test_track_with_link;

    use super::reapply_provider_sync;

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
}
