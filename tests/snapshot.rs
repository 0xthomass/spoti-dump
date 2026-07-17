use std::collections::BTreeMap;
use std::fs;

use chrono::{Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use tempfile::tempdir;

use spoti_dump::domain::{
    merge_provider_snapshot, IdentityConflictStatus, LibraryState, LinkSource, ObservedPlaylist,
    ObservedPlaylistTrack, ObservedSavedTrack, ObservedTrack, PlaylistEntity, PlaylistEntry,
    ProviderCooldown, ProviderHealth, ProviderKind, ProviderLibrarySnapshot, ProviderTrackArtwork,
    SavedTrackEntry, SpotifyConnectionConfig, SyncState, SyncStatusRecord, TrackEntity,
    TrackIdentityConflict, TrackMergeConflictResolution, TrackMetadata,
};
use spoti_dump::storage::{
    create_manual_library_backup_in, database_health_in, database_path_in, export_csv,
    list_library_backups_in, list_provider_connections_in, list_provider_cooldowns_in,
    list_provider_healths_in, list_ui_operation_json_in, manual_backup_dir_in,
    read_library_state_in, read_provider_cooldown_in, read_provider_health_in,
    read_ui_operation_json_in, restore_library_backup_in, runtime_database_path_in,
    save_provider_cooldown_in, save_provider_health_in, save_ui_operation_json_in,
    write_library_state_in, LibraryDb,
};

#[test]
fn merge_provider_snapshot_deduplicates_tracks_into_one_canonical_entity() {
    let captured_at = Utc::now();
    let track = observed_track(
        "spotify-track-1",
        "Sirius",
        &["The Alan Parsons Project"],
        Some("Eye In The Sky"),
    );

    let snapshot = ProviderLibrarySnapshot {
        provider: ProviderKind::Spotify,
        captured_at,
        saved_tracks: vec![ObservedSavedTrack {
            added_at: Some("2024-01-01T00:00:00Z".to_string()),
            track: track.clone(),
        }],
        playlists: vec![ObservedPlaylist {
            provider_id: Some("spotify-playlist-1".to_string()),
            name: "Favorites".to_string(),
            description: Some("High-confidence matches".to_string()),
            tracks: vec![ObservedPlaylistTrack {
                added_at: Some("2024-01-01T00:00:00Z".to_string()),
                track,
            }],
        }],
        warnings: Vec::new(),
    };

    let mut state = LibraryState::new();
    let summary = merge_provider_snapshot(&mut state, snapshot);

    assert_eq!(summary.tracks_created, 1);
    assert_eq!(state.tracks.len(), 1);
    assert_eq!(state.saved_tracks.len(), 1);
    assert_eq!(state.playlists.len(), 1);
    assert_eq!(state.playlists[0].entries.len(), 1);
    assert_eq!(
        state.tracks[0]
            .provider_links
            .get(ProviderKind::Spotify.as_key())
            .map(|link| link.provider_id.as_str()),
        Some("spotify-track-1")
    );
    assert_eq!(
        state.tracks[0]
            .provider_state
            .get(ProviderKind::Spotify.as_key())
            .map(|status| status.state),
        Some(SyncState::Synced)
    );
}

#[test]
fn upsert_track_link_rejects_provider_id_owned_by_another_track() {
    let mut state = LibraryState::new();
    state
        .tracks
        .push(track_entity("track-a", "Fallout 3 Theme"));
    state
        .tracks
        .push(track_entity("track-b", "Fallout 3 Theme Alt"));
    let now = Utc::now();

    assert!(state.upsert_track_link(
        "track-a",
        ProviderKind::Spotify,
        "7rUa1vYP9ahdfHKyrtrsKj",
        LinkSource::Legacy,
        Some(1.0),
        now,
    ));
    assert!(!state.upsert_track_link(
        "track-b",
        ProviderKind::Spotify,
        "7rUa1vYP9ahdfHKyrtrsKj",
        LinkSource::Match,
        Some(0.96),
        now,
    ));

    state.validate().unwrap();
    assert_eq!(
        state.tracks[0]
            .provider_links
            .get(ProviderKind::Spotify.as_key())
            .map(|link| link.provider_id.as_str()),
        Some("7rUa1vYP9ahdfHKyrtrsKj")
    );
    assert!(!state.tracks[1]
        .provider_links
        .contains_key(ProviderKind::Spotify.as_key()));
}

#[test]
fn applying_identity_merges_duplicate_track_rows_and_saved_entries() {
    let mut state = LibraryState::new();
    state.tracks.push(track_entity("spotify-row", "Africa"));
    state.tracks.push(track_entity("youtube-row", "Africa"));
    let now = Utc::now();
    state.upsert_track_link(
        "spotify-row",
        ProviderKind::Spotify,
        "spotify-africa",
        LinkSource::Export,
        Some(1.0),
        now,
    );
    state.upsert_track_link(
        "youtube-row",
        ProviderKind::YoutubeMusic,
        "youtube-africa",
        LinkSource::Export,
        Some(1.0),
        now,
    );
    state.saved_tracks.push(SavedTrackEntry {
        id: "saved-spotify".to_string(),
        track_id: "spotify-row".to_string(),
        added_at: Some("2026-01-02T00:00:00Z".to_string()),
        provider_state: BTreeMap::new(),
    });
    state.saved_tracks.push(SavedTrackEntry {
        id: "saved-youtube".to_string(),
        track_id: "youtube-row".to_string(),
        added_at: Some("2026-01-01T00:00:00Z".to_string()),
        provider_state: BTreeMap::new(),
    });
    state.playlists.push(PlaylistEntity {
        id: "playlist-1".to_string(),
        name: "Favorites".to_string(),
        description: None,
        provider_links: BTreeMap::new(),
        provider_state: BTreeMap::new(),
        entries: vec![PlaylistEntry {
            id: "entry-1".to_string(),
            track_id: "youtube-row".to_string(),
            added_at: None,
            provider_state: BTreeMap::new(),
        }],
    });

    let result = state
        .apply_track_identity(
            "youtube-row",
            ProviderKind::Spotify,
            "spotify-africa",
            LinkSource::Match,
            Some(0.98),
            now,
        )
        .unwrap();

    assert_eq!(result.track_id(), "spotify-row");
    assert_eq!(state.tracks.len(), 1);
    assert_eq!(state.saved_tracks.len(), 1);
    assert_eq!(state.saved_tracks[0].track_id, "spotify-row");
    assert_eq!(
        state.saved_tracks[0].added_at.as_deref(),
        Some("2026-01-01T00:00:00Z")
    );
    assert_eq!(state.playlists[0].entries[0].track_id, "spotify-row");
    assert!(state.tracks[0]
        .provider_links
        .contains_key(ProviderKind::Spotify.as_key()));
    assert!(state.tracks[0]
        .provider_links
        .contains_key(ProviderKind::YoutubeMusic.as_key()));
    state.validate().unwrap();
}

#[test]
fn explicit_conflict_merge_requires_and_records_provider_link_choice() {
    let mut state = LibraryState::new();
    state.tracks.push(track_entity("spotify-row", "Drive"));
    state.tracks.push(track_entity("youtube-row", "Drive"));
    let now = Utc::now();
    state.upsert_track_link(
        "spotify-row",
        ProviderKind::Spotify,
        "spotify-candidate",
        LinkSource::Export,
        Some(1.0),
        now,
    );
    state.upsert_track_link(
        "spotify-row",
        ProviderKind::YoutubeMusic,
        "youtube-shared",
        LinkSource::Export,
        Some(1.0),
        now,
    );
    state.upsert_track_link(
        "youtube-row",
        ProviderKind::Spotify,
        "spotify-current",
        LinkSource::Export,
        Some(1.0),
        now,
    );
    state.saved_tracks.push(SavedTrackEntry {
        id: "saved-youtube".to_string(),
        track_id: "youtube-row".to_string(),
        added_at: Some("2026-01-01T00:00:00Z".to_string()),
        provider_state: BTreeMap::new(),
    });
    state.playlists.push(PlaylistEntity {
        id: "playlist-1".to_string(),
        name: "Favorites".to_string(),
        description: None,
        provider_links: BTreeMap::new(),
        provider_state: BTreeMap::new(),
        entries: vec![PlaylistEntry {
            id: "entry-1".to_string(),
            track_id: "youtube-row".to_string(),
            added_at: None,
            provider_state: BTreeMap::new(),
        }],
    });

    let failed_merge = state
        .merge_track_into("youtube-row", "spotify-row")
        .expect_err("plain merge should reject conflicting Spotify IDs");
    assert!(failed_merge.to_string().contains("conflicting IDs"));

    let result = state
        .merge_track_into_resolving_conflicts(
            "youtube-row",
            "spotify-row",
            TrackMergeConflictResolution::KeepSource,
            now,
        )
        .unwrap();

    assert_eq!(result.target_track_id, "spotify-row");
    assert_eq!(result.source_track_id, "youtube-row");
    assert_eq!(result.resolved_conflicts.len(), 1);
    assert_eq!(
        result.resolved_conflicts[0].provider_key,
        ProviderKind::Spotify.as_key()
    );
    assert_eq!(
        result.resolved_conflicts[0].kept_provider_id,
        "spotify-current"
    );
    assert_eq!(
        result.resolved_conflicts[0].dropped_provider_id,
        "spotify-candidate"
    );
    assert!(result.resolved_conflicts[0].kept_from_source);
    assert_eq!(state.tracks.len(), 1);
    assert_eq!(state.saved_tracks[0].track_id, "spotify-row");
    assert_eq!(state.playlists[0].entries[0].track_id, "spotify-row");
    let merged = &state.tracks[0];
    assert_eq!(
        merged
            .provider_links
            .get(ProviderKind::Spotify.as_key())
            .map(|link| link.provider_id.as_str()),
        Some("spotify-current")
    );
    assert_eq!(
        merged
            .provider_links
            .get(ProviderKind::YoutubeMusic.as_key())
            .map(|link| link.provider_id.as_str()),
        Some("youtube-shared")
    );
    let spotify_status = merged
        .provider_state
        .get(ProviderKind::Spotify.as_key())
        .unwrap();
    assert_eq!(spotify_status.state, SyncState::Synced);
    assert_eq!(
        spotify_status.provider_item_id.as_deref(),
        Some("spotify-current")
    );
    assert!(spotify_status
        .message
        .as_deref()
        .unwrap()
        .contains("dropped alternate ID 'spotify-candidate'"));
    state.validate().unwrap();
}

#[test]
fn merge_by_metadata_match_records_identity_conflict_instead_of_overwriting_link() {
    let mut state = LibraryState::new();
    let now = Utc::now();
    state
        .tracks
        .push(track_entity("track-a", "Fallout 3 Theme"));
    assert!(state.upsert_track_link(
        "track-a",
        ProviderKind::Spotify,
        "established-id",
        LinkSource::Export,
        Some(1.0),
        now,
    ));

    // The observed track matches by metadata similarity (identical metadata,
    // no ISRC) but carries a different Spotify ID than the established link.
    let snapshot = ProviderLibrarySnapshot {
        provider: ProviderKind::Spotify,
        captured_at: now,
        saved_tracks: vec![ObservedSavedTrack {
            added_at: None,
            track: ObservedTrack {
                metadata: state.tracks[0].metadata.clone(),
                provider_id: Some("candidate-id".to_string()),
                artwork: None,
            },
        }],
        playlists: Vec::new(),
        warnings: Vec::new(),
    };
    let summary = merge_provider_snapshot(&mut state, snapshot);

    assert_eq!(state.tracks.len(), 1);
    assert_eq!(
        state.tracks[0]
            .provider_links
            .get(ProviderKind::Spotify.as_key())
            .map(|link| link.provider_id.as_str()),
        Some("established-id")
    );
    assert_eq!(state.tracks[0].identity_conflicts.len(), 1);
    let conflict = &state.tracks[0].identity_conflicts[0];
    assert_eq!(conflict.provider, ProviderKind::Spotify);
    assert_eq!(conflict.candidate_provider_id, "candidate-id");
    assert_eq!(conflict.status, IdentityConflictStatus::Open);
    assert!(summary
        .warnings
        .iter()
        .any(|warning| warning.contains("candidate-id")));
    // No synced status may claim the rejected candidate ID.
    assert_ne!(
        state.tracks[0]
            .provider_state
            .get(ProviderKind::Spotify.as_key())
            .and_then(|status| status.provider_item_id.as_deref()),
        Some("candidate-id")
    );
    assert_eq!(state.saved_tracks.len(), 1);
    state.validate().unwrap();
}

#[test]
fn validation_rejects_duplicate_identity_conflicts_on_a_track() {
    let mut state = LibraryState::new();
    let now = Utc::now();
    let mut track = track_entity("track-a", "Fallout 3 Theme");
    assert!(track.record_identity_conflict(ProviderKind::Spotify, "candidate-id", None, now));
    assert!(!track.record_identity_conflict(ProviderKind::Spotify, "candidate-id", None, now));
    track
        .identity_conflicts
        .push(track.identity_conflicts[0].clone());
    state.tracks.push(track);

    let error = state.validate().unwrap_err().to_string();
    assert!(error.contains("Duplicate identity conflict"));
}

#[test]
fn validation_rejects_open_conflict_matching_own_link() {
    let mut state = LibraryState::new();
    let now = Utc::now();
    let mut track = track_entity("track-a", "Fallout 3 Theme");
    // A track cannot have an OPEN conflict for a candidate that equals its own
    // established link for that provider.
    assert!(track.record_identity_conflict(ProviderKind::Spotify, "spotify-self", None, now));
    state.tracks.push(track);
    assert!(state.upsert_track_link(
        "track-a",
        ProviderKind::Spotify,
        "spotify-self",
        LinkSource::Export,
        Some(1.0),
        now,
    ));

    let error = state.validate().unwrap_err().to_string();
    assert!(error.contains("equals its own linked ID"));

    // Rejecting the conflict turns it into a tombstone, which is allowed to
    // coexist with the link.
    assert!(state.reject_track_identity_conflict(
        "track-a",
        ProviderKind::Spotify,
        "spotify-self",
        now,
    ));
    state.validate().unwrap();
}

#[test]
fn identity_conflict_lifecycle_survives_save_load_and_rejection_is_permanent() {
    let temp = tempdir().unwrap();
    let now = Utc::now();
    let mut state = LibraryState::new();
    state.tracks.push(track_entity("src", "Disputed"));
    state.tracks.push(track_entity("own", "Disputed"));
    assert!(state.upsert_track_link(
        "own",
        ProviderKind::YoutubeMusic,
        "youtube-owner",
        LinkSource::Export,
        Some(1.0),
        now,
    ));
    // Record an open conflict on the source track and persist it.
    assert!(state.record_track_identity_conflict(
        "src",
        ProviderKind::YoutubeMusic,
        "youtube-owner",
        Some(0.95),
        now,
    ));
    write_library_state_in(temp.path(), &state).unwrap();

    let loaded = read_library_state_in(temp.path(), false).unwrap();
    let src = loaded
        .tracks
        .iter()
        .find(|track| track.id == "src")
        .unwrap();
    assert_eq!(src.identity_conflicts.len(), 1);
    assert_eq!(
        src.identity_conflicts[0].status,
        IdentityConflictStatus::Open
    );
    assert_eq!(src.identity_conflicts[0].confidence, Some(0.95));

    // Reject the conflict and persist the tombstone.
    let mut loaded = loaded;
    assert!(loaded.reject_track_identity_conflict(
        "src",
        ProviderKind::YoutubeMusic,
        "youtube-owner",
        now,
    ));
    write_library_state_in(temp.path(), &loaded).unwrap();

    let mut reloaded = read_library_state_in(temp.path(), false).unwrap();
    let src = reloaded
        .tracks
        .iter()
        .find(|track| track.id == "src")
        .unwrap();
    assert_eq!(src.identity_conflicts.len(), 1);
    assert_eq!(
        src.identity_conflicts[0].status,
        IdentityConflictStatus::Rejected
    );
    assert!(src.identity_conflicts[0].rejected_at.is_some());

    // Re-detecting the same candidate must not resurrect the tombstone.
    assert!(!reloaded.record_track_identity_conflict(
        "src",
        ProviderKind::YoutubeMusic,
        "youtube-owner",
        Some(0.99),
        now,
    ));
    let src = reloaded
        .tracks
        .iter()
        .find(|track| track.id == "src")
        .unwrap();
    assert_eq!(src.identity_conflicts.len(), 1);
    assert_eq!(
        src.identity_conflicts[0].status,
        IdentityConflictStatus::Rejected
    );
}

#[test]
fn merge_provider_snapshot_keeps_saved_tracks_append_only_when_unseen_later() {
    let captured_at = Utc::now();
    let initial_snapshot = ProviderLibrarySnapshot {
        provider: ProviderKind::Spotify,
        captured_at,
        saved_tracks: vec![ObservedSavedTrack {
            added_at: None,
            track: observed_track(
                "spotify-track-1",
                "Sirius",
                &["The Alan Parsons Project"],
                Some("Eye In The Sky"),
            ),
        }],
        playlists: Vec::new(),
        warnings: Vec::new(),
    };

    let mut state = LibraryState::new();
    merge_provider_snapshot(&mut state, initial_snapshot);

    let empty_snapshot = ProviderLibrarySnapshot {
        provider: ProviderKind::Spotify,
        captured_at: Utc::now(),
        saved_tracks: Vec::new(),
        playlists: Vec::new(),
        warnings: Vec::new(),
    };
    merge_provider_snapshot(&mut state, empty_snapshot);

    assert_eq!(state.saved_tracks.len(), 1);
    assert_eq!(
        state.saved_tracks[0]
            .provider_state
            .get(ProviderKind::Spotify.as_key())
            .map(|status| status.state),
        Some(SyncState::Synced)
    );
}

#[test]
fn merge_provider_snapshot_keeps_playlist_entries_append_only_when_unseen_later() {
    let captured_at = Utc::now();
    let initial_snapshot = ProviderLibrarySnapshot {
        provider: ProviderKind::Spotify,
        captured_at,
        saved_tracks: Vec::new(),
        playlists: vec![ObservedPlaylist {
            provider_id: Some("spotify-playlist-1".to_string()),
            name: "Favorites".to_string(),
            description: None,
            tracks: vec![ObservedPlaylistTrack {
                added_at: Some("2024-01-01T00:00:00Z".to_string()),
                track: observed_track(
                    "spotify-track-1",
                    "Sirius",
                    &["The Alan Parsons Project"],
                    Some("Eye In The Sky"),
                ),
            }],
        }],
        warnings: Vec::new(),
    };

    let mut state = LibraryState::new();
    merge_provider_snapshot(&mut state, initial_snapshot);

    let empty_playlist_export = ProviderLibrarySnapshot {
        provider: ProviderKind::Spotify,
        captured_at: Utc::now(),
        saved_tracks: Vec::new(),
        playlists: vec![ObservedPlaylist {
            provider_id: Some("spotify-playlist-1".to_string()),
            name: "Favorites".to_string(),
            description: None,
            tracks: Vec::new(),
        }],
        warnings: Vec::new(),
    };
    merge_provider_snapshot(&mut state, empty_playlist_export);

    assert_eq!(state.playlists.len(), 1);
    assert_eq!(state.playlists[0].entries.len(), 1);
    assert_eq!(
        state.playlists[0].entries[0]
            .provider_state
            .get(ProviderKind::Spotify.as_key())
            .map(|status| status.state),
        Some(SyncState::Synced)
    );
}

#[test]
fn library_state_round_trips_with_unmatched_statuses() {
    let temp = tempdir().unwrap();
    let now = Utc::now();

    let mut provider_state = BTreeMap::new();
    provider_state.insert(
        ProviderKind::YoutubeMusic.as_key().to_string(),
        SyncStatusRecord::unmatched("No YouTube Music match for Sirius", now),
    );
    let mut provider_artwork = BTreeMap::new();
    provider_artwork.insert(
        ProviderKind::Spotify.as_key().to_string(),
        ProviderTrackArtwork {
            url: "https://i.scdn.co/image/abc123".to_string(),
            width: Some(640),
            height: Some(640),
            last_seen_at: Some(now),
        },
    );

    let state = LibraryState {
        tracks: vec![TrackEntity {
            id: "track_1".to_string(),
            metadata: TrackMetadata {
                title: "Sirius".to_string(),
                artists: vec!["The Alan Parsons Project".to_string()],
                album: Some("Eye In The Sky".to_string()),
                duration_seconds: Some(401),
                isrc: Some("GBF077930020".to_string()),
            },
            provider_links: BTreeMap::new(),
            provider_artwork,
            identity_conflicts: Vec::new(),
            provider_state: BTreeMap::new(),
        }],
        saved_tracks: vec![SavedTrackEntry {
            id: "saved_track_1".to_string(),
            track_id: "track_1".to_string(),
            added_at: Some("2024-01-01T00:00:00Z".to_string()),
            provider_state,
        }],
        playlists: Vec::new(),
        ..LibraryState::new()
    };

    write_library_state_in(temp.path(), &state).unwrap();
    let reloaded = read_library_state_in(temp.path(), false).unwrap();

    assert_eq!(reloaded.tracks.len(), 1);
    assert_eq!(reloaded.saved_tracks.len(), 1);
    assert_eq!(
        reloaded.saved_tracks[0]
            .provider_state
            .get(ProviderKind::YoutubeMusic.as_key())
            .map(|status| status.state),
        Some(SyncState::Unmatched)
    );
    assert_eq!(
        reloaded.saved_tracks[0]
            .provider_state
            .get(ProviderKind::YoutubeMusic.as_key())
            .and_then(|status| status.message.as_deref()),
        Some("No YouTube Music match for Sirius")
    );
    assert_eq!(
        reloaded.tracks[0]
            .provider_artwork
            .get(ProviderKind::Spotify.as_key())
            .map(|artwork| artwork.url.as_str()),
        Some("https://i.scdn.co/image/abc123")
    );
}

#[test]
fn csv_export_dumps_normalized_database_tables() {
    let temp = tempdir().unwrap();

    let mut track_provider_state = BTreeMap::new();
    track_provider_state.insert(
        ProviderKind::Spotify.as_key().to_string(),
        SyncStatusRecord::synced(
            Some("spotify-track-1".to_string()),
            Some(1.0),
            Some("Observed in provider export".to_string()),
            Utc::now(),
        ),
    );

    let state = LibraryState {
        tracks: vec![TrackEntity {
            id: "track_1".to_string(),
            metadata: TrackMetadata {
                title: "Sirius".to_string(),
                artists: vec!["The Alan Parsons Project".to_string()],
                album: Some("Eye In The Sky".to_string()),
                duration_seconds: Some(401),
                isrc: None,
            },
            provider_links: BTreeMap::new(),
            provider_artwork: BTreeMap::new(),
            identity_conflicts: vec![TrackIdentityConflict {
                provider: ProviderKind::YoutubeMusic,
                candidate_provider_id: "youtube-candidate".to_string(),
                confidence: Some(0.9),
                detected_at: Utc::now(),
                status: IdentityConflictStatus::Open,
                rejected_at: None,
            }],
            provider_state: track_provider_state,
        }],
        saved_tracks: vec![SavedTrackEntry {
            id: "saved_track_1".to_string(),
            track_id: "track_1".to_string(),
            added_at: None,
            provider_state: BTreeMap::new(),
        }],
        playlists: Vec::new(),
        ..LibraryState::new()
    };

    write_library_state_in(temp.path(), &state).unwrap();
    let export_dir = export_csv(temp.path(), &state, None).unwrap();

    assert!(export_dir.join("tracks.csv").exists());
    assert!(export_dir.join("track_provider_artwork.csv").exists());
    assert!(export_dir.join("track_provider_status.csv").exists());
    assert!(export_dir.join("track_identity_conflicts.csv").exists());
    assert!(export_dir.join("saved_tracks.csv").exists());

    let tracks_csv = fs::read_to_string(export_dir.join("tracks.csv")).unwrap();
    let track_status_csv =
        fs::read_to_string(export_dir.join("track_provider_status.csv")).unwrap();
    let conflicts_csv =
        fs::read_to_string(export_dir.join("track_identity_conflicts.csv")).unwrap();

    assert!(tracks_csv.contains("track_1"));
    assert!(track_status_csv.contains("spotify"));
    assert!(track_status_csv.contains("synced"));
    assert!(conflicts_csv.contains("track_1"));
    assert!(conflicts_csv.contains("youtube-candidate"));
    assert!(conflicts_csv.contains("open"));
}

#[test]
fn legacy_csv_dump_is_migrated_into_canonical_state() {
    let temp = tempdir().unwrap();
    let dump_dir = temp.path().join("dump");
    fs::create_dir_all(&dump_dir).unwrap();

    fs::write(
        dump_dir.join("saved_tracks.csv"),
        "added_at,title,artists,album,spotify_id\n2024-01-01T00:00:00Z,Sirius,The Alan Parsons Project,Eye In The Sky,spotify-track-1\n",
    )
    .unwrap();
    fs::write(
        dump_dir.join("Road Trip.csv"),
        "added_at,title,artists,album,spotify_id\n2024-01-02T00:00:00Z,Sirius,The Alan Parsons Project,Eye In The Sky,spotify-track-1\n",
    )
    .unwrap();

    let state = read_library_state_in(temp.path(), false).unwrap();

    assert_eq!(state.tracks.len(), 1);
    assert_eq!(state.saved_tracks.len(), 1);
    assert_eq!(state.playlists.len(), 1);
    assert_eq!(state.playlists[0].entries.len(), 1);
    assert_eq!(
        state.tracks[0]
            .provider_links
            .get(ProviderKind::Spotify.as_key())
            .map(|link| link.provider_id.as_str()),
        Some("spotify-track-1")
    );
}

#[test]
fn removing_saved_track_prunes_orphan_track() {
    let mut state = LibraryState {
        tracks: vec![TrackEntity {
            id: "track_1".to_string(),
            metadata: TrackMetadata {
                title: "Sirius".to_string(),
                artists: vec!["The Alan Parsons Project".to_string()],
                album: Some("Eye In The Sky".to_string()),
                duration_seconds: None,
                isrc: None,
            },
            provider_links: BTreeMap::new(),
            provider_artwork: BTreeMap::new(),
            identity_conflicts: Vec::new(),
            provider_state: BTreeMap::new(),
        }],
        saved_tracks: vec![SavedTrackEntry {
            id: "saved_track_1".to_string(),
            track_id: "track_1".to_string(),
            added_at: None,
            provider_state: BTreeMap::new(),
        }],
        playlists: Vec::new(),
        ..LibraryState::new()
    };

    assert!(state.remove_saved_track("saved_track_1"));
    assert!(state.saved_tracks.is_empty());
    assert!(state.tracks.is_empty());
}

#[test]
fn removing_playlist_entry_keeps_track_when_still_saved_elsewhere() {
    let mut state = LibraryState {
        tracks: vec![TrackEntity {
            id: "track_1".to_string(),
            metadata: TrackMetadata {
                title: "Sirius".to_string(),
                artists: vec!["The Alan Parsons Project".to_string()],
                album: Some("Eye In The Sky".to_string()),
                duration_seconds: None,
                isrc: None,
            },
            provider_links: BTreeMap::new(),
            provider_artwork: BTreeMap::new(),
            identity_conflicts: Vec::new(),
            provider_state: BTreeMap::new(),
        }],
        saved_tracks: vec![SavedTrackEntry {
            id: "saved_track_1".to_string(),
            track_id: "track_1".to_string(),
            added_at: None,
            provider_state: BTreeMap::new(),
        }],
        playlists: vec![PlaylistEntity {
            id: "playlist_1".to_string(),
            name: "Favorites".to_string(),
            description: None,
            provider_links: BTreeMap::new(),
            provider_state: BTreeMap::new(),
            entries: vec![PlaylistEntry {
                id: "entry_1".to_string(),
                track_id: "track_1".to_string(),
                added_at: None,
                provider_state: BTreeMap::new(),
            }],
        }],
        ..LibraryState::new()
    };

    assert!(state.remove_playlist_entry("playlist_1", "entry_1"));
    assert!(state.playlists[0].entries.is_empty());
    assert_eq!(state.tracks.len(), 1);
}

#[test]
fn removing_track_everywhere_clears_saved_and_playlist_references() {
    let mut state = LibraryState {
        tracks: vec![
            TrackEntity {
                id: "track_1".to_string(),
                metadata: TrackMetadata {
                    title: "Sirius".to_string(),
                    artists: vec!["The Alan Parsons Project".to_string()],
                    album: Some("Eye In The Sky".to_string()),
                    duration_seconds: None,
                    isrc: None,
                },
                provider_links: BTreeMap::new(),
                provider_artwork: BTreeMap::new(),
                identity_conflicts: Vec::new(),
                provider_state: BTreeMap::new(),
            },
            TrackEntity {
                id: "track_2".to_string(),
                metadata: TrackMetadata {
                    title: "Mammagamma".to_string(),
                    artists: vec!["The Alan Parsons Project".to_string()],
                    album: Some("Eye In The Sky".to_string()),
                    duration_seconds: None,
                    isrc: None,
                },
                provider_links: BTreeMap::new(),
                provider_artwork: BTreeMap::new(),
                identity_conflicts: Vec::new(),
                provider_state: BTreeMap::new(),
            },
        ],
        saved_tracks: vec![SavedTrackEntry {
            id: "saved_track_1".to_string(),
            track_id: "track_1".to_string(),
            added_at: None,
            provider_state: BTreeMap::new(),
        }],
        playlists: vec![PlaylistEntity {
            id: "playlist_1".to_string(),
            name: "Favorites".to_string(),
            description: None,
            provider_links: BTreeMap::new(),
            provider_state: BTreeMap::new(),
            entries: vec![
                PlaylistEntry {
                    id: "entry_1".to_string(),
                    track_id: "track_1".to_string(),
                    added_at: None,
                    provider_state: BTreeMap::new(),
                },
                PlaylistEntry {
                    id: "entry_2".to_string(),
                    track_id: "track_2".to_string(),
                    added_at: None,
                    provider_state: BTreeMap::new(),
                },
            ],
        }],
        ..LibraryState::new()
    };

    assert!(state.remove_track_everywhere("track_1"));
    assert_eq!(state.saved_tracks.len(), 0);
    assert_eq!(state.playlists[0].entries.len(), 1);
    assert_eq!(state.playlists[0].entries[0].track_id, "track_2");
    assert_eq!(state.tracks.len(), 1);
    assert_eq!(state.tracks[0].id, "track_2");
}

#[test]
fn writing_existing_database_creates_point_in_time_backup() {
    let temp = tempdir().unwrap();
    let initial = LibraryState::new();
    write_library_state_in(temp.path(), &initial).unwrap();

    let updated = LibraryState::new();
    write_library_state_in(temp.path(), &updated).unwrap();

    let backup_dir = temp.path().join("dump").join("backups");
    let backups = fs::read_dir(backup_dir)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(backups.len(), 1);
    assert!(backups[0].path().metadata().unwrap().len() > 0);
}

fn count_automatic_backups(root: &std::path::Path) -> usize {
    let backup_dir = root.join("dump").join("backups");
    fs::read_dir(&backup_dir)
        .map(|entries| entries.count())
        .unwrap_or(0)
}

#[test]
fn library_handle_runs_in_wal_and_skips_unchanged_saves() {
    let temp = tempdir().unwrap();
    // A single handle is opened once and reused across saves; the process-wide
    // path shares one such handle for the whole run.
    let handle = LibraryDb::open(temp.path()).unwrap();

    handle.save(&LibraryState::new()).unwrap();

    // The connection opened the database in WAL journal mode, and the setting is
    // persisted so a fresh connection also reports it.
    let journal_mode: String = Connection::open(database_path_in(temp.path()))
        .unwrap()
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode.to_lowercase(), "wal");

    // Persist real content, then repeat the identical save on the same handle.
    let mut populated = LibraryState::new();
    populated.tracks.push(track_entity("track-1", "One"));
    handle.save(&populated).unwrap();
    let after_change = count_automatic_backups(temp.path());
    assert_eq!(after_change, 1);

    // The second, byte-identical save reuses the handle's recorded fingerprint
    // and must skip both the write and the pre-write snapshot.
    handle.save(&populated).unwrap();
    assert_eq!(count_automatic_backups(temp.path()), after_change);
}

