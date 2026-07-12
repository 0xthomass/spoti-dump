//! Seeds a demo library database for local UI development.
//!
//! Usage:
//!   SPOTI_DUMP_DATA_DIR=/path/to/demo cargo run --example seed_demo
//!
//! The generated data covers every UI surface: multi-provider tracks, identity
//! gaps, identity conflicts, playlists with unmatched entries, and artwork.

use std::collections::BTreeMap;

use chrono::Utc;
use spoti_dump::domain::{
    new_canonical_id, LibraryState, LinkSource, PlaylistEntity, PlaylistEntry, ProviderKind,
    ProviderTrackArtwork, ProviderTrackLink, SavedTrackEntry, SyncStatusRecord, TrackEntity,
    TrackMetadata,
};

struct SeedTrack {
    title: &'static str,
    artists: &'static [&'static str],
    album: &'static str,
    duration: u32,
    spotify_id: Option<&'static str>,
    youtube_id: Option<&'static str>,
    gap_provider: Option<ProviderKind>,
    conflict_candidate: Option<&'static str>,
}

fn main() -> anyhow::Result<()> {
    let now = Utc::now();
    let mut state = LibraryState::new();

    let seeds = [
        SeedTrack {
            title: "Midnight City",
            artists: &["M83"],
            album: "Hurry Up, We're Dreaming",
            duration: 243,
            spotify_id: Some("6GyFP1nfCDB8lbD2bG0Hq9"),
            youtube_id: Some("dX3k_QDnzHE"),
            gap_provider: None,
            conflict_candidate: None,
        },
        SeedTrack {
            title: "Nightcall",
            artists: &["Kavinsky", "Lovefoxxx"],
            album: "OutRun",
            duration: 258,
            spotify_id: Some("0U0ldCRmgCqhVvD6ksG63j"),
            youtube_id: None,
            gap_provider: Some(ProviderKind::YoutubeMusic),
            conflict_candidate: None,
        },
        SeedTrack {
            title: "Instant Crush",
            artists: &["Daft Punk", "Julian Casablancas"],
            album: "Random Access Memories",
            duration: 337,
            spotify_id: Some("2cGxRwrMyEAp8dEbuZaVv6"),
            youtube_id: Some("a5uQMwRMHcs"),
            gap_provider: None,
            conflict_candidate: None,
        },
        SeedTrack {
            title: "Genesis",
            artists: &["Grimes"],
            album: "Visions",
            duration: 255,
            spotify_id: None,
            youtube_id: Some("BR3hAWzq0P4"),
            gap_provider: Some(ProviderKind::Spotify),
            conflict_candidate: None,
        },
        SeedTrack {
            title: "The Less I Know The Better",
            artists: &["Tame Impala"],
            album: "Currents",
            duration: 216,
            spotify_id: Some("6K4t31amVTZDgR3sKmwUJJ"),
            youtube_id: None,
            gap_provider: None,
            conflict_candidate: Some("sVi9zeAyF1E"),
        },
        SeedTrack {
            title: "Weird Fishes / Arpeggi",
            artists: &["Radiohead"],
            album: "In Rainbows",
            duration: 318,
            spotify_id: Some("1PS1QMdUqOal0ai3Gt7sDQ"),
            youtube_id: Some("pbEGdDOhtxA"),
            gap_provider: None,
            conflict_candidate: None,
        },
        SeedTrack {
            title: "Solar",
            artists: &["Betical"],
            album: "Solar EP",
            duration: 201,
            spotify_id: None,
            youtube_id: Some("Zq3h_LrTvOM"),
            gap_provider: Some(ProviderKind::Spotify),
            conflict_candidate: None,
        },
        SeedTrack {
            title: "Retrograde",
            artists: &["James Blake"],
            album: "Overgrown",
            duration: 223,
            spotify_id: Some("5aOSKcTz1zzsqEwYcSDkVX"),
            youtube_id: None,
            gap_provider: Some(ProviderKind::YoutubeMusic),
            conflict_candidate: Some("6p6PcFFUm5I"),
        },
    ];

    let mut track_ids = Vec::new();
    for seed in &seeds {
        let track_id = new_canonical_id("track");
        let mut provider_links = BTreeMap::new();
        let mut provider_state = BTreeMap::new();
        let mut provider_artwork = BTreeMap::new();

        if let Some(spotify_id) = seed.spotify_id {
            provider_links.insert(
                ProviderKind::Spotify.as_key().to_string(),
                ProviderTrackLink {
                    provider_id: spotify_id.to_string(),
                    source: LinkSource::Export,
                    confidence: None,
                    linked_at: now,
                    last_seen_at: Some(now),
                },
            );
            provider_state.insert(
                ProviderKind::Spotify.as_key().to_string(),
                SyncStatusRecord::synced(Some(spotify_id.to_string()), None, None, now),
            );
        }

        if let Some(youtube_id) = seed.youtube_id {
            provider_links.insert(
                ProviderKind::YoutubeMusic.as_key().to_string(),
                ProviderTrackLink {
                    provider_id: youtube_id.to_string(),
                    source: LinkSource::Match,
                    confidence: Some(0.94),
                    linked_at: now,
                    last_seen_at: Some(now),
                },
            );
            provider_artwork.insert(
                ProviderKind::YoutubeMusic.as_key().to_string(),
                ProviderTrackArtwork {
                    url: format!("https://i.ytimg.com/vi/{youtube_id}/hqdefault.jpg"),
                    width: Some(480),
                    height: Some(360),
                    last_seen_at: Some(now),
                },
            );
        }

        if let Some(gap_provider) = seed.gap_provider {
            provider_state.insert(
                gap_provider.as_key().to_string(),
                SyncStatusRecord::unmatched(
                    format!(
                        "No confident {} match found during identity resolution.",
                        gap_provider.display_name()
                    ),
                    now,
                ),
            );
        }

        if let Some(candidate) = seed.conflict_candidate {
            provider_state.insert(
                ProviderKind::YoutubeMusic.as_key().to_string(),
                SyncStatusRecord::error_with_provider_item_id(
                    format!(
                        "Skipped YouTube Music identity '{candidate}' because it would merge tracks with conflicting provider IDs."
                    ),
                    candidate,
                    Some(0.88),
                    now,
                ),
            );
        }

        state.tracks.push(TrackEntity {
            id: track_id.clone(),
            metadata: TrackMetadata {
                title: seed.title.to_string(),
                artists: seed
                    .artists
                    .iter()
                    .map(|artist| artist.to_string())
                    .collect(),
                album: Some(seed.album.to_string()),
                duration_seconds: Some(seed.duration),
                isrc: None,
            },
            provider_links,
            provider_artwork,
            provider_state,
            identity_conflicts: Vec::new(),
        });
        track_ids.push(track_id);
    }

    for (index, track_id) in track_ids.iter().enumerate() {
        let mut provider_state = BTreeMap::new();
        provider_state.insert(
            ProviderKind::Spotify.as_key().to_string(),
            SyncStatusRecord::synced(None, None, None, now),
        );
        if index % 3 == 0 {
            provider_state.insert(
                ProviderKind::YoutubeMusic.as_key().to_string(),
                SyncStatusRecord::unmatched("Not yet pushed to YouTube Music.", now),
            );
        }
        state.saved_tracks.push(SavedTrackEntry {
            id: new_canonical_id("saved-track"),
            track_id: track_id.clone(),
            added_at: Some(now.to_rfc3339()),
            provider_state,
        });
    }

    let playlists = [
        (
            "Synthwave Essentials",
            "Late-night driving music.",
            &track_ids[0..4],
        ),
        (
            "Deep Focus",
            "Instrumental and ambient favorites.",
            &track_ids[4..8],
        ),
    ];
    for (name, description, members) in playlists {
        let mut entries = Vec::new();
        for track_id in members {
            entries.push(PlaylistEntry {
                id: new_canonical_id("playlist-entry"),
                track_id: track_id.clone(),
                added_at: Some(now.to_rfc3339()),
                provider_state: BTreeMap::new(),
            });
        }
        let mut provider_state = BTreeMap::new();
        provider_state.insert(
            ProviderKind::Spotify.as_key().to_string(),
            SyncStatusRecord::synced(None, None, None, now),
        );
        state.playlists.push(PlaylistEntity {
            id: new_canonical_id("playlist"),
            name: name.to_string(),
            description: Some(description.to_string()),
            provider_links: BTreeMap::new(),
            provider_state,
            entries,
        });
    }

    // Mark one track's sync state per playlist entry as unmatched so playlist
    // detail views show mixed coverage.
    if let Some(playlist) = state.playlists.first_mut() {
        if let Some(entry) = playlist.entries.last_mut() {
            entry.provider_state.insert(
                ProviderKind::YoutubeMusic.as_key().to_string(),
                SyncStatusRecord::unmatched("No confident match on YouTube Music.", now),
            );
        }
    }

    state.validate()?;
    let path = spoti_dump::storage::write_library_state(&state)?;
    println!(
        "Seeded demo library with {} tracks, {} saved tracks, {} playlists at {}",
        state.tracks.len(),
        state.saved_tracks.len(),
        state.playlists.len(),
        path.display()
    );
    Ok(())
}
