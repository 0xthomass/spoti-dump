//! One-shot import of the retired dump formats: the pre-database `library.json`
//! snapshot and the even older per-playlist CSV exports. Moved here unchanged; a
//! separate wave decides this module's long-term fate.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use csv::{Reader, StringRecord};
use serde::Deserialize;

use crate::domain::{
    new_canonical_id, LibraryState, LinkSource, PlaylistEntity, PlaylistEntry, ProviderKind,
    ProviderPlaylistLink, ProviderTrackLink, SavedTrackEntry, TrackEntity, TrackMetadata,
    LIBRARY_STATE_FORMAT_VERSION,
};

use super::{database_path_in, legacy_library_state_path_in, DUMP_DIR};

pub(super) fn read_legacy_json_state(root: &Path) -> Result<Option<LibraryState>> {
    let path = legacy_library_state_path_in(root);
    if !path.exists() {
        return Ok(None);
    }

    let contents =
        fs::read_to_string(&path).with_context(|| format!("Failed to read {}", path.display()))?;

    if let Ok(state) = serde_json::from_str::<LibraryState>(&contents) {
        state.validate()?;
        return Ok(Some(state));
    }

    if let Ok(snapshot) = serde_json::from_str::<LegacyLibrarySnapshot>(&contents) {
        return Ok(Some(migrate_legacy_snapshot(snapshot)));
    }

    anyhow::bail!(
        "Failed to parse legacy state file {} as either current state or legacy snapshot.",
        path.display()
    );
}

pub(super) fn read_legacy_csv_state(root: &Path) -> Result<LibraryState> {
    let dump_dir = root.join(DUMP_DIR);
    let now = Utc::now();
    let mut state = LibraryState {
        format_version: LIBRARY_STATE_FORMAT_VERSION,
        created_at: now,
        updated_at: now,
        tracks: Vec::new(),
        saved_tracks: Vec::new(),
        playlists: Vec::new(),
    };

    let saved_tracks_path = dump_dir.join("saved_tracks.csv");
    if saved_tracks_path.exists() {
        let mut reader = Reader::from_path(&saved_tracks_path)
            .with_context(|| format!("Failed to read {}", saved_tracks_path.display()))?;
        for record in reader.records() {
            let record = record?;
            if let Some((metadata, provider_ids)) = parse_legacy_csv_track(&record) {
                let track_id = find_or_create_track(&mut state, metadata, provider_ids, now);
                state.saved_tracks.push(SavedTrackEntry {
                    id: new_canonical_id("saved-track"),
                    track_id,
                    added_at: record.get(0).map(ToOwned::to_owned),
                    provider_state: Default::default(),
                });
            }
        }
    }

    for entry in fs::read_dir(&dump_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file()
            || path.extension().and_then(|extension| extension.to_str()) != Some("csv")
        {
            continue;
        }
        if path.file_name().and_then(|name| name.to_str()) == Some("saved_tracks.csv") {
            continue;
        }

        let playlist_name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(ToOwned::to_owned)
            .with_context(|| format!("Invalid playlist filename {}", path.display()))?;

        let mut playlist = PlaylistEntity {
            id: new_canonical_id("playlist"),
            name: playlist_name,
            description: None,
            provider_links: Default::default(),
            provider_state: Default::default(),
            entries: Vec::new(),
        };

        let mut reader = Reader::from_path(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        for record in reader.records() {
            let record = record?;
            if let Some((metadata, provider_ids)) = parse_legacy_csv_track(&record) {
                let track_id = find_or_create_track(&mut state, metadata, provider_ids, now);
                playlist.entries.push(PlaylistEntry {
                    id: new_canonical_id("playlist-entry"),
                    track_id,
                    added_at: record.get(0).map(ToOwned::to_owned),
                    provider_state: Default::default(),
                });
            }
        }

        state.playlists.push(playlist);
    }

    if state.saved_tracks.is_empty() && state.playlists.is_empty() {
        anyhow::bail!(
            "No library data found in {}. Expected {} or legacy CSV exports.",
            dump_dir.display(),
            database_path_in(root).display()
        );
    }

    Ok(state)
}

fn parse_legacy_csv_track(
    record: &StringRecord,
) -> Option<(TrackMetadata, BTreeMap<String, String>)> {
    let title = record.get(1)?.trim().to_string();
    let artists = record
        .get(2)
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|artist| !artist.is_empty() && *artist != "Unknown")
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let album = normalize_optional_field(record.get(3));
    let spotify_id = normalize_optional_field(record.get(4));

    let mut provider_ids = BTreeMap::new();
    if let Some(spotify_id) = spotify_id {
        provider_ids.insert(ProviderKind::Spotify.as_key().to_string(), spotify_id);
    }

    Some((
        TrackMetadata {
            title: if title.is_empty() {
                "Unknown".to_string()
            } else {
                title
            },
            artists,
            album,
            duration_seconds: None,
            isrc: None,
        },
        provider_ids,
    ))
}

