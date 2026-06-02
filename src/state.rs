use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::matching::metadata_similarity;
use crate::model::{
    LibraryState, LinkSource, MergeSummary, ObservedPlaylist, ObservedTrack, ProviderKind,
    ProviderLibrarySnapshot, ProviderPlaylistLink, ProviderTrackArtwork, ProviderTrackLink,
    SavedTrackEntry, SavedTrackSyncTarget, SyncState, SyncStatusRecord, TrackEntity, TrackMetadata,
};
use crate::model::{PlaylistEntity, PlaylistEntry, PlaylistEntrySyncTarget, PlaylistSyncTarget};

const TRACK_MATCH_THRESHOLD: f64 = 0.94;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrackIdentityApplyResult {
    Linked {
        track_id: String,
    },
    AlreadyLinked {
        track_id: String,
    },
    Merged {
        source_track_id: String,
        target_track_id: String,
    },
}

impl TrackIdentityApplyResult {
    pub fn track_id(&self) -> &str {
        match self {
            Self::Linked { track_id } | Self::AlreadyLinked { track_id } => track_id,
            Self::Merged {
                target_track_id, ..
            } => target_track_id,
        }
    }
}

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

    for saved_track in snapshot.saved_tracks {
        let (track_id, created) = upsert_track_from_observation(
            state,
            provider,
            &saved_track.track,
            LinkSource::Export,
            Some(1.0),
            observed_at,
        );
        if created {
            summary.tracks_created += 1;
        }

        let saved_track_id = ensure_saved_track(state, &track_id, saved_track.added_at);
        set_track_status(
            state,
            &track_id,
            provider,
            SyncStatusRecord::synced(
                saved_track.track.provider_id.clone(),
                Some(1.0),
                Some("Observed in provider export".to_string()),
                observed_at,
            ),
        );
        set_saved_track_status(
            state,
            &saved_track_id,
            provider,
            SyncStatusRecord::synced(
                saved_track.track.provider_id.clone(),
                Some(1.0),
                Some("Observed in provider export".to_string()),
                observed_at,
            ),
        );
        summary.saved_tracks_seen += 1;
    }

    for playlist in snapshot.playlists {
        let playlist_id = upsert_playlist_from_observation(state, provider, &playlist, observed_at);
        summary.playlists_seen += 1;

        let observed_entries =
            collect_playlist_observations(state, provider, &playlist, observed_at);
        summary.playlist_entries_seen += observed_entries.len();
        merge_playlist_entries(state, &playlist_id, provider, observed_entries, observed_at);
    }

    state.touch();
    summary
}

