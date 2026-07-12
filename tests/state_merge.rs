//! Characterization tests for the canonical mutation engine in `src/domain/`.
//!
//! These lock in the behavior of `merge_provider_snapshot` and the related
//! public `LibraryState` mutators. Where the behavior is surprising (URL
//! always overwritten, merge path skips sanitization, appended-not-inserted
//! playlist order) the assertions and their comments document exactly what
//! happens rather than what is ideal.

use std::collections::BTreeMap;

use chrono::Utc;

use spoti_dump::domain::{
    merge_provider_snapshot, IdentityConflictStatus, LibraryState, LinkSource, ObservedArtwork,
    ObservedPlaylist, ObservedPlaylistTrack, ObservedSavedTrack, ObservedTrack, PlaylistEntity,
    PlaylistEntry, ProviderKind, ProviderLibrarySnapshot, SavedTrackEntry, SyncState,
    SyncStatusRecord, TrackEntity, TrackMetadata,
};

// ---------------------------------------------------------------------------
// Local helper builders (mirroring the style of tests/snapshot.rs).
// ---------------------------------------------------------------------------

fn metadata(
    title: &str,
    artists: &[&str],
    album: Option<&str>,
    duration_seconds: Option<u32>,
    isrc: Option<&str>,
) -> TrackMetadata {
    TrackMetadata {
        title: title.to_string(),
        artists: artists.iter().map(|artist| artist.to_string()).collect(),
        album: album.map(str::to_string),
        duration_seconds,
        isrc: isrc.map(str::to_string),
    }
}

fn observed(provider_id: Option<&str>, metadata: TrackMetadata) -> ObservedTrack {
    ObservedTrack {
        metadata,
        provider_id: provider_id.map(str::to_string),
        artwork: None,
    }
}

fn observed_with_artwork(
    provider_id: Option<&str>,
    metadata: TrackMetadata,
    url: &str,
    width: Option<u32>,
    height: Option<u32>,
) -> ObservedTrack {
    ObservedTrack {
        metadata,
        provider_id: provider_id.map(str::to_string),
        artwork: Some(ObservedArtwork {
            url: url.to_string(),
            width,
            height,
        }),
    }
}

fn saved(added_at: Option<&str>, track: ObservedTrack) -> ObservedSavedTrack {
    ObservedSavedTrack {
        added_at: added_at.map(str::to_string),
        track,
    }
}

fn playlist_track(added_at: Option<&str>, track: ObservedTrack) -> ObservedPlaylistTrack {
    ObservedPlaylistTrack {
        added_at: added_at.map(str::to_string),
        track,
    }
}

fn observed_playlist(
    provider_id: Option<&str>,
    name: &str,
    tracks: Vec<ObservedPlaylistTrack>,
) -> ObservedPlaylist {
    ObservedPlaylist {
        provider_id: provider_id.map(str::to_string),
        name: name.to_string(),
        description: None,
        tracks,
    }
}

fn provider_snapshot(
    provider: ProviderKind,
    saved_tracks: Vec<ObservedSavedTrack>,
    playlists: Vec<ObservedPlaylist>,
) -> ProviderLibrarySnapshot {
    ProviderLibrarySnapshot {
        provider,
        captured_at: Utc::now(),
        saved_tracks,
        playlists,
        warnings: Vec::new(),
    }
}

fn track_entity(id: &str, metadata: TrackMetadata) -> TrackEntity {
    TrackEntity {
        id: id.to_string(),
        metadata,
        provider_links: BTreeMap::new(),
        provider_artwork: BTreeMap::new(),
        identity_conflicts: Vec::new(),
        provider_state: BTreeMap::new(),
    }
}

fn spotify_link_id(track: &TrackEntity) -> Option<&str> {
    track
        .provider_links
        .get(ProviderKind::Spotify.as_key())
        .map(|link| link.provider_id.as_str())
}

fn spotify_state(track: &TrackEntity) -> Option<SyncState> {
    track
        .provider_state
        .get(ProviderKind::Spotify.as_key())
        .map(|status| status.state)
}