#[test]
fn saves_are_change_guarded() {
    let temp = tempdir().unwrap();
    let handle = LibraryDb::open(temp.path()).unwrap();

    let mut state = LibraryState::new();
    state.tracks.push(track_entity("track-1", "One"));

    // First write creates the database, so there is nothing to snapshot yet.
    handle.save(&state).unwrap();
    assert_eq!(count_automatic_backups(temp.path()), 0);

    // Saving identical content is a no-op: still no backup.
    handle.save(&state).unwrap();
    assert_eq!(count_automatic_backups(temp.path()), 0);

    // Mutating the state makes the next save fire a pre-write snapshot.
    state.tracks.push(track_entity("track-2", "Two"));
    handle.save(&state).unwrap();
    assert_eq!(count_automatic_backups(temp.path()), 1);

    // A subsequent identical save is skipped again; the backup count holds.
    handle.save(&state).unwrap();
    assert_eq!(count_automatic_backups(temp.path()), 1);

    // The mutation is durable despite the skipped saves in between.
    let reloaded = read_library_state_in(temp.path(), false).unwrap();
    assert_eq!(reloaded.tracks.len(), 2);
}

#[test]
fn validation_rejects_saved_track_with_missing_canonical_track() {
    let state = LibraryState {
        saved_tracks: vec![SavedTrackEntry {
            id: "saved_track_1".to_string(),
            track_id: "missing_track".to_string(),
            added_at: None,
            provider_state: BTreeMap::new(),
        }],
        ..LibraryState::new()
    };

    let error = state.validate().unwrap_err().to_string();
    assert!(error.contains("missing track 'missing_track'"));
}

