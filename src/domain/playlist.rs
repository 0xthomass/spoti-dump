use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::sync::SyncStatusRecord;
use super::track::LinkSource;

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