// ---------------------------------------------------------------------------
// 1. merge_status_maps precedence via status_rank.
// ---------------------------------------------------------------------------

#[test]
fn merging_tracks_keeps_the_highest_ranked_sync_state() {
    // `merge_track_into` is the public entry point that reaches `merge_status_maps`,
    // which uses `status_rank` (Synced=6 .. Pending=1) to decide which record wins.
    fn record(state: SyncState) -> SyncStatusRecord {
        SyncStatusRecord {
            state,
            ..Default::default()
        }
    }
    fn state_with(target_state: SyncState, source_state: SyncState) -> LibraryState {
        let key = ProviderKind::Spotify.as_key().to_string();
        let mut target = track_entity("target", metadata("Song", &["Artist"], None, None, None));
        target
            .provider_state
            .insert(key.clone(), record(target_state));
        let mut source = track_entity("source", metadata("Song", &["Artist"], None, None, None));
        source.provider_state.insert(key, record(source_state));
        let mut library = LibraryState::new();
        library.tracks.push(target);
        library.tracks.push(source);
        library
    }

    // A `synced` target is NOT downgraded when a `pending` source is merged into it.
    let mut library = state_with(SyncState::Synced, SyncState::Pending);
    library.merge_track_into("source", "target").unwrap();
    assert_eq!(library.tracks.len(), 1);
    assert_eq!(spotify_state(&library.tracks[0]), Some(SyncState::Synced));

    // A `pending` target IS upgraded when a `synced` source is merged into it
    // (higher rank always wins, regardless of merge direction).
    let mut library = state_with(SyncState::Pending, SyncState::Synced);
    library.merge_track_into("source", "target").unwrap();
    assert_eq!(spotify_state(&library.tracks[0]), Some(SyncState::Synced));
}

// ---------------------------------------------------------------------------
// 2. Artwork dimension precedence (preferred_dimension) via merge_provider_snapshot.
// ---------------------------------------------------------------------------

#[test]
fn observed_artwork_keeps_largest_dimensions_but_always_takes_latest_url() {
    let key = ProviderKind::Spotify.as_key();
    let base = || {
        metadata(
            "Sirius",
            &["The Alan Parsons Project"],
            Some("Eye In The Sky"),
            None,
            None,
        )
    };
    let merge_artwork = |library: &mut LibraryState, url: &str, w: Option<u32>, h: Option<u32>| {
        merge_provider_snapshot(
            library,
            provider_snapshot(
                ProviderKind::Spotify,
                vec![saved(
                    None,
                    observed_with_artwork(Some("s1"), base(), url, w, h),
                )],
                Vec::new(),
            ),
        );
    };

    let mut library = LibraryState::new();
    merge_artwork(&mut library, "url-small", Some(300), Some(300));

    // A larger later observation upgrades the stored dimensions.
    merge_artwork(&mut library, "url-large", Some(640), Some(640));
    assert_eq!(library.tracks.len(), 1);
    let art = library.tracks[0].provider_artwork.get(key).unwrap();
    assert_eq!(art.width, Some(640));
    assert_eq!(art.height, Some(640));
    assert_eq!(art.url, "url-large");

    // A SMALLER later observation does not shrink the dimensions...
    merge_artwork(&mut library, "url-tiny", Some(100), Some(100));
    let art = library.tracks[0].provider_artwork.get(key).unwrap();
    assert_eq!(art.width, Some(640));
    assert_eq!(art.height, Some(640));
    // ...but the URL is STILL overwritten with the smaller image's URL. This is a
    // documented quirk: `upsert_track_artwork` keeps max(width/height) yet always
    // replaces `url`, so the stored dimensions can describe a different image.
    assert_eq!(art.url, "url-tiny");

    // A `None` dimension likewise never clobbers a known dimension.
    merge_artwork(&mut library, "url-none", None, None);
    let art = library.tracks[0].provider_artwork.get(key).unwrap();
    assert_eq!(art.width, Some(640));
    assert_eq!(art.height, Some(640));
    assert_eq!(art.url, "url-none");
}

