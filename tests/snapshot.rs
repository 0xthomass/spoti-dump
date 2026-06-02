use std::collections::BTreeMap;
use std::fs;

use chrono::{Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use tempfile::tempdir;

use spoti_dump::model::{
    LibraryState, LinkSource, ObservedPlaylist, ObservedPlaylistTrack, ObservedSavedTrack,
    ObservedTrack, PlaylistEntity, PlaylistEntry, ProviderCooldown, ProviderHealth, ProviderKind,
    ProviderLibrarySnapshot, ProviderTrackArtwork, SavedTrackEntry, SpotifyConnectionConfig,
    SyncState, SyncStatusRecord, TrackEntity, TrackMetadata,
};
use spoti_dump::state::merge_provider_snapshot;
use spoti_dump::storage::{
    create_manual_library_backup_in, database_health_in, database_path_in, export_csv,
    list_library_backups_in, list_provider_connections_in, list_provider_cooldowns_in,
    list_provider_healths_in, list_ui_operation_json_in, manual_backup_dir_in,
    read_library_state_in, read_provider_cooldown_in, read_provider_health_in,
    read_ui_operation_json_in, restore_library_backup_in, runtime_database_path_in,
    save_provider_cooldown_in, save_provider_health_in, save_ui_operation_json_in,
    write_library_state_in,
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
    assert!(export_dir.join("saved_tracks.csv").exists());

    let tracks_csv = fs::read_to_string(export_dir.join("tracks.csv")).unwrap();
    let track_status_csv =
        fs::read_to_string(export_dir.join("track_provider_status.csv")).unwrap();

    assert!(tracks_csv.contains("track_1"));
    assert!(track_status_csv.contains("spotify"));
    assert!(track_status_csv.contains("synced"));
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
    let backup = create_manual_library_backup_in(temp.path()).unwrap();
    let backup_database = Connection::open(&backup.path).unwrap();
    backup_database
        .execute("DROP TABLE track_provider_artwork", [])
        .unwrap();
    drop(backup_database);

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
    let restored_database = Connection::open(database_path_in(temp.path())).unwrap();
    let artwork_table_exists: i64 = restored_database
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'track_provider_artwork'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(artwork_table_exists, 1);
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

#[test]
fn opening_older_database_snapshots_before_schema_migration() {
    let temp = tempdir().unwrap();
    write_library_state_in(temp.path(), &LibraryState::new()).unwrap();
    let database_path = database_path_in(temp.path());
    let database = Connection::open(&database_path).unwrap();
    database
        .execute("DROP TABLE track_provider_artwork", [])
        .unwrap();
    drop(database);

    read_library_state_in(temp.path(), false).unwrap();

    assert_eq!(
        fs::read_dir(temp.path().join("dump").join("backups"))
            .unwrap()
            .count(),
        1
    );
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
        provider_state: BTreeMap::new(),
    }
}
