use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

pub const LIBRARY_STATE_FORMAT_VERSION: u32 = 4;
pub const REJECTED_IDENTITY_CANDIDATE_MARKER: &str = "Rejected identity candidate";

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    Serialize,
    Deserialize,
    ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
#[clap(rename_all = "kebab-case")]
pub enum ProviderKind {
    #[default]
    Spotify,
    YoutubeMusic,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderConnection {
    pub provider: ProviderKind,
    pub connected_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub config: ProviderConnectionConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderCooldown {
    pub provider: ProviderKind,
    pub blocked_until: DateTime<Utc>,
    pub reason: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderHealth {
    pub provider: ProviderKind,
    pub checked_at: DateTime<Utc>,
    pub ok: bool,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "kebab-case")]
pub enum ProviderConnectionConfig {
    Spotify(SpotifyConnectionConfig),
    YoutubeMusic(YoutubeMusicConnectionConfig),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpotifyConnectionConfig {
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct YoutubeMusicConnectionConfig {
    pub cookie: String,
    pub x_goog_authuser: String,
    pub origin: Option<String>,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 2] = [ProviderKind::Spotify, ProviderKind::YoutubeMusic];

    pub fn as_key(self) -> &'static str {
        match self {
            ProviderKind::Spotify => "spotify",
            ProviderKind::YoutubeMusic => "youtube-music",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            ProviderKind::Spotify => "Spotify",
            ProviderKind::YoutubeMusic => "YouTube Music",
        }
    }

    pub fn supports_library_reset(self) -> bool {
        matches!(self, ProviderKind::Spotify)
    }

    pub fn all() -> &'static [ProviderKind] {
        &Self::ALL
    }

    pub fn from_key(value: &str) -> anyhow::Result<Self> {
        match value {
            "spotify" => Ok(Self::Spotify),
            "youtube-music" => Ok(Self::YoutubeMusic),
            _ => anyhow::bail!("Unsupported provider key '{value}'"),
        }
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

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
pub struct TrackEntity {
    pub id: String,
    pub metadata: TrackMetadata,
    #[serde(default)]
    pub provider_links: BTreeMap<String, ProviderTrackLink>,
    #[serde(default)]
    pub provider_artwork: BTreeMap<String, ProviderTrackArtwork>,
    #[serde(default)]
    pub provider_state: BTreeMap<String, SyncStatusRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrackMetadata {
    pub title: String,
    #[serde(default)]
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub duration_seconds: Option<u32>,
    pub isrc: Option<String>,
}

impl TrackMetadata {
    pub fn artist_summary(&self) -> String {
        if self.artists.is_empty() {
            "Unknown artist".to_string()
        } else {
            self.artists.join(", ")
        }
    }

    pub fn display_label(&self) -> String {
        format!("{} - {}", self.artist_summary(), self.title)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderTrackLink {
    pub provider_id: String,
    pub source: LinkSource,
    pub confidence: Option<f64>,
    pub linked_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderTrackArtwork {
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub last_seen_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedTrackEntry {
    pub id: String,
    pub track_id: String,
    pub added_at: Option<String>,
    #[serde(default)]
    pub provider_state: BTreeMap<String, SyncStatusRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaylistEntity {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub provider_links: BTreeMap<String, ProviderPlaylistLink>,
    #[serde(default)]
    pub provider_state: BTreeMap<String, SyncStatusRecord>,
    #[serde(default)]
    pub entries: Vec<PlaylistEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderPlaylistLink {
    pub provider_id: String,
    pub source: LinkSource,
    pub confidence: Option<f64>,
    pub linked_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaylistEntry {
    pub id: String,
    pub track_id: String,
    pub added_at: Option<String>,
    #[serde(default)]
    pub provider_state: BTreeMap<String, SyncStatusRecord>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkSource {
    Export,
    Match,
    Create,
    Legacy,
    Manual,
}

impl LinkSource {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkSource::Export => "export",
            LinkSource::Match => "match",
            LinkSource::Create => "create",
            LinkSource::Legacy => "legacy",
            LinkSource::Manual => "manual",
        }
    }
}

impl fmt::Display for LinkSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LinkSource {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "export" => Ok(Self::Export),
            "match" => Ok(Self::Match),
            "create" => Ok(Self::Create),
            "legacy" => Ok(Self::Legacy),
            "manual" => Ok(Self::Manual),
            _ => anyhow::bail!("Unsupported link source '{value}'"),
        }
    }
}

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

#[derive(Clone, Debug)]
pub struct ProviderLibrarySnapshot {
    pub provider: ProviderKind,
    pub captured_at: DateTime<Utc>,
    pub saved_tracks: Vec<ObservedSavedTrack>,
    pub playlists: Vec<ObservedPlaylist>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ObservedSavedTrack {
    pub added_at: Option<String>,
    pub track: ObservedTrack,
}

#[derive(Clone, Debug)]
pub struct ObservedPlaylist {
    pub provider_id: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub tracks: Vec<ObservedPlaylistTrack>,
}

#[derive(Clone, Debug)]
pub struct ObservedPlaylistTrack {
    pub added_at: Option<String>,
    pub track: ObservedTrack,
}

#[derive(Clone, Debug)]
pub struct ObservedTrack {
    pub metadata: TrackMetadata,
    pub provider_id: Option<String>,
    pub artwork: Option<ObservedArtwork>,
}

#[derive(Clone, Debug)]
pub struct ObservedArtwork {
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
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
