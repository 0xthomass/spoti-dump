use chrono::{DateTime, Utc};

use super::provider::ProviderKind;
use super::track::TrackMetadata;

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