impl LibraryState {
    pub fn saved_track_targets(
        &self,
        provider: ProviderKind,
    ) -> anyhow::Result<Vec<SavedTrackSyncTarget>> {
        self.saved_tracks
            .iter()
            .map(|saved_track| {
                let track = self
                    .tracks
                    .iter()
                    .find(|track| track.id == saved_track.track_id)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Saved track {} references missing track {}",
                            saved_track.id,
                            saved_track.track_id
                        )
                    })?;
                Ok(SavedTrackSyncTarget {
                    saved_track_id: saved_track.id.clone(),
                    track_id: track.id.clone(),
                    metadata: track.metadata.clone(),
                    existing_provider_id: track
                        .provider_links
                        .get(provider.as_key())
                        .map(|link| link.provider_id.clone()),
                })
            })
            .collect()
    }

    pub fn playlist_targets(
        &self,
        provider: ProviderKind,
    ) -> anyhow::Result<Vec<PlaylistSyncTarget>> {
        self.playlists
            .iter()
            .map(|playlist| {
                let entries = playlist
                    .entries
                    .iter()
                    .map(|entry| {
                        let track = self
                            .tracks
                            .iter()
                            .find(|track| track.id == entry.track_id)
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "Playlist entry {} references missing track {}",
                                    entry.id,
                                    entry.track_id
                                )
                            })?;
                        Ok(PlaylistEntrySyncTarget {
                            entry_id: entry.id.clone(),
                            track_id: track.id.clone(),
                            added_at: entry.added_at.clone(),
                            metadata: track.metadata.clone(),
                            existing_provider_id: track
                                .provider_links
                                .get(provider.as_key())
                                .map(|link| link.provider_id.clone()),
                        })
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;

                Ok(PlaylistSyncTarget {
                    playlist_id: playlist.id.clone(),
                    name: playlist.name.clone(),
                    description: playlist.description.clone(),
                    existing_provider_id: playlist
                        .provider_links
                        .get(provider.as_key())
                        .map(|link| link.provider_id.clone()),
                    entries,
                })
            })
            .collect()
    }

    pub fn upsert_track_link(
        &mut self,
        track_id: &str,
        provider: ProviderKind,
        provider_id: impl Into<String>,
        source: LinkSource,
        confidence: Option<f64>,
        seen_at: DateTime<Utc>,
    ) -> bool {
        let provider_id = provider_id.into();
        let key = provider.as_key().to_string();
        let provider_id_owned_elsewhere = self.tracks.iter().any(|track| {
            track.id != track_id
                && track
                    .provider_links
                    .get(&key)
                    .map(|link| link.provider_id == provider_id)
                    .unwrap_or(false)
        });
        if provider_id_owned_elsewhere {
            return false;
        }

        if let Some(track) = self.tracks.iter_mut().find(|track| track.id == track_id) {
            if let Some(existing) = track.provider_links.get_mut(&key) {
                existing.provider_id = provider_id;
                existing.confidence = confidence.or(existing.confidence);
                existing.last_seen_at = Some(seen_at);
            } else {
                track.provider_links.insert(
                    key,
                    ProviderTrackLink {
                        provider_id,
                        source,
                        confidence,
                        linked_at: seen_at,
                        last_seen_at: Some(seen_at),
                    },
                );
            }
            true
        } else {
            false
        }
    }

    pub fn apply_track_identity(
        &mut self,
        track_id: &str,
        provider: ProviderKind,
        provider_id: impl Into<String>,
        source: LinkSource,
        confidence: Option<f64>,
        seen_at: DateTime<Utc>,
    ) -> anyhow::Result<TrackIdentityApplyResult> {
        let provider_id = provider_id.into();
        let key = provider.as_key();
        if !self.tracks.iter().any(|track| track.id == track_id) {
            anyhow::bail!("Unknown track '{track_id}'");
        }

        let existing_owner = self.tracks.iter().find_map(|track| {
            track
                .provider_links
                .get(key)
                .filter(|link| link.provider_id == provider_id)
                .map(|_| track.id.clone())
        });

        match existing_owner {
            Some(owner_id) if owner_id == track_id => {
                self.upsert_track_link(
                    track_id,
                    provider,
                    provider_id,
                    source,
                    confidence,
                    seen_at,
                );
                Ok(TrackIdentityApplyResult::AlreadyLinked {
                    track_id: track_id.to_string(),
                })
            }
            Some(owner_id) => {
                self.merge_track_into(track_id, &owner_id)?;
                self.upsert_track_link(
                    &owner_id,
                    provider,
                    provider_id,
                    source,
                    confidence,
                    seen_at,
                );
                Ok(TrackIdentityApplyResult::Merged {
                    source_track_id: track_id.to_string(),
                    target_track_id: owner_id,
                })
            }
            None => {
                self.upsert_track_link(
                    track_id,
                    provider,
                    provider_id,
                    source,
                    confidence,
                    seen_at,
                );
                Ok(TrackIdentityApplyResult::Linked {
                    track_id: track_id.to_string(),
                })
            }
        }
    }

    pub fn merge_track_into(
        &mut self,
        source_track_id: &str,
        target_track_id: &str,
    ) -> anyhow::Result<bool> {
        if source_track_id == target_track_id {
            return Ok(false);
        }

        let source = self
            .tracks
            .iter()
            .find(|track| track.id == source_track_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Unknown source track '{source_track_id}'"))?;
        if !self.tracks.iter().any(|track| track.id == target_track_id) {
            anyhow::bail!("Unknown target track '{target_track_id}'");
        }

        {
            let target = self
                .tracks
                .iter_mut()
                .find(|track| track.id == target_track_id)
                .expect("target was checked above");
            merge_metadata(&mut target.metadata, &source.metadata);
            merge_provider_links(&mut target.provider_links, source.provider_links)?;
            merge_provider_artwork(&mut target.provider_artwork, source.provider_artwork);
            merge_status_maps(&mut target.provider_state, source.provider_state);
        }

        for saved_track in &mut self.saved_tracks {
            if saved_track.track_id == source_track_id {
                saved_track.track_id = target_track_id.to_string();
            }
        }
        for playlist in &mut self.playlists {
            for entry in &mut playlist.entries {
                if entry.track_id == source_track_id {
                    entry.track_id = target_track_id.to_string();
                }
            }
        }

        self.tracks.retain(|track| track.id != source_track_id);
        self.consolidate_duplicate_saved_tracks();
        self.touch();
        Ok(true)
    }

    pub fn consolidate_duplicate_saved_tracks(&mut self) -> usize {
        let mut consolidated = Vec::<SavedTrackEntry>::new();
        let mut removed = 0usize;

        for saved_track in self.saved_tracks.drain(..) {
            if let Some(existing) = consolidated
                .iter_mut()
                .find(|existing| existing.track_id == saved_track.track_id)
            {
                merge_added_at(&mut existing.added_at, saved_track.added_at);
                merge_status_maps(&mut existing.provider_state, saved_track.provider_state);
                removed += 1;
            } else {
                consolidated.push(saved_track);
            }
        }

        if removed > 0 {
            self.saved_tracks = consolidated;
            self.touch();
        } else {
            self.saved_tracks = consolidated;
        }

        removed
    }

    pub fn set_track_status(
        &mut self,
        track_id: &str,
        provider: ProviderKind,
        status: SyncStatusRecord,
    ) {
        set_track_status(self, track_id, provider, status);
        self.touch();
    }

    pub fn upsert_track_artwork(
        &mut self,
        track_id: &str,
        provider: ProviderKind,
        url: impl Into<String>,
        width: Option<u32>,
        height: Option<u32>,
        seen_at: DateTime<Utc>,
    ) {
        if let Some(track) = self.tracks.iter_mut().find(|track| track.id == track_id) {
            let url = url.into();
            let key = provider.as_key().to_string();
            if let Some(existing) = track.provider_artwork.get_mut(&key) {
                existing.url = url;
                existing.width = preferred_dimension(existing.width, width);
                existing.height = preferred_dimension(existing.height, height);
                existing.last_seen_at = Some(seen_at);
            } else {
                track.provider_artwork.insert(
                    key,
                    ProviderTrackArtwork {
                        url,
                        width,
                        height,
                        last_seen_at: Some(seen_at),
                    },
                );
            }
            self.touch();
        }
    }

    pub fn set_saved_track_status(
        &mut self,
        saved_track_id: &str,
        provider: ProviderKind,
        status: SyncStatusRecord,
    ) {
        set_saved_track_status(self, saved_track_id, provider, status);
        self.touch();
    }

    pub fn upsert_playlist_link(
        &mut self,
        playlist_id: &str,
        provider: ProviderKind,
        provider_id: impl Into<String>,
        source: LinkSource,
        confidence: Option<f64>,
        seen_at: DateTime<Utc>,
    ) {
        if let Some(playlist) = self
            .playlists
            .iter_mut()
            .find(|playlist| playlist.id == playlist_id)
        {
            let provider_id = provider_id.into();
            let key = provider.as_key().to_string();
            if let Some(existing) = playlist.provider_links.get_mut(&key) {
                existing.provider_id = provider_id;
                existing.confidence = confidence.or(existing.confidence);
                existing.last_seen_at = Some(seen_at);
            } else {
                playlist.provider_links.insert(
                    key,
                    ProviderPlaylistLink {
                        provider_id,
                        source,
                        confidence,
                        linked_at: seen_at,
                        last_seen_at: Some(seen_at),
                    },
                );
            }
        }
    }

    pub fn set_playlist_status(
        &mut self,
        playlist_id: &str,
        provider: ProviderKind,
        status: SyncStatusRecord,
    ) {
        if let Some(playlist) = self
            .playlists
            .iter_mut()
            .find(|playlist| playlist.id == playlist_id)
        {
            playlist
                .provider_state
                .insert(provider.as_key().to_string(), status);
        }
        self.touch();
    }

    pub fn set_playlist_entry_status(
        &mut self,
        playlist_id: &str,
        entry_id: &str,
        provider: ProviderKind,
        status: SyncStatusRecord,
    ) {
        if let Some(playlist) = self
            .playlists
            .iter_mut()
            .find(|playlist| playlist.id == playlist_id)
        {
            if let Some(entry) = playlist
                .entries
                .iter_mut()
                .find(|entry| entry.id == entry_id)
            {
                entry
                    .provider_state
                    .insert(provider.as_key().to_string(), status);
            }
        }
        self.touch();
    }

    pub fn clear_playlist_provider_state(&mut self, provider: ProviderKind) {
        let key = provider.as_key();
        for playlist in &mut self.playlists {
            playlist.provider_links.remove(key);
            playlist.provider_state.remove(key);
            for entry in &mut playlist.entries {
                entry.provider_state.remove(key);
            }
        }
        self.touch();
    }

    pub fn remove_saved_track(&mut self, saved_track_id: &str) -> bool {
        let Some(index) = self
            .saved_tracks
            .iter()
            .position(|saved_track| saved_track.id == saved_track_id)
        else {
            return false;
        };

        let track_id = self.saved_tracks[index].track_id.clone();
        self.saved_tracks.remove(index);
        prune_track_if_unreferenced(self, &track_id);
        self.touch();
        true
    }

    pub fn remove_playlist(&mut self, playlist_id: &str) -> bool {
        let Some(index) = self
            .playlists
            .iter()
            .position(|playlist| playlist.id == playlist_id)
        else {
            return false;
        };

        let track_ids = self.playlists[index]
            .entries
            .iter()
            .map(|entry| entry.track_id.clone())
            .collect::<Vec<_>>();

        self.playlists.remove(index);
        for track_id in track_ids {
            prune_track_if_unreferenced(self, &track_id);
        }
        self.touch();
        true
    }

    pub fn remove_playlist_entry(&mut self, playlist_id: &str, entry_id: &str) -> bool {
        let Some(track_id) = ({
            let Some(playlist) = self
                .playlists
                .iter_mut()
                .find(|playlist| playlist.id == playlist_id)
            else {
                return false;
            };

            let Some(index) = playlist
                .entries
                .iter()
                .position(|entry| entry.id == entry_id)
            else {
                return false;
            };

            let track_id = playlist.entries[index].track_id.clone();
            playlist.entries.remove(index);
            Some(track_id)
        }) else {
            return false;
        };

        prune_track_if_unreferenced(self, &track_id);
        self.touch();
        true
    }

    pub fn remove_track_everywhere(&mut self, track_id: &str) -> bool {
        let mut changed = false;

        let saved_before = self.saved_tracks.len();
        self.saved_tracks
            .retain(|saved_track| saved_track.track_id != track_id);
        changed |= self.saved_tracks.len() != saved_before;

        for playlist in &mut self.playlists {
            let entry_before = playlist.entries.len();
            playlist.entries.retain(|entry| entry.track_id != track_id);
            changed |= playlist.entries.len() != entry_before;
        }

        if let Some(index) = self.tracks.iter().position(|track| track.id == track_id) {
            self.tracks.remove(index);
            changed = true;
        }

        if changed {
            self.touch();
        }

        changed
    }

    pub fn update_playlist_details(
        &mut self,
        playlist_id: &str,
        name: impl Into<String>,
        description: Option<String>,
    ) -> anyhow::Result<()> {
        let name = sanitize_required_text(name.into(), "Playlist name")?;
        let description = normalize_optional_text(description);
        let playlist = self
            .playlists
            .iter_mut()
            .find(|playlist| playlist.id == playlist_id)
            .ok_or_else(|| anyhow::anyhow!("Unknown playlist '{playlist_id}'"))?;

        playlist.name = name;
        playlist.description = description;
        self.touch();
        Ok(())
    }

    pub fn update_track_metadata(
        &mut self,
        track_id: &str,
        metadata: TrackMetadata,
    ) -> anyhow::Result<()> {
        let metadata = sanitize_track_metadata(metadata)?;
        let track = self
            .tracks
            .iter_mut()
            .find(|track| track.id == track_id)
            .ok_or_else(|| anyhow::anyhow!("Unknown track '{track_id}'"))?;

        track.metadata = metadata;
        self.touch();
        Ok(())
    }
}

