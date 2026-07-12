use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::track::{LinkSource, TrackMetadata};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncState {
    #[default]
    Pending,
    Synced,
    Unmatched,
    Missing,
    Error,
    Skipped,
}

impl SyncState {
    pub fn as_str(self) -> &'static str {
        match self {
            SyncState::Pending => "pending",
            SyncState::Synced => "synced",
            SyncState::Unmatched => "unmatched",
            SyncState::Missing => "missing",
            SyncState::Error => "error",
            SyncState::Skipped => "skipped",
        }
    }
}

impl fmt::Display for SyncState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SyncState {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "pending" => Ok(Self::Pending),
            "synced" => Ok(Self::Synced),
            "unmatched" => Ok(Self::Unmatched),
            "missing" => Ok(Self::Missing),
            "error" => Ok(Self::Error),
            "skipped" => Ok(Self::Skipped),
            _ => anyhow::bail!("Unsupported sync state '{value}'"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncStatusRecord {
    pub state: SyncState,
    pub message: Option<String>,
    pub confidence: Option<f64>,
    pub provider_item_id: Option<String>,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_seen_at: Option<DateTime<Utc>>,
}

impl SyncStatusRecord {
    pub fn synced(
        provider_item_id: Option<String>,
        confidence: Option<f64>,
        message: Option<String>,
        at: DateTime<Utc>,
    ) -> Self {
        Self {
            state: SyncState::Synced,
            message,
            confidence,
            provider_item_id,
            last_attempt_at: Some(at),
            last_success_at: Some(at),
            last_seen_at: Some(at),
        }
    }

    pub fn unmatched(message: impl Into<String>, at: DateTime<Utc>) -> Self {
        Self {
            state: SyncState::Unmatched,
            message: Some(message.into()),
            confidence: None,
            provider_item_id: None,
            last_attempt_at: Some(at),
            last_success_at: None,
            last_seen_at: None,
        }
    }

    pub fn unmatched_with_provider_item_id(
        message: impl Into<String>,
        provider_item_id: impl Into<String>,
        confidence: Option<f64>,
        at: DateTime<Utc>,
    ) -> Self {
        Self {
            state: SyncState::Unmatched,
            message: Some(message.into()),
            confidence,
            provider_item_id: Some(provider_item_id.into()),
            last_attempt_at: Some(at),
            last_success_at: None,
            last_seen_at: None,
        }
    }

    pub fn missing(message: impl Into<String>, at: DateTime<Utc>) -> Self {
        Self {
            state: SyncState::Missing,
            message: Some(message.into()),
            confidence: None,
            provider_item_id: None,
            last_attempt_at: Some(at),
            last_success_at: None,
            last_seen_at: Some(at),
        }
    }

    pub fn error(message: impl Into<String>, at: DateTime<Utc>) -> Self {
        Self {
            state: SyncState::Error,
            message: Some(message.into()),
            confidence: None,
            provider_item_id: None,
            last_attempt_at: Some(at),
            last_success_at: None,
            last_seen_at: None,
        }
    }

    pub fn error_with_provider_item_id(
        message: impl Into<String>,
        provider_item_id: impl Into<String>,
        confidence: Option<f64>,
        at: DateTime<Utc>,
    ) -> Self {
        Self {
            state: SyncState::Error,
            message: Some(message.into()),
            confidence,
            provider_item_id: Some(provider_item_id.into()),
            last_attempt_at: Some(at),
            last_success_at: None,
            last_seen_at: None,
        }
    }

    pub fn skipped(message: impl Into<String>, at: DateTime<Utc>) -> Self {
        Self {
            state: SyncState::Skipped,
            message: Some(message.into()),
            confidence: None,
            provider_item_id: None,
            last_attempt_at: Some(at),
            last_success_at: None,
            last_seen_at: None,
        }
    }
}

impl Default for SyncStatusRecord {
    fn default() -> Self {
        Self {
            state: SyncState::Pending,
            message: None,
            confidence: None,
            provider_item_id: None,
            last_attempt_at: None,
            last_success_at: None,
            last_seen_at: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct MergeSummary {
    pub tracks_created: usize,
    pub saved_tracks_seen: usize,
    pub playlists_seen: usize,
    pub playlist_entries_seen: usize,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SyncSummary {
    pub saved_tracks_requested: usize,
    pub saved_tracks_synced: usize,
    pub saved_tracks_unmatched: usize,
    pub playlists_processed: usize,
    pub playlist_entries_requested: usize,
    pub playlist_entries_synced: usize,
    pub playlist_entries_unmatched: usize,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct PurgeReport {
    pub saved_tracks: usize,
    pub playlists: usize,
}

#[derive(Clone, Debug)]
pub struct SavedTrackSyncTarget {
    pub saved_track_id: String,
    pub track_id: String,
    pub metadata: TrackMetadata,
    pub existing_provider_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PlaylistSyncTarget {
    pub playlist_id: String,
    pub name: String,
    pub description: Option<String>,
    pub existing_provider_id: Option<String>,
    pub entries: Vec<PlaylistEntrySyncTarget>,
}

#[derive(Clone, Debug)]
pub struct PlaylistEntrySyncTarget {
    pub entry_id: String,
    pub track_id: String,
    pub added_at: Option<String>,
    pub metadata: TrackMetadata,
    pub existing_provider_id: Option<String>,
}

/// Everything a provider needs to push the canonical library, derived from
/// `LibraryState` by core so providers never see the state itself.
#[derive(Clone, Debug, Default)]
pub struct PushPlan {
    pub saved_tracks: Vec<SavedTrackSyncTarget>,
    pub playlists: Vec<PlaylistSyncTarget>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PushMode {
    DryRun,
    Apply,
}

/// Per-item results of a provider push. Core applies these to the canonical
/// state via `apply_push_outcome`, which also derives the `SyncSummary`.
#[derive(Clone, Debug, Default)]
pub struct PushOutcome {
    pub saved_tracks: Vec<PushItemResult>,
    pub playlists: Vec<PushPlaylistResult>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PushItemResult {
    /// Canonical saved-track entry ID.
    pub canonical_id: String,
    pub track_id: String,
    pub status: SyncStatusRecord,
    pub new_link: Option<NewProviderLink>,
}

#[derive(Clone, Debug)]
pub struct PushPlaylistResult {
    pub playlist_id: String,
    pub status: SyncStatusRecord,
    pub new_link: Option<NewProviderLink>,
    pub entries: Vec<PushEntryResult>,
}

#[derive(Clone, Debug)]
pub struct PushEntryResult {
    pub entry_id: String,
    pub track_id: String,
    pub status: SyncStatusRecord,
    pub new_link: Option<NewProviderLink>,
}

#[derive(Clone, Debug)]
pub struct NewProviderLink {
    pub provider_id: String,
    pub source: LinkSource,
    pub confidence: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct TrackIdentityMatch {
    pub provider_id: String,
    pub confidence: f64,
}
