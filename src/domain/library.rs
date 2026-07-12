use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::playlist::PlaylistEntity;
use super::sync::SyncStatusRecord;
use super::track::TrackEntity;

pub const LIBRARY_STATE_FORMAT_VERSION: u32 = 4;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibraryState {
    pub format_version: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub tracks: Vec<TrackEntity>,
    #[serde(default)]
    pub saved_tracks: Vec<SavedTrackEntry>,
    #[serde(default)]
    pub playlists: Vec<PlaylistEntity>,
}

impl LibraryState {
    pub fn new() -> Self {
        let now = Utc::now();
        Self {
            format_version: LIBRARY_STATE_FORMAT_VERSION,
            created_at: now,
            updated_at: now,
            tracks: Vec::new(),
            saved_tracks: Vec::new(),
            playlists: Vec::new(),
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.format_version != LIBRARY_STATE_FORMAT_VERSION {
            anyhow::bail!(
                "Unsupported library state format version {}. Expected {}.",
                self.format_version,
                LIBRARY_STATE_FORMAT_VERSION
            );
        }

        let mut track_ids = HashSet::new();
        let mut track_provider_ids = HashSet::new();
        for track in &self.tracks {
            if !track_ids.insert(track.id.as_str()) {
                anyhow::bail!("Duplicate canonical track ID '{}'.", track.id);
            }
            for (provider, link) in &track.provider_links {
                if !track_provider_ids.insert((provider.as_str(), link.provider_id.as_str())) {
                    anyhow::bail!(
                        "Duplicate track provider ID '{}:{}' in canonical state.",
                        provider,
                        link.provider_id
                    );
                }
            }
            let mut conflict_candidates = HashSet::new();
            for conflict in &track.identity_conflicts {
                if !conflict_candidates
                    .insert((conflict.provider, conflict.candidate_provider_id.as_str()))
                {
                    anyhow::bail!(
                        "Duplicate identity conflict '{}:{}' on track '{}'.",
                        conflict.provider.as_key(),
                        conflict.candidate_provider_id,
                        track.id
                    );
                }
            }
        }

        let mut saved_track_ids = HashSet::new();
        for saved_track in &self.saved_tracks {
            if !saved_track_ids.insert(saved_track.id.as_str()) {
                anyhow::bail!("Duplicate saved track ID '{}'.", saved_track.id);
            }
            if !track_ids.contains(saved_track.track_id.as_str()) {
                anyhow::bail!(
                    "Saved track '{}' references missing track '{}'.",
                    saved_track.id,
                    saved_track.track_id
                );
            }
        }

        let mut playlist_ids = HashSet::new();
        let mut playlist_provider_ids = HashSet::new();
        let mut playlist_entry_ids = HashSet::new();
        for playlist in &self.playlists {
            if !playlist_ids.insert(playlist.id.as_str()) {
                anyhow::bail!("Duplicate playlist ID '{}'.", playlist.id);
            }
            for (provider, link) in &playlist.provider_links {
                if !playlist_provider_ids.insert((provider.as_str(), link.provider_id.as_str())) {
                    anyhow::bail!(
                        "Duplicate playlist provider ID '{}:{}' in canonical state.",
                        provider,
                        link.provider_id
                    );
                }
            }
            for entry in &playlist.entries {
                if !playlist_entry_ids.insert(entry.id.as_str()) {
                    anyhow::bail!("Duplicate playlist entry ID '{}'.", entry.id);
                }
                if !track_ids.contains(entry.track_id.as_str()) {
                    anyhow::bail!(
                        "Playlist entry '{}' references missing track '{}'.",
                        entry.id,
                        entry.track_id
                    );
                }
            }
        }

        Ok(())
    }

    pub fn touch(&mut self) {
        self.updated_at = Utc::now();
    }

    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    pub fn playlist_entry_count(&self) -> usize {
        self.playlists
            .iter()
            .map(|playlist| playlist.entries.len())
            .sum()
    }
}

impl Default for LibraryState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedTrackEntry {
    pub id: String,
    pub track_id: String,
    pub added_at: Option<String>,
    #[serde(default)]
    pub provider_state: BTreeMap<String, SyncStatusRecord>,
}