// ---------------------------------------------------------------------------
// 3. Explicit provider-id match wins over metadata (today's behavior).
// ---------------------------------------------------------------------------

#[test]
fn explicit_provider_id_match_wins_over_fuzzy_metadata_match() {
    let mut library = LibraryState::new();
    let now = Utc::now();
    library.tracks.push(track_entity(
        "t1",
        metadata("Song One", &["Artist A"], Some("Album A"), Some(200), None),
    ));
    library.tracks.push(track_entity(
        "t2",
        metadata("Song Two", &["Artist B"], Some("Album B"), Some(300), None),
    ));
    // t1 owns the Spotify id "shared-id" even though its metadata differs from
    // the export; t2's metadata is an exact twin of what the export carries.
    assert!(library.upsert_track_link(
        "t1",
        ProviderKind::Spotify,
        "shared-id",
        LinkSource::Export,
        Some(1.0),
        now,
    ));

    merge_provider_snapshot(
        &mut library,
        provider_snapshot(
            ProviderKind::Spotify,
            vec![saved(
                Some("2024-01-01T00:00:00Z"),
                observed(
                    Some("shared-id"),
                    metadata("Song Two", &["Artist B"], Some("Album B"), Some(300), None),
                ),
            )],
            Vec::new(),
        ),
    );

    // The provider-id lookup short-circuits before fuzzy matching, so the export
    // re-attaches to t1 (the id owner) and never touches its metadata-twin t2.
    assert_eq!(library.tracks.len(), 2);
    let t1 = library.tracks.iter().find(|t| t.id == "t1").unwrap();
    let t2 = library.tracks.iter().find(|t| t.id == "t2").unwrap();
    assert_eq!(t1.metadata.title, "Song One");
    assert_eq!(spotify_link_id(t1), Some("shared-id"));
    assert_eq!(t2.metadata.title, "Song Two");
    assert!(t2.provider_links.is_empty());
    assert_eq!(library.saved_tracks.len(), 1);
    assert_eq!(library.saved_tracks[0].track_id, "t1");
    library.validate().unwrap();
}

// ---------------------------------------------------------------------------
// 4. Fuzzy metadata matching (no provider id).
// ---------------------------------------------------------------------------

#[test]
fn snapshot_without_provider_id_fuzzy_merges_into_near_identical_track() {
    let mut library = LibraryState::new();
    library.tracks.push(track_entity(
        "t1",
        metadata("Africa", &["Toto"], Some("Toto IV"), Some(295), None),
    ));

    merge_provider_snapshot(
        &mut library,
        provider_snapshot(
            ProviderKind::Spotify,
            vec![saved(
                None,
                // No provider id and no ISRC -> resolved purely by metadata similarity.
                // Near-identical (only duration differs by 1s) scores ~0.997 >= 0.94.
                observed(
                    None,
                    metadata("Africa", &["Toto"], Some("Toto IV"), Some(296), None),
                ),
            )],
            Vec::new(),
        ),
    );

    assert_eq!(
        library.tracks.len(),
        1,
        "near-identical metadata folds into the existing canonical track"
    );
    assert_eq!(library.saved_tracks.len(), 1);
    assert_eq!(library.saved_tracks[0].track_id, "t1");
}

#[test]
fn snapshot_without_provider_id_creates_new_track_for_distinct_metadata() {
    let mut library = LibraryState::new();
    library.tracks.push(track_entity(
        "t1",
        metadata("Africa", &["Toto"], Some("Toto IV"), Some(295), None),
    ));

    merge_provider_snapshot(
        &mut library,
        provider_snapshot(
            ProviderKind::Spotify,
            vec![saved(
                None,
                observed(
                    None,
                    metadata(
                        "Purple Rain",
                        &["Prince"],
                        Some("Purple Rain"),
                        Some(520),
                        None,
                    ),
                ),
            )],
            Vec::new(),
        ),
    );

    assert_eq!(
        library.tracks.len(),
        2,
        "clearly different metadata below the 0.94 threshold creates a new track"
    );
}

