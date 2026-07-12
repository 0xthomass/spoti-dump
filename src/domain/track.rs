use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::provider::ProviderKind;
use super::sync::SyncStatusRecord;

pub const REJECTED_IDENTITY_CANDIDATE_MARKER: &str = "Rejected identity candidate";

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
    #[serde(default)]
    pub identity_conflicts: Vec<TrackIdentityConflict>,
}

impl TrackEntity {
    /// Records an open identity conflict for `provider` unless a conflict with
    /// the same candidate provider ID (of any status) is already present.
    /// Returns true when a new conflict was added.
    pub fn record_identity_conflict(
        &mut self,
        provider: ProviderKind,
        candidate_provider_id: impl Into<String>,
        confidence: Option<f64>,
        detected_at: DateTime<Utc>,
    ) -> bool {
        let candidate_provider_id = candidate_provider_id.into();
        if self.identity_conflicts.iter().any(|conflict| {
            conflict.provider == provider && conflict.candidate_provider_id == candidate_provider_id
        }) {
            return false;
        }
        self.identity_conflicts.push(TrackIdentityConflict {
            provider,
            candidate_provider_id,
            confidence,
            detected_at,
            status: IdentityConflictStatus::Open,
            rejected_at: None,
        });
        true
    }
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
pub struct TrackIdentityConflict {
    pub provider: ProviderKind,
    pub candidate_provider_id: String,
    pub confidence: Option<f64>,
    pub detected_at: DateTime<Utc>,
    pub status: IdentityConflictStatus,
    pub rejected_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityConflictStatus {
    Open,
    Rejected,
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