#[test]
fn automatic_database_backups_are_bounded() {
    let temp = tempdir().unwrap();
    write_library_state_in(temp.path(), &LibraryState::new()).unwrap();

    let manual_backup = create_manual_library_backup_in(temp.path()).unwrap();
    assert!(manual_backup.path.exists());
    assert_eq!(manual_backup.backup_type, "manual");

    for _ in 0..55 {
        write_library_state_in(temp.path(), &LibraryState::new()).unwrap();
    }

    let backup_count = fs::read_dir(temp.path().join("dump").join("backups"))
        .unwrap()
        .count();
    assert_eq!(backup_count, 50);
    assert_eq!(
        fs::read_dir(manual_backup_dir_in(temp.path()))
            .unwrap()
            .count(),
        1
    );

    let backups = list_library_backups_in(temp.path()).unwrap();
    assert_eq!(
        backups
            .iter()
            .filter(|backup| backup.backup_type == "manual")
            .count(),
        1
    );
    assert!(backups
        .iter()
        .any(|backup| backup.file_name == manual_backup.file_name));
}

#[test]
fn restoring_backup_validates_and_preserves_pre_restore_snapshot() {
    let temp = tempdir().unwrap();
    let mut original = LibraryState::new();
    original
        .tracks
        .push(track_entity("original-track", "Original"));
    write_library_state_in(temp.path(), &original).unwrap();
    // The backup is an older, v4-shaped database (no typed conflict table): the
    // restore must migrate it up to the current schema before loading it.
    let backup = create_manual_library_backup_in(temp.path()).unwrap();
    downgrade_database_to_v4(&backup.path);

    let mut changed = LibraryState::new();
    changed
        .tracks
        .push(track_entity("changed-track", "Changed"));
    write_library_state_in(temp.path(), &changed).unwrap();

    let summary = restore_library_backup_in(temp.path(), "manual", &backup.file_name).unwrap();
    assert_eq!(summary.restored_backup.file_name, backup.file_name);
    assert_eq!(summary.pre_restore_backup.backup_type, "manual");
    assert!(summary.pre_restore_backup.path.exists());

    let restored = read_library_state_in(temp.path(), false).unwrap();
    assert_eq!(restored.tracks.len(), 1);
    assert_eq!(restored.tracks[0].metadata.title, "Original");
    assert_eq!(restored.format_version, 5);
    // The restored (live) database carries the migrated schema.
    let restored_database = Connection::open(database_path_in(temp.path())).unwrap();
    let conflicts_table_exists: i64 = restored_database
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'track_identity_conflicts'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(conflicts_table_exists, 1);
    assert!(fs::read_dir(temp.path().join("dump"))
        .unwrap()
        .filter_map(|entry| entry.ok())
        .all(|entry| !entry
            .file_name()
            .to_string_lossy()
            .starts_with(".restore-staging-")));
    assert!(restore_library_backup_in(temp.path(), "manual", "../library.db").is_err());
}