// ---------------------------------------------------------------------------
// 5. Idempotence: merging the same snapshot twice does not duplicate.
// ---------------------------------------------------------------------------

#[test]
fn merging_the_same_snapshot_twice_is_idempotent() {
    let build = || {
        let track = || {
            observed(
                Some("s1"),
                metadata(
                    "Sirius",
                    &["The Alan Parsons Project"],
                    Some("Eye In The Sky"),
                    None,
                    None,
                ),
            )
        };
        provider_snapshot(
            ProviderKind::Spotify,
            vec![saved(Some("2024-01-01T00:00:00Z"), track())],
            vec![observed_playlist(
                Some("p1"),
                "Favorites",
                vec![playlist_track(Some("2024-01-01T00:00:00Z"), track())],
            )],
        )
    };

    let mut library = LibraryState::new();
    merge_provider_snapshot(&mut library, build());
    merge_provider_snapshot(&mut library, build());

    assert_eq!(library.tracks.len(), 1);
    assert_eq!(library.saved_tracks.len(), 1);
    assert_eq!(library.playlists.len(), 1);
    assert_eq!(library.playlists[0].entries.len(), 1);
    library.validate().unwrap();
}

// ---------------------------------------------------------------------------
// 6. Playlist entries are append-only (order is NOT reconciled to the provider).
// ---------------------------------------------------------------------------

#[test]
fn re_exported_playlist_appends_new_track_instead_of_inserting_in_provider_order() {
    let alpha = || observed(Some("s-a"), metadata("Alpha", &["A"], None, None, None));
    let bravo = || observed(Some("s-b"), metadata("Bravo", &["B"], None, None, None));
    let xray = || observed(Some("s-x"), metadata("Xray", &["X"], None, None, None));

    let mut library = LibraryState::new();
    merge_provider_snapshot(
        &mut library,
        provider_snapshot(
            ProviderKind::Spotify,
            Vec::new(),
            vec![observed_playlist(
                Some("p1"),
                "Mix",
                vec![playlist_track(None, alpha()), playlist_track(None, bravo())],
            )],
        ),
    );

    // Re-export inserts Xray BETWEEN Alpha and Bravo in provider order.
    merge_provider_snapshot(
        &mut library,
        provider_snapshot(
            ProviderKind::Spotify,
            Vec::new(),
            vec![observed_playlist(
                Some("p1"),
                "Mix",
                vec![
                    playlist_track(None, alpha()),
                    playlist_track(None, xray()),
                    playlist_track(None, bravo()),
                ],
            )],
        ),
    );

    let title_of = |track_id: &str| {
        library
            .tracks
            .iter()
            .find(|t| t.id == track_id)
            .unwrap()
            .metadata
            .title
            .clone()
    };
    let order: Vec<String> = library.playlists[0]
        .entries
        .iter()
        .map(|entry| title_of(&entry.track_id))
        .collect();

    // Existing entries stay put and the new track is appended at the END; the
    // provider's insertion position is not honored (`merge_playlist_entries` is
    // append-only).
    assert_eq!(
        order,
        vec!["Alpha".to_string(), "Bravo".to_string(), "Xray".to_string()]
    );
}

// ---------------------------------------------------------------------------
// 7. prune_track_if_unreferenced via public removal functions.
// ---------------------------------------------------------------------------

#[test]
fn removing_last_playlist_reference_prunes_the_track() {
    let mut library = LibraryState::new();
    library.tracks.push(track_entity(
        "t1",
        metadata("Sirius", &["APP"], None, None, None),
    ));
    library.playlists.push(PlaylistEntity {
        id: "p1".to_string(),
        name: "Favorites".to_string(),
        description: None,
        provider_links: BTreeMap::new(),
        provider_state: BTreeMap::new(),
        entries: vec![PlaylistEntry {
            id: "e1".to_string(),
            track_id: "t1".to_string(),
            added_at: None,
            provider_state: BTreeMap::new(),
        }],
    });

    assert!(library.remove_playlist("p1"));
    assert!(library.playlists.is_empty());
    assert!(
        library.tracks.is_empty(),
        "a track whose only reference was the removed playlist is pruned"
    );
}

