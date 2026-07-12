use std::fmt;

use chrono::{DateTime, Utc};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

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