fn collect_playlist_observations(
    state: &mut LibraryState,
    provider: ProviderKind,
    playlist: &ObservedPlaylist,
    observed_at: DateTime<Utc>,
) -> Vec<(String, Option<String>, Option<String>)> {
    playlist
        .tracks
        .iter()
        .map(|track| {
            let (track_id, _) = upsert_track_from_observation(
                state,
                provider,
                &track.track,
                LinkSource::Export,
                Some(1.0),
                observed_at,
            );
            set_track_status(
                state,
                &track_id,
                provider,
                SyncStatusRecord::synced(
                    track.track.provider_id.clone(),
                    Some(1.0),
                    Some("Observed in provider export".to_string()),
                    observed_at,
                ),
            );
            (
                track_id,
                track.added_at.clone(),
                track.track.provider_id.clone(),
            )
        })
        .collect()
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
                let mut provider_state = std::collections::BTreeMap::new();
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
    provider: ProviderKind,
    observed: &ObservedTrack,
    source: LinkSource,
    confidence: Option<f64>,
    seen_at: DateTime<Utc>,
) -> (String, bool) {
    if let Some(index) = find_existing_track_index(state, provider, observed) {
        let track = &mut state.tracks[index];
        merge_metadata(&mut track.metadata, &observed.metadata);
        upsert_track_artwork(track, provider, observed, seen_at);
        if let Some(provider_id) = &observed.provider_id {
            let key = provider.as_key().to_string();
            if let Some(existing) = track.provider_links.get_mut(&key) {
                existing.provider_id = provider_id.clone();
                existing.confidence = confidence.or(existing.confidence);
                existing.last_seen_at = Some(seen_at);
            } else {
                track.provider_links.insert(
                    key,
                    ProviderTrackLink {
                        provider_id: provider_id.clone(),
                        source,
                        confidence,
                        linked_at: seen_at,
                        last_seen_at: Some(seen_at),
                    },
                );
            }
        }

        return (track.id.clone(), false);
    }

    let id = new_canonical_id("track");
    let mut provider_links = std::collections::BTreeMap::new();
    if let Some(provider_id) = &observed.provider_id {
        provider_links.insert(
            provider.as_key().to_string(),
            ProviderTrackLink {
                provider_id: provider_id.clone(),
                source,
                confidence,
                linked_at: seen_at,
                last_seen_at: Some(seen_at),
            },
        );
    }
    let mut provider_artwork = std::collections::BTreeMap::new();
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

    state.tracks.push(TrackEntity {
        id: id.clone(),
        metadata: observed.metadata.clone(),
        provider_links,
        provider_artwork,
        provider_state: Default::default(),
    });

    (id, true)
}