#[test]
fn removing_saved_track_keeps_track_still_referenced_by_a_playlist() {
    let mut library = LibraryState::new();
    library.tracks.push(track_entity(
        "t1",
        metadata("Sirius", &["APP"], None, None, None),
    ));
    library.saved_tracks.push(SavedTrackEntry {
        id: "saved-1".to_string(),
        track_id: "t1".to_string(),
        added_at: None,
        provider_state: BTreeMap::new(),
    });
    library.playlists.push(PlaylistEntity {
        id: "p1".to_string(),
        name: "Favorites".to_string(),
        description: None,
        provider_links: BTreeMap::new(),
        provider_state: BTreeMap::new(),
        entries: vec![PlaylistEntry {
            id: "e1".to_string(),
            track_id: "t1".to_string(),
            added_at: None,
            provider_state: BTreeMap::new(),
        }],
    });

    assert!(library.remove_saved_track("saved-1"));
    assert!(library.saved_tracks.is_empty());
    assert_eq!(
        library.tracks.len(),
        1,
        "track survives because a playlist still references it"
    );
    assert_eq!(library.playlists[0].entries.len(), 1);
}

// ---------------------------------------------------------------------------
// 8. Merge path does NOT sanitize metadata.
// ---------------------------------------------------------------------------

#[test]
fn merge_stores_blank_metadata_verbatim_without_sanitizing_or_skipping() {
    let mut library = LibraryState::new();
    let summary = merge_provider_snapshot(
        &mut library,
        provider_snapshot(
            ProviderKind::Spotify,
            vec![saved(
                None,
                observed(
                    Some("s1"),
                    metadata("   ", &["", "Real Artist"], None, None, None),
                ),
            )],
            Vec::new(),
        ),
    );

    // `sanitize_track_metadata` is only reached from `update_track_metadata`, NOT
    // from the snapshot merge path. So a whitespace-only title and an empty artist
    // entry are neither rejected nor skipped nor warned about: they are stored as-is.
    assert_eq!(summary.tracks_created, 1);
    assert!(summary.warnings.is_empty());
    assert_eq!(library.tracks.len(), 1);
    assert_eq!(library.tracks[0].metadata.title, "   ");
    assert_eq!(
        library.tracks[0].metadata.artists,
        vec!["".to_string(), "Real Artist".to_string()]
    );
}

// ---------------------------------------------------------------------------
// 9. Status merges preserve timestamps; added_at merges compare instants.
// ---------------------------------------------------------------------------

#[test]
fn winning_status_record_keeps_the_other_sides_timestamps() {
    let key = ProviderKind::Spotify.as_key().to_string();
    let seen_at = Utc::now();
    let mut target = track_entity("target", metadata("Song", &["Artist"], None, None, None));
    target.provider_state.insert(
        key.clone(),
        SyncStatusRecord::missing("Missing at provider", seen_at),
    );
    let mut source = track_entity("source", metadata("Song", &["Artist"], None, None, None));
    source.provider_state.insert(
        key.clone(),
        SyncStatusRecord {
            state: SyncState::Error,
            message: Some("Push failed".to_string()),
            ..Default::default()
        },
    );
    let mut library = LibraryState::new();
    library.tracks.push(target);
    library.tracks.push(source);

    library.merge_track_into("source", "target").unwrap();

    // Error outranks Missing so the source record wins, but the target's
    // timestamps survive the replacement instead of being discarded.
    let status = library.tracks[0].provider_state.get(&key).unwrap();
    assert_eq!(status.state, SyncState::Error);
    assert_eq!(status.message.as_deref(), Some("Push failed"));
    assert_eq!(status.last_seen_at, Some(seen_at));
    assert_eq!(status.last_attempt_at, Some(seen_at));
}