fn migrate_legacy_snapshot(snapshot: LegacyLibrarySnapshot) -> LibraryState {
    let created_at = snapshot.generated_at;
    let mut state = LibraryState {
        format_version: LIBRARY_STATE_FORMAT_VERSION,
        created_at,
        updated_at: Utc::now(),
        tracks: Vec::new(),
        saved_tracks: Vec::new(),
        playlists: Vec::new(),
    };

    for saved_track in snapshot.saved_tracks {
        let track_id = find_or_create_track(
            &mut state,
            TrackMetadata {
                title: saved_track.track.title,
                artists: saved_track.track.artists,
                album: saved_track.track.album,
                duration_seconds: saved_track.track.duration_seconds,
                isrc: saved_track.track.isrc,
            },
            saved_track.track.provider_ids,
            created_at,
        );
        state.saved_tracks.push(SavedTrackEntry {
            id: new_canonical_id("saved-track"),
            track_id,
            added_at: saved_track.added_at,
            provider_state: Default::default(),
        });
    }

    for playlist in snapshot.playlists {
        let mut provider_links = BTreeMap::new();
        for (provider_key, provider_id) in playlist.provider_ids {
            provider_links.insert(
                provider_key,
                ProviderPlaylistLink {
                    provider_id,
                    source: LinkSource::Legacy,
                    confidence: Some(1.0),
                    linked_at: created_at,
                    last_seen_at: Some(created_at),
                },
            );
        }

        let mut playlist_entity = PlaylistEntity {
            id: new_canonical_id("playlist"),
            name: playlist.name,
            description: playlist.description,
            provider_links,
            provider_state: Default::default(),
            entries: Vec::new(),
        };

        for entry in playlist.tracks {
            let track_id = find_or_create_track(
                &mut state,
                TrackMetadata {
                    title: entry.track.title,
                    artists: entry.track.artists,
                    album: entry.track.album,
                    duration_seconds: entry.track.duration_seconds,
                    isrc: entry.track.isrc,
                },
                entry.track.provider_ids,
                created_at,
            );
            playlist_entity.entries.push(PlaylistEntry {
                id: new_canonical_id("playlist-entry"),
                track_id,
                added_at: entry.added_at,
                provider_state: Default::default(),
            });
        }

        state.playlists.push(playlist_entity);
    }

    state
}

fn find_or_create_track(
    state: &mut LibraryState,
    metadata: TrackMetadata,
    provider_ids: BTreeMap<String, String>,
    at: DateTime<Utc>,
) -> String {
    if let Some(index) = state.tracks.iter().position(|track| {
        provider_ids.iter().any(|(provider_key, provider_id)| {
            track
                .provider_links
                .get(provider_key)
                .map(|link| link.provider_id.as_str())
                == Some(provider_id.as_str())
        })
    }) {
        merge_track_metadata(&mut state.tracks[index].metadata, &metadata);
        for (provider_key, provider_id) in provider_ids {
            state.tracks[index]
                .provider_links
                .entry(provider_key)
                .or_insert_with(|| ProviderTrackLink {
                    provider_id,
                    source: LinkSource::Legacy,
                    confidence: Some(1.0),
                    linked_at: at,
                    last_seen_at: Some(at),
                });
        }
        return state.tracks[index].id.clone();
    }

    if let Some(isrc) = metadata.isrc.as_deref() {
        if let Some(index) = state
            .tracks
            .iter()
            .position(|track| track.metadata.isrc.as_deref() == Some(isrc))
        {
            merge_track_metadata(&mut state.tracks[index].metadata, &metadata);
            return state.tracks[index].id.clone();
        }
    }

    if let Some(index) = state.tracks.iter().position(|track| {
        normalize_text(&track.metadata.title) == normalize_text(&metadata.title)
            && normalize_text(&track.metadata.artist_summary())
                == normalize_text(&metadata.artist_summary())
            && normalize_text(track.metadata.album.as_deref().unwrap_or(""))
                == normalize_text(metadata.album.as_deref().unwrap_or(""))
    }) {
        merge_track_metadata(&mut state.tracks[index].metadata, &metadata);
        return state.tracks[index].id.clone();
    }

    let track_id = new_canonical_id("track");
    let mut provider_links = BTreeMap::new();
    for (provider_key, provider_id) in provider_ids {
        provider_links.insert(
            provider_key,
            ProviderTrackLink {
                provider_id,
                source: LinkSource::Legacy,
                confidence: Some(1.0),
                linked_at: at,
                last_seen_at: Some(at),
            },
        );
    }

    state.tracks.push(TrackEntity {
        id: track_id.clone(),
        metadata,
        provider_links,
        provider_artwork: BTreeMap::new(),
        provider_state: Default::default(),
        identity_conflicts: Vec::new(),
    });
    track_id
}

fn merge_track_metadata(existing: &mut TrackMetadata, observed: &TrackMetadata) {
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

fn normalize_optional_field(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("unknown") {
        None
    } else {
        Some(value.to_string())
    }
}

fn normalize_text(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character.is_ascii_whitespace() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Deserialize)]
struct LegacyLibrarySnapshot {
    generated_at: DateTime<Utc>,
    #[allow(dead_code)]
    format_version: u32,
    #[allow(dead_code)]
    source_provider: Option<ProviderKind>,
    #[serde(default)]
    saved_tracks: Vec<LegacySavedTrackRecord>,
    #[serde(default)]
    playlists: Vec<LegacyPlaylistRecord>,
}

#[derive(Debug, Deserialize)]
struct LegacySavedTrackRecord {
    added_at: Option<String>,
    track: LegacyTrackRecord,
}

#[derive(Debug, Deserialize)]
struct LegacyPlaylistRecord {
    name: String,
    description: Option<String>,
    #[serde(default)]
    provider_ids: BTreeMap<String, String>,
    #[serde(default)]
    tracks: Vec<LegacyPlaylistTrackRecord>,
}

#[derive(Debug, Deserialize)]
struct LegacyPlaylistTrackRecord {
    added_at: Option<String>,
    track: LegacyTrackRecord,
}

#[derive(Debug, Deserialize)]
struct LegacyTrackRecord {
    title: String,
    #[serde(default)]
    artists: Vec<String>,
    album: Option<String>,
    duration_seconds: Option<u32>,
    isrc: Option<String>,
    #[serde(default)]
    provider_ids: BTreeMap<String, String>,
}
