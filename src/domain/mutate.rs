use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use uuid::Uuid;

use super::library::{LibraryState, SavedTrackEntry};
use super::playlist::ProviderPlaylistLink;
use super::provider::ProviderKind;
use super::sync::{
    NewProviderLink, PlaylistEntrySyncTarget, PlaylistSyncTarget, PushOutcome, PushPlan,
    SavedTrackSyncTarget, SyncState, SyncStatusRecord, SyncSummary,
};
use super::track::{
    LinkSource, ProviderTrackArtwork, ProviderTrackLink, TrackEntity, TrackMetadata,
};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrackMergeConflictResolution {
    KeepSource,
    KeepTarget,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTrackMergeConflict {
    pub provider_key: String,
    pub kept_provider_id: String,
    pub dropped_provider_id: String,
    pub kept_from_source: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackMergeResult {
    pub source_track_id: String,
    pub target_track_id: String,
    pub resolved_conflicts: Vec<ResolvedTrackMergeConflict>,
}

impl LibraryState {
    pub fn saved_track_targets(
        &self,
        provider: ProviderKind,
    ) -> anyhow::Result<Vec<SavedTrackSyncTarget>> {
        let tracks_by_id: HashMap<&str, &TrackEntity> = self
            .tracks
            .iter()
            .map(|track| (track.id.as_str(), track))
            .collect();
        self.saved_tracks
            .iter()
            .map(|saved_track| {
                let track = tracks_by_id
                    .get(saved_track.track_id.as_str())
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
        let tracks_by_id: HashMap<&str, &TrackEntity> = self
            .tracks
            .iter()
            .map(|track| (track.id.as_str(), track))
            .collect();
        self.playlists
            .iter()
            .map(|playlist| {
                let entries = playlist
                    .entries
                    .iter()
                    .map(|entry| {
                        let track = tracks_by_id.get(entry.track_id.as_str()).ok_or_else(|| {
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

    pub fn push_plan(&self, provider: ProviderKind) -> anyhow::Result<PushPlan> {
        Ok(PushPlan {
            saved_tracks: self.saved_track_targets(provider)?,
            playlists: self.playlist_targets(provider)?,
        })
    }

    /// Links `track_id` to a provider track. Returns false when the provider
    /// ID is already owned by another track or when `track_id` is unknown; the
    /// existing owner's link is never overwritten.
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
        let key = provider.as_key();
        let provider_id_owned_elsewhere = self.tracks.iter().any(|track| {
            track.id != track_id
                && track
                    .provider_links
                    .get(key)
                    .map(|link| link.provider_id == provider_id)
                    .unwrap_or(false)
        });
        if provider_id_owned_elsewhere {
            return false;
        }

        if let Some(track) = self.tracks.iter_mut().find(|track| track.id == track_id) {
            upsert_provider_link(
                &mut track.provider_links,
                provider,
                provider_id,
                source,
                confidence,
                seen_at,
            );
            true
        } else {
            false
        }
    }

    /// Records an open identity conflict on `track_id` unless one with the
    /// same candidate already exists. Returns true when a conflict was added.
    pub fn record_track_identity_conflict(
        &mut self,
        track_id: &str,
        provider: ProviderKind,
        candidate_provider_id: impl Into<String>,
        confidence: Option<f64>,
        detected_at: DateTime<Utc>,
    ) -> bool {
        let Some(track) = self.tracks.iter_mut().find(|track| track.id == track_id) else {
            return false;
        };
        let added = track.record_identity_conflict(
            provider,
            candidate_provider_id,
            confidence,
            detected_at,
        );
        if added {
            self.touch();
        }
        added
    }

    /// Rejects an open identity conflict on `track_id`, turning it into a
    /// permanent tombstone so the candidate is never re-proposed. Returns true
    /// when an open conflict was rejected.
    pub fn reject_track_identity_conflict(
        &mut self,
        track_id: &str,
        provider: ProviderKind,
        candidate_provider_id: &str,
        rejected_at: DateTime<Utc>,
    ) -> bool {
        let Some(track) = self.tracks.iter_mut().find(|track| track.id == track_id) else {
            return false;
        };
        let rejected = track.reject_identity_conflict(provider, candidate_provider_id, rejected_at);
        if rejected {
            self.touch();
        }
        rejected
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
        self.merge_track_into_with_conflict_resolution(
            source_track_id,
            target_track_id,
            TrackMergeConflictResolution::KeepTarget,
            Utc::now(),
            false,
        )
        .map(|result| result.is_some())
    }

    pub fn merge_track_into_resolving_conflicts(
        &mut self,
        source_track_id: &str,
        target_track_id: &str,
        resolution: TrackMergeConflictResolution,
        merged_at: DateTime<Utc>,
    ) -> anyhow::Result<TrackMergeResult> {
        self.merge_track_into_with_conflict_resolution(
            source_track_id,
            target_track_id,
            resolution,
            merged_at,
            true,
        )?
        .ok_or_else(|| anyhow::anyhow!("Source and target track are already the same row."))
    }

    fn merge_track_into_with_conflict_resolution(
        &mut self,
        source_track_id: &str,
        target_track_id: &str,
        resolution: TrackMergeConflictResolution,
        merged_at: DateTime<Utc>,
        allow_conflicts: bool,
    ) -> anyhow::Result<Option<TrackMergeResult>> {
        if source_track_id == target_track_id {
            return Ok(None);
        }

        let source = self
            .tracks
            .iter()
            .find(|track| track.id == source_track_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Unknown source track '{source_track_id}'"))?;
        let target_index = self
            .tracks
            .iter()
            .position(|track| track.id == target_track_id)
            .ok_or_else(|| anyhow::anyhow!("Unknown target track '{target_track_id}'"))?;

        // Resolve the fallible provider-link merge on a clone first so a
        // rejected merge leaves the state completely untouched.
        let mut merged_links = self.tracks[target_index].provider_links.clone();
        let resolved_conflicts = merge_provider_links_with_resolution(
            &mut merged_links,
            source.provider_links,
            resolution,
            allow_conflicts,
        )?;

        let target = &mut self.tracks[target_index];
        merge_metadata(&mut target.metadata, &source.metadata);
        target.provider_links = merged_links;
        merge_provider_artwork(&mut target.provider_artwork, source.provider_artwork);
        merge_status_maps(&mut target.provider_state, source.provider_state);
        for conflict in source.identity_conflicts {
            if !target.identity_conflicts.iter().any(|existing| {
                existing.provider == conflict.provider
                    && existing.candidate_provider_id == conflict.candidate_provider_id
            }) {
                target.identity_conflicts.push(conflict);
            }
        }
        for conflict in &resolved_conflicts {
            target.provider_state.insert(
                conflict.provider_key.clone(),
                SyncStatusRecord::synced(
                    Some(conflict.kept_provider_id.clone()),
                    Some(1.0),
                    Some(format!(
                        "Manual conflict merge kept {} ID '{}' and dropped alternate ID '{}'.",
                        provider_display_name(&conflict.provider_key),
                        conflict.kept_provider_id,
                        conflict.dropped_provider_id
                    )),
                    merged_at,
                ),
            );
        }

        let result = TrackMergeResult {
            source_track_id: source_track_id.to_string(),
            target_track_id: target_track_id.to_string(),
            resolved_conflicts,
        };

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
        Ok(Some(result))
    }

    pub fn consolidate_duplicate_saved_tracks(&mut self) -> usize {
        let mut consolidated = Vec::<SavedTrackEntry>::with_capacity(self.saved_tracks.len());
        let mut index_by_track_id = HashMap::<String, usize>::new();
        let mut removed = 0usize;

        for saved_track in self.saved_tracks.drain(..) {
            if let Some(&index) = index_by_track_id.get(&saved_track.track_id) {
                let existing = &mut consolidated[index];
                merge_added_at(&mut existing.added_at, saved_track.added_at);
                merge_status_maps(&mut existing.provider_state, saved_track.provider_state);
                removed += 1;
            } else {
                index_by_track_id.insert(saved_track.track_id.clone(), consolidated.len());
                consolidated.push(saved_track);
            }
        }

        self.saved_tracks = consolidated;
        if removed > 0 {
            self.touch();
        }

        removed
    }

    pub fn set_track_status(
        &mut self,
        track_id: &str,
        provider: ProviderKind,
        status: SyncStatusRecord,
    ) {
        if let Some(track) = self.tracks.iter_mut().find(|track| track.id == track_id) {
            track
                .provider_state
                .insert(provider.as_key().to_string(), status);
        }
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
            let key = provider.as_key().to_string();
            let candidate = ProviderTrackArtwork {
                url: url.into(),
                width,
                height,
                last_seen_at: Some(seen_at),
            };
            match track.provider_artwork.get_mut(&key) {
                Some(existing) => merge_artwork_observation(existing, candidate),
                None => {
                    track.provider_artwork.insert(key, candidate);
                }
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
        if let Some(saved_track) = self
            .saved_tracks
            .iter_mut()
            .find(|saved_track| saved_track.id == saved_track_id)
        {
            saved_track
                .provider_state
                .insert(provider.as_key().to_string(), status);
        }
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
            upsert_provider_link(
                &mut playlist.provider_links,
                provider,
                provider_id.into(),
                source,
                confidence,
                seen_at,
            );
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

/// Applies the per-item results of a provider push to the canonical state and
/// derives the `SyncSummary` totals from the outcome items in one place.
///
/// New provider links are applied through the ownership-guarded upsert: when a
/// pushed provider ID is already owned by another track, the existing link is
/// preserved, an open `TrackIdentityConflict` is recorded on the item's track,
/// the item's status is not applied (it would claim a link that does not
/// exist), and the item counts as unmatched.
pub fn apply_push_outcome(
    state: &mut LibraryState,
    provider: ProviderKind,
    outcome: &PushOutcome,
) -> SyncSummary {
    let mut summary = SyncSummary {
        warnings: outcome.warnings.clone(),
        ..Default::default()
    };

    for item in &outcome.saved_tracks {
        summary.saved_tracks_requested += 1;
        if !apply_new_track_link(
            state,
            provider,
            &item.track_id,
            item.new_link.as_ref(),
            &mut summary.warnings,
        ) {
            summary.saved_tracks_unmatched += 1;
            continue;
        }
        state.set_track_status(&item.track_id, provider, item.status.clone());
        state.set_saved_track_status(&item.canonical_id, provider, item.status.clone());
        match item.status.state {
            SyncState::Synced => summary.saved_tracks_synced += 1,
            SyncState::Unmatched => summary.saved_tracks_unmatched += 1,
            _ => {}
        }
    }

    for playlist_result in &outcome.playlists {
        summary.playlists_processed += 1;
        if let Some(link) = &playlist_result.new_link {
            state.upsert_playlist_link(
                &playlist_result.playlist_id,
                provider,
                link.provider_id.clone(),
                link.source,
                link.confidence,
                Utc::now(),
            );
        }
        state.set_playlist_status(
            &playlist_result.playlist_id,
            provider,
            playlist_result.status.clone(),
        );
        for entry in &playlist_result.entries {
            summary.playlist_entries_requested += 1;
            if !apply_new_track_link(
                state,
                provider,
                &entry.track_id,
                entry.new_link.as_ref(),
                &mut summary.warnings,
            ) {
                summary.playlist_entries_unmatched += 1;
                continue;
            }
            state.set_playlist_entry_status(
                &playlist_result.playlist_id,
                &entry.entry_id,
                provider,
                entry.status.clone(),
            );
            match entry.status.state {
                SyncState::Synced => summary.playlist_entries_synced += 1,
                SyncState::Unmatched => summary.playlist_entries_unmatched += 1,
                _ => {}
            }
        }
    }

    state.touch();
    summary
}

fn apply_new_track_link(
    state: &mut LibraryState,
    provider: ProviderKind,
    track_id: &str,
    new_link: Option<&NewProviderLink>,
    warnings: &mut Vec<String>,
) -> bool {
    let Some(link) = new_link else {
        return true;
    };
    let seen_at = Utc::now();
    if state.upsert_track_link(
        track_id,
        provider,
        link.provider_id.clone(),
        link.source,
        link.confidence,
        seen_at,
    ) {
        return true;
    }
    state.record_track_identity_conflict(
        track_id,
        provider,
        link.provider_id.clone(),
        link.confidence,
        seen_at,
    );
    warnings.push(format!(
        "Could not link {} ID '{}' to track '{}' because another track already owns it; recorded an identity conflict instead.",
        provider.display_name(),
        link.provider_id,
        track_id
    ));
    false
}

pub(super) trait ProviderLinkRecord {
    fn new_link(
        provider_id: String,
        source: LinkSource,
        confidence: Option<f64>,
        linked_at: DateTime<Utc>,
    ) -> Self;

    fn record_observation(
        &mut self,
        provider_id: String,
        confidence: Option<f64>,
        seen_at: DateTime<Utc>,
    );
}

impl ProviderLinkRecord for ProviderTrackLink {
    fn new_link(
        provider_id: String,
        source: LinkSource,
        confidence: Option<f64>,
        linked_at: DateTime<Utc>,
    ) -> Self {
        Self {
            provider_id,
            source,
            confidence,
            linked_at,
            last_seen_at: Some(linked_at),
        }
    }

    fn record_observation(
        &mut self,
        provider_id: String,
        confidence: Option<f64>,
        seen_at: DateTime<Utc>,
    ) {
        self.provider_id = provider_id;
        self.confidence = confidence.or(self.confidence);
        self.last_seen_at = Some(seen_at);
    }
}

impl ProviderLinkRecord for ProviderPlaylistLink {
    fn new_link(
        provider_id: String,
        source: LinkSource,
        confidence: Option<f64>,
        linked_at: DateTime<Utc>,
    ) -> Self {
        Self {
            provider_id,
            source,
            confidence,
            linked_at,
            last_seen_at: Some(linked_at),
        }
    }

    fn record_observation(
        &mut self,
        provider_id: String,
        confidence: Option<f64>,
        seen_at: DateTime<Utc>,
    ) {
        self.provider_id = provider_id;
        self.confidence = confidence.or(self.confidence);
        self.last_seen_at = Some(seen_at);
    }
}

/// Inserts or refreshes the provider link entry in `links`. Ownership checks
/// (whether another entity already claims the provider ID) are the caller's
/// responsibility; this helper only maintains the map for one entity.
pub(super) fn upsert_provider_link<L: ProviderLinkRecord>(
    links: &mut BTreeMap<String, L>,
    provider: ProviderKind,
    provider_id: String,
    source: LinkSource,
    confidence: Option<f64>,
    seen_at: DateTime<Utc>,
) {
    let key = provider.as_key().to_string();
    if let Some(existing) = links.get_mut(&key) {
        existing.record_observation(provider_id, confidence, seen_at);
    } else {
        links.insert(key, L::new_link(provider_id, source, confidence, seen_at));
    }
}

pub(super) fn merge_metadata(existing: &mut TrackMetadata, observed: &TrackMetadata) {
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

fn merge_provider_links_with_resolution(
    target: &mut BTreeMap<String, ProviderTrackLink>,
    source: BTreeMap<String, ProviderTrackLink>,
    resolution: TrackMergeConflictResolution,
    allow_conflicts: bool,
) -> anyhow::Result<Vec<ResolvedTrackMergeConflict>> {
    let mut resolved_conflicts = Vec::new();
    for (provider, source_link) in source {
        if let Some(target_link) = target.get_mut(&provider) {
            if target_link.provider_id != source_link.provider_id {
                if !allow_conflicts {
                    anyhow::bail!(
                        "Cannot merge tracks because provider '{}' has conflicting IDs '{}' and '{}'.",
                        provider,
                        target_link.provider_id,
                        source_link.provider_id
                    );
                }

                let (kept_provider_id, dropped_provider_id, kept_from_source) = match resolution {
                    TrackMergeConflictResolution::KeepSource => {
                        let dropped = target_link.provider_id.clone();
                        *target_link = source_link;
                        (target_link.provider_id.clone(), dropped, true)
                    }
                    TrackMergeConflictResolution::KeepTarget => (
                        target_link.provider_id.clone(),
                        source_link.provider_id,
                        false,
                    ),
                };
                resolved_conflicts.push(ResolvedTrackMergeConflict {
                    provider_key: provider,
                    kept_provider_id,
                    dropped_provider_id,
                    kept_from_source,
                });
                continue;
            }
            target_link.confidence =
                preferred_confidence(target_link.confidence, source_link.confidence);
            target_link.last_seen_at = target_link.last_seen_at.or(source_link.last_seen_at);
        } else {
            target.insert(provider, source_link);
        }
    }
    Ok(resolved_conflicts)
}

fn provider_display_name(key: &str) -> String {
    ProviderKind::from_key(key)
        .map(|provider| provider.display_name().to_string())
        .unwrap_or_else(|_| key.to_string())
}

fn merge_provider_artwork(
    target: &mut BTreeMap<String, ProviderTrackArtwork>,
    source: BTreeMap<String, ProviderTrackArtwork>,
) {
    for (provider, source_artwork) in source {
        match target.get_mut(&provider) {
            Some(target_artwork) => merge_artwork_observation(target_artwork, source_artwork),
            None => {
                target.insert(provider, source_artwork);
            }
        }
    }
}

/// Merges two provider status maps, keeping the higher-ranked record for each
/// provider (see [`status_rank`]).
///
/// A status record's timestamps (`last_attempt_at`/`last_success_at`/
/// `last_seen_at`) describe that specific record's own history — e.g. an
/// `Unmatched` record has no `last_success_at`, a `Synced` record's
/// `last_seen_at` is when the item was last confirmed present. So the kept
/// record keeps ONLY its own timestamps: the discarded record's timestamps are
/// discarded along with it, never merged in. This prevents incoherent rows such
/// as an `Unmatched` record that claims a success timestamp borrowed from a
/// dropped `Synced` record, or a kept record whose "last attempt" was actually
/// the discarded record's attempt.
pub(super) fn merge_status_maps(
    target: &mut BTreeMap<String, SyncStatusRecord>,
    source: BTreeMap<String, SyncStatusRecord>,
) {
    for (provider, source_status) in source {
        match target.get_mut(&provider) {
            Some(target_status) => {
                if status_rank(source_status.state) > status_rank(target_status.state) {
                    *target_status = source_status;
                }
            }
            None => {
                target.insert(provider, source_status);
            }
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

fn max_timestamp(
    left: Option<DateTime<Utc>>,
    right: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (left, right) => left.or(right),
    }
}

fn parse_added_at(value: &str) -> Option<DateTime<Utc>> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed.with_timezone(&Utc));
    }
    if let Ok(parsed) = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.fZ") {
        return Some(Utc.from_utc_datetime(&parsed));
    }
    if let Ok(parsed) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return Some(Utc.from_utc_datetime(&parsed.and_hms_opt(0, 0, 0)?));
    }
    None
}

pub(super) fn merge_added_at(target: &mut Option<String>, source: Option<String>) {
    let Some(source) = source else {
        return;
    };
    let Some(existing) = target.as_ref() else {
        *target = Some(source);
        return;
    };
    let source_is_earlier = match (parse_added_at(&source), parse_added_at(existing)) {
        (Some(source_at), Some(existing_at)) => source_at < existing_at,
        _ => source < *existing,
    };
    if source_is_earlier {
        *target = Some(source);
    }
}

/// The single rule for reconciling a stored provider-artwork record with a
/// newer observation of the same provider's artwork.
///
/// `(url, width, height)` is treated as ONE unit: the stored record always
/// describes a single real image, never one image's URL glued to another
/// image's dimensions. The candidate replaces the stored record as a whole when
/// it is at least as large (by [`artwork_dimension_score`], so any real
/// observation outranks a dimensionless record); otherwise the stored record is
/// kept whole. Either way `last_seen_at` advances to the newer timestamp, so
/// freshness reflects every observation regardless of which image won.
pub(super) fn merge_artwork_observation(
    existing: &mut ProviderTrackArtwork,
    candidate: ProviderTrackArtwork,
) {
    let last_seen_at = max_timestamp(existing.last_seen_at, candidate.last_seen_at);
    if artwork_dimension_score(candidate.width, candidate.height)
        >= artwork_dimension_score(existing.width, existing.height)
    {
        existing.url = candidate.url;
        existing.width = candidate.width;
        existing.height = candidate.height;
    }
    existing.last_seen_at = last_seen_at;
}

/// Ranks an artwork's dimensions so two observations can be compared as whole
/// images. A fully-known image is ranked by pixel area; a partially-known one
/// by its larger side (so it still outranks a dimensionless record); a
/// dimensionless record ranks lowest, at zero.
fn artwork_dimension_score(width: Option<u32>, height: Option<u32>) -> u64 {
    let width = width.unwrap_or(0) as u64;
    let height = height.unwrap_or(0) as u64;
    if width > 0 && height > 0 {
        width * height
    } else {
        width.max(height)
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

pub(super) fn sanitize_track_metadata(
    mut metadata: TrackMetadata,
) -> anyhow::Result<TrackMetadata> {
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

#[cfg(test)]
mod tests {
    use super::super::sync::{
        NewProviderLink, PushEntryResult, PushItemResult, PushOutcome, PushPlaylistResult,
    };
    use super::super::track::IdentityConflictStatus;
    use super::*;
    use crate::domain::PlaylistEntity;
    use crate::domain::PlaylistEntry;

    fn metadata(title: &str) -> TrackMetadata {
        TrackMetadata {
            title: title.to_string(),
            artists: vec!["Artist".to_string()],
            album: None,
            duration_seconds: None,
            isrc: None,
        }
    }

    fn track(id: &str, title: &str) -> TrackEntity {
        TrackEntity {
            id: id.to_string(),
            metadata: metadata(title),
            provider_links: BTreeMap::new(),
            provider_artwork: BTreeMap::new(),
            provider_state: BTreeMap::new(),
            identity_conflicts: Vec::new(),
        }
    }

    fn saved(id: &str, track_id: &str) -> SavedTrackEntry {
        SavedTrackEntry {
            id: id.to_string(),
            track_id: track_id.to_string(),
            added_at: None,
            provider_state: BTreeMap::new(),
        }
    }

    fn new_link(provider_id: &str) -> Option<NewProviderLink> {
        Some(NewProviderLink {
            provider_id: provider_id.to_string(),
            source: LinkSource::Match,
            confidence: Some(0.97),
        })
    }

    #[test]
    fn apply_push_outcome_applies_statuses_links_and_summary_counts() {
        let now = Utc::now();
        let mut state = LibraryState::new();
        state.tracks.push(track("track-1", "Sirius"));
        state.tracks.push(track("track-2", "Mammagamma"));
        state.saved_tracks.push(saved("saved-1", "track-1"));
        state.saved_tracks.push(saved("saved-2", "track-2"));
        state.playlists.push(PlaylistEntity {
            id: "playlist-1".to_string(),
            name: "Favorites".to_string(),
            description: None,
            provider_links: BTreeMap::new(),
            provider_state: BTreeMap::new(),
            entries: vec![PlaylistEntry {
                id: "entry-1".to_string(),
                track_id: "track-1".to_string(),
                added_at: None,
                provider_state: BTreeMap::new(),
            }],
        });

        let outcome = PushOutcome {
            saved_tracks: vec![
                PushItemResult {
                    canonical_id: "saved-1".to_string(),
                    track_id: "track-1".to_string(),
                    status: SyncStatusRecord::synced(
                        Some("yt-1".to_string()),
                        Some(0.97),
                        None,
                        now,
                    ),
                    new_link: new_link("yt-1"),
                },
                PushItemResult {
                    canonical_id: "saved-2".to_string(),
                    track_id: "track-2".to_string(),
                    status: SyncStatusRecord::unmatched("No match", now),
                    new_link: None,
                },
            ],
            playlists: vec![PushPlaylistResult {
                playlist_id: "playlist-1".to_string(),
                status: SyncStatusRecord::synced(Some("yt-pl-1".to_string()), Some(1.0), None, now),
                new_link: Some(NewProviderLink {
                    provider_id: "yt-pl-1".to_string(),
                    source: LinkSource::Create,
                    confidence: Some(1.0),
                }),
                entries: vec![PushEntryResult {
                    entry_id: "entry-1".to_string(),
                    track_id: "track-1".to_string(),
                    status: SyncStatusRecord::synced(
                        Some("yt-1".to_string()),
                        Some(0.97),
                        None,
                        now,
                    ),
                    new_link: None,
                }],
            }],
            warnings: vec!["provider warning".to_string()],
        };

        let summary = apply_push_outcome(&mut state, ProviderKind::YoutubeMusic, &outcome);

        assert_eq!(summary.saved_tracks_requested, 2);
        assert_eq!(summary.saved_tracks_synced, 1);
        assert_eq!(summary.saved_tracks_unmatched, 1);
        assert_eq!(summary.playlists_processed, 1);
        assert_eq!(summary.playlist_entries_requested, 1);
        assert_eq!(summary.playlist_entries_synced, 1);
        assert_eq!(summary.playlist_entries_unmatched, 0);
        assert_eq!(summary.warnings, vec!["provider warning".to_string()]);

        let key = ProviderKind::YoutubeMusic.as_key();
        let track_1 = state
            .tracks
            .iter()
            .find(|track| track.id == "track-1")
            .unwrap();
        assert_eq!(
            track_1
                .provider_links
                .get(key)
                .map(|link| link.provider_id.as_str()),
            Some("yt-1")
        );
        assert_eq!(
            track_1.provider_state.get(key).map(|status| status.state),
            Some(SyncState::Synced)
        );
        assert_eq!(
            state.saved_tracks[0]
                .provider_state
                .get(key)
                .map(|status| status.state),
            Some(SyncState::Synced)
        );
        assert_eq!(
            state.saved_tracks[1]
                .provider_state
                .get(key)
                .map(|status| status.state),
            Some(SyncState::Unmatched)
        );
        assert_eq!(
            state.playlists[0]
                .provider_links
                .get(key)
                .map(|link| link.provider_id.as_str()),
            Some("yt-pl-1")
        );
        assert_eq!(
            state.playlists[0]
                .provider_state
                .get(key)
                .map(|status| status.state),
            Some(SyncState::Synced)
        );
        assert_eq!(
            state.playlists[0].entries[0]
                .provider_state
                .get(key)
                .map(|status| status.state),
            Some(SyncState::Synced)
        );
        state.validate().unwrap();
    }

    #[test]
    fn apply_push_outcome_rejects_link_owned_elsewhere_and_records_conflict() {
        let now = Utc::now();
        let mut state = LibraryState::new();
        state.tracks.push(track("owner", "Sirius"));
        state.tracks.push(track("pushed", "Sirius (Remaster)"));
        state.saved_tracks.push(saved("saved-owner", "owner"));
        state.saved_tracks.push(saved("saved-pushed", "pushed"));
        assert!(state.upsert_track_link(
            "owner",
            ProviderKind::Spotify,
            "shared-id",
            LinkSource::Export,
            Some(1.0),
            now,
        ));

        let outcome = PushOutcome {
            saved_tracks: vec![PushItemResult {
                canonical_id: "saved-pushed".to_string(),
                track_id: "pushed".to_string(),
                status: SyncStatusRecord::synced(
                    Some("shared-id".to_string()),
                    Some(0.95),
                    None,
                    now,
                ),
                new_link: Some(NewProviderLink {
                    provider_id: "shared-id".to_string(),
                    source: LinkSource::Match,
                    confidence: Some(0.95),
                }),
            }],
            playlists: Vec::new(),
            warnings: Vec::new(),
        };

        let summary = apply_push_outcome(&mut state, ProviderKind::Spotify, &outcome);

        assert_eq!(summary.saved_tracks_requested, 1);
        assert_eq!(summary.saved_tracks_synced, 0);
        assert_eq!(summary.saved_tracks_unmatched, 1);
        assert_eq!(summary.warnings.len(), 1);
        assert!(summary.warnings[0].contains("shared-id"));

        let key = ProviderKind::Spotify.as_key();
        let owner = state
            .tracks
            .iter()
            .find(|track| track.id == "owner")
            .unwrap();
        assert_eq!(
            owner
                .provider_links
                .get(key)
                .map(|link| link.provider_id.as_str()),
            Some("shared-id")
        );
        let pushed = state
            .tracks
            .iter()
            .find(|track| track.id == "pushed")
            .unwrap();
        assert!(!pushed.provider_links.contains_key(key));
        assert!(
            !pushed.provider_state.contains_key(key),
            "no status may claim a link that was not created"
        );
        assert_eq!(pushed.identity_conflicts.len(), 1);
        let conflict = &pushed.identity_conflicts[0];
        assert_eq!(conflict.provider, ProviderKind::Spotify);
        assert_eq!(conflict.candidate_provider_id, "shared-id");
        assert_eq!(conflict.status, IdentityConflictStatus::Open);
        assert_eq!(conflict.confidence, Some(0.95));
        state.validate().unwrap();

        // A second identical rejection does not duplicate the conflict.
        apply_push_outcome(&mut state, ProviderKind::Spotify, &outcome);
        let pushed = state
            .tracks
            .iter()
            .find(|track| track.id == "pushed")
            .unwrap();
        assert_eq!(pushed.identity_conflicts.len(), 1);
        state.validate().unwrap();
    }

    #[test]
    fn apply_push_outcome_counts_error_items_as_neither_synced_nor_unmatched() {
        let now = Utc::now();
        let mut state = LibraryState::new();
        state.tracks.push(track("track-1", "Sirius"));
        state.saved_tracks.push(saved("saved-1", "track-1"));

        let outcome = PushOutcome {
            saved_tracks: vec![PushItemResult {
                canonical_id: "saved-1".to_string(),
                track_id: "track-1".to_string(),
                status: SyncStatusRecord::error("Provider rejected the request", now),
                new_link: None,
            }],
            playlists: Vec::new(),
            warnings: Vec::new(),
        };

        let summary = apply_push_outcome(&mut state, ProviderKind::Spotify, &outcome);

        assert_eq!(summary.saved_tracks_requested, 1);
        assert_eq!(summary.saved_tracks_synced, 0);
        assert_eq!(summary.saved_tracks_unmatched, 0);
        let key = ProviderKind::Spotify.as_key();
        assert_eq!(
            state.tracks[0]
                .provider_state
                .get(key)
                .map(|status| status.state),
            Some(SyncState::Error)
        );
        assert_eq!(
            state.saved_tracks[0]
                .provider_state
                .get(key)
                .map(|status| status.state),
            Some(SyncState::Error)
        );
    }

    #[test]
    fn push_plan_is_built_from_saved_track_and_playlist_targets() {
        let now = Utc::now();
        let mut state = LibraryState::new();
        state.tracks.push(track("track-1", "Sirius"));
        state.saved_tracks.push(saved("saved-1", "track-1"));
        state.playlists.push(PlaylistEntity {
            id: "playlist-1".to_string(),
            name: "Favorites".to_string(),
            description: None,
            provider_links: BTreeMap::new(),
            provider_state: BTreeMap::new(),
            entries: vec![PlaylistEntry {
                id: "entry-1".to_string(),
                track_id: "track-1".to_string(),
                added_at: None,
                provider_state: BTreeMap::new(),
            }],
        });
        assert!(state.upsert_track_link(
            "track-1",
            ProviderKind::Spotify,
            "spotify-1",
            LinkSource::Export,
            Some(1.0),
            now,
        ));

        let plan = state.push_plan(ProviderKind::Spotify).unwrap();
        assert_eq!(plan.saved_tracks.len(), 1);
        assert_eq!(
            plan.saved_tracks[0].existing_provider_id.as_deref(),
            Some("spotify-1")
        );
        assert_eq!(plan.playlists.len(), 1);
        assert_eq!(plan.playlists[0].entries.len(), 1);
        assert_eq!(
            plan.playlists[0].entries[0].existing_provider_id.as_deref(),
            Some("spotify-1")
        );
    }
}