#[test]
fn merge_added_at_keeps_the_chronologically_earlier_instant_across_formats() {
    let mut library = LibraryState::new();
    library.tracks.push(track_entity(
        "t1",
        metadata("Song", &["Artist"], None, None, None),
    ));
    library.tracks.push(track_entity(
        "t2",
        metadata("Song", &["Artist"], None, None, None),
    ));
    // Lexically "2024-01-01T22:00:00-05:00" sorts BEFORE "2024-01-02T00:00:00Z",
    // but as an instant it is 2024-01-02T03:00:00Z, i.e. LATER.
    library.saved_tracks.push(SavedTrackEntry {
        id: "saved-1".to_string(),
        track_id: "t1".to_string(),
        added_at: Some("2024-01-02T00:00:00Z".to_string()),
        provider_state: BTreeMap::new(),
    });
    library.saved_tracks.push(SavedTrackEntry {
        id: "saved-2".to_string(),
        track_id: "t2".to_string(),
        added_at: Some("2024-01-01T22:00:00-05:00".to_string()),
        provider_state: BTreeMap::new(),
    });

    library.merge_track_into("t2", "t1").unwrap();

    assert_eq!(library.saved_tracks.len(), 1);
    assert_eq!(
        library.saved_tracks[0].added_at.as_deref(),
        Some("2024-01-02T00:00:00Z"),
        "the chronologically earlier timestamp wins even when lexical order disagrees"
    );
}

// ---------------------------------------------------------------------------
// 10. ISRC/fuzzy match must not clobber an existing link.
// ---------------------------------------------------------------------------

#[test]
fn isrc_match_preserves_existing_provider_link_and_records_conflict() {
    let mut library = LibraryState::new();
    let now = Utc::now();
    library.tracks.push(track_entity(
        "t1",
        metadata(
            "Song",
            &["Artist"],
            Some("Album"),
            Some(200),
            Some("USABC1234567"),
        ),
    ));
    assert!(library.upsert_track_link(
        "t1",
        ProviderKind::Spotify,
        "old-id",
        LinkSource::Export,
        Some(1.0),
        now,
    ));

    // The export matches t1 by ISRC (its Spotify id "new-id" is unknown), yet it
    // carries a DIFFERENT Spotify id than the established link.
    let snapshot = || {
        provider_snapshot(
            ProviderKind::Spotify,
            vec![saved(
                None,
                observed(
                    Some("new-id"),
                    metadata(
                        "Song",
                        &["Artist"],
                        Some("Album"),
                        Some(200),
                        Some("USABC1234567"),
                    ),
                ),
            )],
            Vec::new(),
        )
    };
    let summary = merge_provider_snapshot(&mut library, snapshot());

    // The established link is kept and the mismatch is surfaced as an open
    // identity conflict plus a merge warning instead of clobbering the link.
    assert_eq!(library.tracks.len(), 1);
    assert_eq!(spotify_link_id(&library.tracks[0]), Some("old-id"));
    assert!(
        !summary.warnings.is_empty(),
        "the conflicting provider id should be recorded as a warning"
    );
    assert_eq!(library.tracks[0].identity_conflicts.len(), 1);
    let conflict = &library.tracks[0].identity_conflicts[0];
    assert_eq!(conflict.provider, ProviderKind::Spotify);
    assert_eq!(conflict.candidate_provider_id, "new-id");
    assert_eq!(conflict.status, IdentityConflictStatus::Open);
    // No synced status may claim the rejected id.
    assert_ne!(
        library.tracks[0]
            .provider_state
            .get(ProviderKind::Spotify.as_key())
            .and_then(|status| status.provider_item_id.as_deref()),
        Some("new-id")
    );
    // The saved entry is still recorded; only the identity link is disputed.
    assert_eq!(library.saved_tracks.len(), 1);
    library.validate().unwrap();

    // Re-observing the same conflict neither duplicates it nor warns again.
    let summary = merge_provider_snapshot(&mut library, snapshot());
    assert!(summary.warnings.is_empty());
    assert_eq!(library.tracks[0].identity_conflicts.len(), 1);
    library.validate().unwrap();
}
