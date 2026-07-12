use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::provider::ProviderKind;
use super::sync::SyncStatusRecord;

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
    /// Records an open identity conflict for `provider`.
    ///
    /// At most one conflict row exists per `(provider, candidate_provider_id)`.
    /// Re-detecting an already-open conflict refreshes its `detected_at` and
    /// `confidence` in place. A rejected conflict is a permanent tombstone:
    /// re-detection must NOT resurrect it, so the candidate is never
    /// re-proposed. Returns true only when a brand-new open row was added.
    pub fn record_identity_conflict(
        &mut self,
        provider: ProviderKind,
        candidate_provider_id: impl Into<String>,
        confidence: Option<f64>,
        detected_at: DateTime<Utc>,
    ) -> bool {
        let candidate_provider_id = candidate_provider_id.into();
        if let Some(existing) = self.identity_conflicts.iter_mut().find(|conflict| {
            conflict.provider == provider && conflict.candidate_provider_id == candidate_provider_id
        }) {
            if existing.status == IdentityConflictStatus::Open {
                existing.detected_at = detected_at;
                existing.confidence = confidence.or(existing.confidence);
            }
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

    /// Flips an open identity conflict for `(provider, candidate_provider_id)`
    /// into a rejected tombstone. Returns true when an open row was rejected;
    /// false when no such open conflict exists (already rejected or absent).
    pub fn reject_identity_conflict(
        &mut self,
        provider: ProviderKind,
        candidate_provider_id: &str,
        rejected_at: DateTime<Utc>,
    ) -> bool {
        let Some(conflict) = self.identity_conflicts.iter_mut().find(|conflict| {
            conflict.provider == provider
                && conflict.candidate_provider_id == candidate_provider_id
                && conflict.status == IdentityConflictStatus::Open
        }) else {
            return false;
        };
        conflict.status = IdentityConflictStatus::Rejected;
        conflict.rejected_at = Some(rejected_at);
        true
    }

    /// True when `provider`'s `candidate_provider_id` has been rejected as an
    /// identity match for this track (a tombstone that must never be
    /// re-proposed).
    pub fn has_rejected_identity_conflict(
        &self,
        provider: ProviderKind,
        candidate_provider_id: &str,
    ) -> bool {
        self.identity_conflicts.iter().any(|conflict| {
            conflict.provider == provider
                && conflict.candidate_provider_id == candidate_provider_id
                && conflict.status == IdentityConflictStatus::Rejected
        })
    }

    /// Open (unresolved) identity conflicts, in stored order.
    pub fn open_identity_conflicts(&self) -> impl Iterator<Item = &TrackIdentityConflict> {
        self.identity_conflicts
            .iter()
            .filter(|conflict| conflict.status == IdentityConflictStatus::Open)
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

impl IdentityConflictStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            IdentityConflictStatus::Open => "open",
            IdentityConflictStatus::Rejected => "rejected",
        }
    }
}

impl fmt::Display for IdentityConflictStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for IdentityConflictStatus {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "open" => Ok(Self::Open),
            "rejected" => Ok(Self::Rejected),
            _ => anyhow::bail!("Unsupported identity conflict status '{value}'"),
        }
    }
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