fn find_existing_track_index(
    state: &LibraryState,
    provider: ProviderKind,
    observed: &ObservedTrack,
) -> Option<usize> {
    if let Some(provider_id) = &observed.provider_id {
        if let Some(index) = state.tracks.iter().position(|track| {
            track
                .provider_links
                .get(provider.as_key())
                .map(|link| link.provider_id.as_str())
                == Some(provider_id.as_str())
        }) {
            return Some(index);
        }
    }

    if let Some(isrc) = &observed.metadata.isrc {
        if let Some(index) = state
            .tracks
            .iter()
            .position(|track| track.metadata.isrc.as_deref() == Some(isrc.as_str()))
        {
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

fn merge_metadata(existing: &mut TrackMetadata, observed: &TrackMetadata) {
    if existing.title.trim().is_empty() || existing.title == "Unknown" {
        existing.title = observed.title.clone();
    }

    if existing.artists.is_empty() && !observed.artists.is_empty() {
        existing.artists = observed.artists.clone();
    }

    if existing.album.is_none() && observed.album.is_some() {
        existing.album = observed.album.clone();
    }

    if existing.duration_seconds.is_none() {
        existing.duration_seconds = observed.duration_seconds;
    }

    if existing.isrc.is_none() {
        existing.isrc = observed.isrc.clone();
    }
}

fn upsert_track_artwork(
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

fn merge_provider_links(
    target: &mut BTreeMap<String, ProviderTrackLink>,
    source: BTreeMap<String, ProviderTrackLink>,
) -> anyhow::Result<()> {
    for (provider, source_link) in source {
        if let Some(target_link) = target.get_mut(&provider) {
            if target_link.provider_id != source_link.provider_id {
                anyhow::bail!(
                    "Cannot merge tracks because provider '{}' has conflicting IDs '{}' and '{}'.",
                    provider,
                    target_link.provider_id,
                    source_link.provider_id
                );
            }
            target_link.confidence =
                preferred_confidence(target_link.confidence, source_link.confidence);
            target_link.last_seen_at = target_link.last_seen_at.or(source_link.last_seen_at);
        } else {
            target.insert(provider, source_link);
        }
    }
    Ok(())
}

fn merge_provider_artwork(
    target: &mut BTreeMap<String, ProviderTrackArtwork>,
    source: BTreeMap<String, ProviderTrackArtwork>,
) {
    for (provider, source_artwork) in source {
        if let Some(target_artwork) = target.get_mut(&provider) {
            target_artwork.width = preferred_dimension(target_artwork.width, source_artwork.width);
            target_artwork.height =
                preferred_dimension(target_artwork.height, source_artwork.height);
            target_artwork.last_seen_at =
                target_artwork.last_seen_at.or(source_artwork.last_seen_at);
            if target_artwork.url.trim().is_empty() {
                target_artwork.url = source_artwork.url;
            }
        } else {
            target.insert(provider, source_artwork);
        }
    }
}

fn merge_status_maps(
    target: &mut BTreeMap<String, SyncStatusRecord>,
    source: BTreeMap<String, SyncStatusRecord>,
) {
    for (provider, source_status) in source {
        if let Some(target_status) = target.get_mut(&provider) {
            if status_rank(source_status.state) > status_rank(target_status.state) {
                *target_status = source_status;
            } else {
                target_status.last_attempt_at = target_status
                    .last_attempt_at
                    .or(source_status.last_attempt_at);
                target_status.last_success_at = target_status
                    .last_success_at
                    .or(source_status.last_success_at);
                target_status.last_seen_at =
                    target_status.last_seen_at.or(source_status.last_seen_at);
            }
        } else {
            target.insert(provider, source_status);
        }
    }
}

fn status_rank(state: SyncState) -> u8 {
    match state {
        SyncState::Synced => 6,
        SyncState::Error => 5,
        SyncState::Missing => 4,
        SyncState::Unmatched => 3,
        SyncState::Skipped => 2,
        SyncState::Pending => 1,
    }
}

fn preferred_confidence(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn merge_added_at(target: &mut Option<String>, source: Option<String>) {
    match (target.as_ref(), source) {
        (None, Some(source)) => *target = Some(source),
        (Some(existing), Some(source)) if source < *existing => *target = Some(source),
        _ => {}
    }
}

fn preferred_dimension(existing: Option<u32>, observed: Option<u32>) -> Option<u32> {
    match (existing, observed) {
        (Some(existing), Some(observed)) => Some(existing.max(observed)),
        (Some(existing), None) => Some(existing),
        (None, Some(observed)) => Some(observed),
        (None, None) => None,
    }
}

fn ensure_saved_track(
    state: &mut LibraryState,
    track_id: &str,
    added_at: Option<String>,
) -> String {
    if let Some(saved_track) = state
        .saved_tracks
        .iter_mut()
        .find(|saved_track| saved_track.track_id == track_id)
    {
        if saved_track.added_at.is_none() {
            saved_track.added_at = added_at;
        }
        return saved_track.id.clone();
    }

    let id = new_canonical_id("saved-track");
    state.saved_tracks.push(SavedTrackEntry {
        id: id.clone(),
        track_id: track_id.to_string(),
        added_at,
        provider_state: Default::default(),
    });
    id
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
            let key = provider.as_key().to_string();
            if let Some(existing) = playlist_entity.provider_links.get_mut(&key) {
                existing.provider_id = provider_id.clone();
                existing.last_seen_at = Some(observed_at);
            } else {
                playlist_entity.provider_links.insert(
                    key,
                    ProviderPlaylistLink {
                        provider_id: provider_id.clone(),
                        source: LinkSource::Export,
                        confidence: Some(1.0),
                        linked_at: observed_at,
                        last_seen_at: Some(observed_at),
                    },
                );
            }
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
    let mut provider_links = std::collections::BTreeMap::new();
    if let Some(provider_id) = &playlist.provider_id {
        provider_links.insert(
            provider.as_key().to_string(),
            ProviderPlaylistLink {
                provider_id: provider_id.clone(),
                source: LinkSource::Export,
                confidence: Some(1.0),
                linked_at: observed_at,
                last_seen_at: Some(observed_at),
            },
        );
    }
    let mut provider_state = std::collections::BTreeMap::new();
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

fn set_saved_track_status(
    state: &mut LibraryState,
    saved_track_id: &str,
    provider: ProviderKind,
    status: SyncStatusRecord,
) {
    if let Some(saved_track) = state
        .saved_tracks
        .iter_mut()
        .find(|saved_track| saved_track.id == saved_track_id)
    {
        saved_track
            .provider_state
            .insert(provider.as_key().to_string(), status);
    }
}

fn set_track_status(
    state: &mut LibraryState,
    track_id: &str,
    provider: ProviderKind,
    status: SyncStatusRecord,
) {
    if let Some(track) = state.tracks.iter_mut().find(|track| track.id == track_id) {
        track
            .provider_state
            .insert(provider.as_key().to_string(), status);
    }
}

fn prune_track_if_unreferenced(state: &mut LibraryState, track_id: &str) -> bool {
    let is_still_saved = state
        .saved_tracks
        .iter()
        .any(|saved_track| saved_track.track_id == track_id);
    let is_still_in_playlist = state.playlists.iter().any(|playlist| {
        playlist
            .entries
            .iter()
            .any(|entry| entry.track_id == track_id)
    });

    if is_still_saved || is_still_in_playlist {
        return false;
    }

    if let Some(index) = state.tracks.iter().position(|track| track.id == track_id) {
        state.tracks.remove(index);
        true
    } else {
        false
    }
}

fn sanitize_track_metadata(mut metadata: TrackMetadata) -> anyhow::Result<TrackMetadata> {
    metadata.title = sanitize_required_text(metadata.title, "Track title")?;
    metadata.artists = sanitize_artists(metadata.artists);
    metadata.album = normalize_optional_text(metadata.album);
    metadata.isrc = normalize_optional_text(metadata.isrc).map(|value| value.to_ascii_uppercase());
    metadata.duration_seconds = metadata.duration_seconds.filter(|value| *value > 0);
    Ok(metadata)
}

fn sanitize_artists(artists: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for artist in artists {
        let artist = artist.trim();
        if artist.is_empty() {
            continue;
        }
        if normalized
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(artist))
        {
            continue;
        }
        normalized.push(artist.to_string());
    }
    normalized
}

fn sanitize_required_text(value: String, label: &str) -> anyhow::Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("{label} cannot be empty");
    }
    Ok(trimmed.to_string())
}

fn normalize_optional_text(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

pub fn new_canonical_id(prefix: &str) -> String {
    format!("{}_{}", prefix, Uuid::new_v4().simple())
}