#[test]
fn ui_operation_history_is_durable_and_database_health_is_reported() {
    let temp = tempdir().unwrap();
    write_library_state_in(temp.path(), &LibraryState::new()).unwrap();

    save_ui_operation_json_in(
        temp.path(),
        "operation-1",
        "running",
        r#"{"id":"operation-1"}"#,
    )
    .unwrap();

    assert_eq!(
        read_ui_operation_json_in(temp.path(), "operation-1")
            .unwrap()
            .as_deref(),
        Some(r#"{"id":"operation-1"}"#)
    );
    assert_eq!(list_ui_operation_json_in(temp.path()).unwrap().len(), 1);
    assert!(runtime_database_path_in(temp.path()).exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            fs::metadata(runtime_database_path_in(temp.path()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let library = Connection::open(database_path_in(temp.path())).unwrap();
    let legacy_operation_table = library
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ui_operations'",
            [],
            |_| Ok(()),
        )
        .optional()
        .unwrap();
    assert!(legacy_operation_table.is_none());

    let health = database_health_in(temp.path()).unwrap();
    assert_eq!(health.integrity_check, "ok");
    assert_eq!(health.tracks, 0);
}

#[test]
fn provider_cooldowns_are_runtime_only_and_expire() {
    let temp = tempdir().unwrap();
    write_library_state_in(temp.path(), &LibraryState::new()).unwrap();

    let now = Utc::now();
    save_provider_cooldown_in(
        temp.path(),
        &ProviderCooldown {
            provider: ProviderKind::Spotify,
            blocked_until: now + Duration::minutes(15),
            reason: "Spotify returned 429 Too Many Requests.".to_string(),
            updated_at: now,
        },
    )
    .unwrap();

    let active = read_provider_cooldown_in(temp.path(), ProviderKind::Spotify)
        .unwrap()
        .unwrap();
    assert_eq!(active.provider, ProviderKind::Spotify);
    assert!(active.reason.contains("429"));
    assert_eq!(list_provider_cooldowns_in(temp.path()).unwrap().len(), 1);
    save_provider_health_in(
        temp.path(),
        &ProviderHealth {
            provider: ProviderKind::YoutubeMusic,
            checked_at: now,
            ok: false,
            message: Some("Expired browser headers.".to_string()),
        },
    )
    .unwrap();
    let health = read_provider_health_in(temp.path(), ProviderKind::YoutubeMusic)
        .unwrap()
        .unwrap();
    assert_eq!(health.provider, ProviderKind::YoutubeMusic);
    assert!(!health.ok);
    assert_eq!(list_provider_healths_in(temp.path()).unwrap().len(), 1);

    let library = Connection::open(database_path_in(temp.path())).unwrap();
    let library_cooldown_table = library
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'provider_cooldowns'",
            [],
            |_| Ok(()),
        )
        .optional()
        .unwrap();
    assert!(library_cooldown_table.is_none());
    let library_health_table = library
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'provider_health'",
            [],
            |_| Ok(()),
        )
        .optional()
        .unwrap();
    assert!(library_health_table.is_none());

    save_provider_cooldown_in(
        temp.path(),
        &ProviderCooldown {
            provider: ProviderKind::Spotify,
            blocked_until: now - Duration::minutes(1),
            reason: "expired".to_string(),
            updated_at: now,
        },
    )
    .unwrap();
    assert!(
        read_provider_cooldown_in(temp.path(), ProviderKind::Spotify)
            .unwrap()
            .is_none()
    );
}

/// Rewrites a freshly written v5 database into a v4-shaped one: drops the typed
/// conflict table and rolls the stored schema version back to 4. Callers then
/// insert legacy status rows to exercise the migration.
fn downgrade_database_to_v4(database_path: &std::path::Path) {
    let database = Connection::open(database_path).unwrap();
    database
        .execute("DROP TABLE track_identity_conflicts", [])
        .unwrap();
    database
        .execute(
            "UPDATE library_metadata SET value = '4' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
    drop(database);
}

fn insert_legacy_track_status(
    database: &Connection,
    track_id: &str,
    provider: ProviderKind,
    state: &str,
    message: &str,
    provider_item_id: Option<&str>,
) {
    database
        .execute(
            "INSERT INTO track_provider_status
                (track_id, provider, state, message, confidence, provider_item_id, last_attempt_at, last_success_at, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL)",
            params![
                track_id,
                provider.as_key(),
                state,
                message,
                0.9_f64,
                provider_item_id,
                Utc::now().to_rfc3339(),
            ],
        )
        .unwrap();
}

#[test]
fn migrating_v4_database_converts_legacy_conflicts_and_snapshots() {
    let temp = tempdir().unwrap();
    let mut state = LibraryState::new();
    state
        .tracks
        .push(track_entity("track-open", "Open Conflict"));
    state
        .tracks
        .push(track_entity("track-rejected", "Rejected Conflict"));
    state
        .tracks
        .push(track_entity("track-unparseable", "Unparseable Conflict"));
    state
        .tracks
        .push(track_entity("track-normal", "Normal Error"));
    write_library_state_in(temp.path(), &state).unwrap();

    let database_path = database_path_in(temp.path());
    downgrade_database_to_v4(&database_path);

    let database = Connection::open(&database_path).unwrap();
    // Legacy open conflict: an error row whose candidate id is in the
    // provider_item_id column.
    insert_legacy_track_status(
        &database,
        "track-open",
        ProviderKind::YoutubeMusic,
        "error",
        "Skipped YouTube Music identity 'youtube-open' because it would merge tracks with conflicting provider IDs.",
        Some("youtube-open"),
    );
    // Legacy rejected tombstone with the retired marker; candidate parsed from
    // the message (no provider_item_id column value).
    insert_legacy_track_status(
        &database,
        "track-rejected",
        ProviderKind::YoutubeMusic,
        "unmatched",
        "Rejected identity candidate: rejected YouTube Music identity 'youtube-rejected' for a track.",
        None,
    );
    // Matches a conflict pattern but the candidate id cannot be recovered: must
    // be dropped, not converted.
    insert_legacy_track_status(
        &database,
        "track-unparseable",
        ProviderKind::YoutubeMusic,
        "error",
        "Merge aborted because of conflicting provider IDs.",
        None,
    );
    // A genuine, non-conflict error must never become a conflict.
    insert_legacy_track_status(
        &database,
        "track-normal",
        ProviderKind::Spotify,
        "error",
        "Provider rejected the request.",
        None,
    );
    drop(database);

    let migrated = read_library_state_in(temp.path(), false).unwrap();
    assert_eq!(migrated.format_version, 5);

    let open = migrated
        .tracks
        .iter()
        .find(|track| track.id == "track-open")
        .unwrap();
    assert_eq!(open.identity_conflicts.len(), 1);
    assert_eq!(
        open.identity_conflicts[0].provider,
        ProviderKind::YoutubeMusic
    );
    assert_eq!(
        open.identity_conflicts[0].candidate_provider_id,
        "youtube-open"
    );
    assert_eq!(
        open.identity_conflicts[0].status,
        IdentityConflictStatus::Open
    );

    let rejected = migrated
        .tracks
        .iter()
        .find(|track| track.id == "track-rejected")
        .unwrap();
    assert_eq!(rejected.identity_conflicts.len(), 1);
    assert_eq!(
        rejected.identity_conflicts[0].candidate_provider_id,
        "youtube-rejected"
    );
    assert_eq!(
        rejected.identity_conflicts[0].status,
        IdentityConflictStatus::Rejected
    );
    assert!(rejected.identity_conflicts[0].rejected_at.is_some());

    let unparseable = migrated
        .tracks
        .iter()
        .find(|track| track.id == "track-unparseable")
        .unwrap();
    assert!(unparseable.identity_conflicts.is_empty());

    let normal = migrated
        .tracks
        .iter()
        .find(|track| track.id == "track-normal")
        .unwrap();
    assert!(normal.identity_conflicts.is_empty());

    // A pre-migration snapshot was taken before the schema was upgraded.
    assert_eq!(
        fs::read_dir(temp.path().join("dump").join("backups"))
            .unwrap()
            .count(),
        1
    );

    // The database on disk was permanently bumped to the current version.
    let database = Connection::open(&database_path).unwrap();
    let stored_version: String = database
        .query_row(
            "SELECT value FROM library_metadata WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_version, "5");
}

#[test]
fn opening_future_version_database_is_rejected() {
    let temp = tempdir().unwrap();
    write_library_state_in(temp.path(), &LibraryState::new()).unwrap();
    let database_path = database_path_in(temp.path());
    let database = Connection::open(&database_path).unwrap();
    database
        .execute(
            "UPDATE library_metadata SET value = '999' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
    drop(database);

    let error = read_library_state_in(temp.path(), false).unwrap_err();
    assert!(error.to_string().contains("newer than this app supports"));
}

#[test]
fn legacy_provider_credentials_move_out_of_canonical_database() {
    let temp = tempdir().unwrap();
    write_library_state_in(temp.path(), &LibraryState::new()).unwrap();
    let database_path = database_path_in(temp.path());
    let database = Connection::open(&database_path).unwrap();
    database
        .execute_batch(
            "CREATE TABLE provider_connections (
                provider TEXT PRIMARY KEY,
                config_json TEXT NOT NULL,
                connected_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );",
        )
        .unwrap();
    let now = Utc::now().to_rfc3339();
    let config_json = serde_json::to_string(&SpotifyConnectionConfig {
        client_id: "client-id".to_string(),
        client_secret: "client-secret".to_string(),
        refresh_token: "refresh-token".to_string(),
    })
    .unwrap();
    database
        .execute(
            "INSERT INTO provider_connections
                (provider, config_json, connected_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)",
            params![ProviderKind::Spotify.as_key(), config_json, now],
        )
        .unwrap();
    drop(database);

    let connections = list_provider_connections_in(temp.path()).unwrap();

    assert_eq!(connections.len(), 1);
    assert_eq!(connections[0].provider, ProviderKind::Spotify);
    let library = Connection::open(&database_path).unwrap();
    let remaining: i64 = library
        .query_row("SELECT COUNT(*) FROM provider_connections", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(remaining, 0);
    assert!(runtime_database_path_in(temp.path()).exists());
    assert_eq!(
        fs::read_dir(temp.path().join("dump").join("backups"))
            .unwrap()
            .count(),
        1
    );
}

fn observed_track(
    provider_id: &str,
    title: &str,
    artists: &[&str],
    album: Option<&str>,
) -> ObservedTrack {
    ObservedTrack {
        metadata: TrackMetadata {
            title: title.to_string(),
            artists: artists.iter().map(|artist| artist.to_string()).collect(),
            album: album.map(str::to_string),
            duration_seconds: None,
            isrc: None,
        },
        provider_id: Some(provider_id.to_string()),
        artwork: None,
    }
}

fn track_entity(id: &str, title: &str) -> TrackEntity {
    TrackEntity {
        id: id.to_string(),
        metadata: TrackMetadata {
            title: title.to_string(),
            artists: vec!["Inon Zur".to_string()],
            album: Some("Fallout 3".to_string()),
            duration_seconds: None,
            isrc: None,
        },
        provider_links: BTreeMap::new(),
        provider_artwork: BTreeMap::new(),
        identity_conflicts: Vec::new(),
        provider_state: BTreeMap::new(),
    }
}
