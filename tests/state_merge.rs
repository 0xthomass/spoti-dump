//! Characterization tests for the canonical mutation engine in `src/domain/`.
//!
//! These lock in the behavior of `merge_provider_snapshot` and the related
//! public `LibraryState` mutators. Where the behavior is surprising
//! (appended-not-inserted playlist order, a coherent artwork record treated as
//! one unit, sanitation-driven skips) the assertions and their comments
//! document exactly what happens rather than what is ideal.

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
// 2. Artwork (url, width, height) is ONE unit; the larger whole image wins.
// ---------------------------------------------------------------------------

fn merge_spotify_artwork(
    library: &mut LibraryState,
    url: &str,
    width: Option<u32>,
    height: Option<u32>,
) {
    let base = metadata(
        "Sirius",
        &["The Alan Parsons Project"],
        Some("Eye In The Sky"),
        None,
        None,
    );
    merge_provider_snapshot(
        library,
        provider_snapshot(
            ProviderKind::Spotify,
            vec![saved(
                None,
                observed_with_artwork(Some("s1"), base, url, width, height),
            )],
            Vec::new(),
        ),
    );
}

#[test]
fn observed_artwork_keeps_url_and_dimensions_as_one_coherent_unit() {
    let key = ProviderKind::Spotify.as_key();

    let mut library = LibraryState::new();
    merge_spotify_artwork(&mut library, "url-small", Some(300), Some(300));

    // A larger later observation replaces the whole record: url AND dimensions.
    merge_spotify_artwork(&mut library, "url-large", Some(640), Some(640));
    assert_eq!(library.tracks.len(), 1);
    let art = library.tracks[0].provider_artwork.get(key).unwrap();
    assert_eq!(
        (art.width, art.height, art.url.as_str()),
        (Some(640), Some(640), "url-large")
    );

    // A SMALLER later observation is rejected WHOLE: neither the dimensions nor
    // the URL move, so the stored url still describes the stored dimensions
    // (the desync where a smaller image's url landed on the larger dimensions
    // is gone).
    merge_spotify_artwork(&mut library, "url-tiny", Some(100), Some(100));
    let art = library.tracks[0].provider_artwork.get(key).unwrap();
    assert_eq!(
        (art.width, art.height, art.url.as_str()),
        (Some(640), Some(640), "url-large")
    );

    // A dimensionless observation likewise cannot displace a known-size image.
    merge_spotify_artwork(&mut library, "url-none", None, None);
    let art = library.tracks[0].provider_artwork.get(key).unwrap();
    assert_eq!(
        (art.width, art.height, art.url.as_str()),
        (Some(640), Some(640), "url-large")
    );
}

#[test]
fn observed_artwork_replaces_a_dimensionless_record_and_only_advances_last_seen() {
    let key = ProviderKind::Spotify.as_key();

    let mut library = LibraryState::new();
    // The first observation carries no dimensions at all (score 0).
    merge_spotify_artwork(&mut library, "url-unknown", None, None);
    let first_seen = library.tracks[0]
        .provider_artwork
        .get(key)
        .unwrap()
        .last_seen_at;
    assert!(first_seen.is_some());

    // Any sized observation outranks a dimensionless record and takes over whole.
    merge_spotify_artwork(&mut library, "url-sized", Some(300), Some(300));
    let art = library.tracks[0].provider_artwork.get(key).unwrap();
    assert_eq!(
        (art.width, art.height, art.url.as_str()),
        (Some(300), Some(300), "url-sized")
    );

    // An equally-large observation still replaces the record (>= wins), so the
    // freshest URL is kept and last_seen_at only ever advances.
    merge_spotify_artwork(&mut library, "url-sized-newer", Some(300), Some(300));
    let art = library.tracks[0].provider_artwork.get(key).unwrap();
    assert_eq!(art.url, "url-sized-newer");
    assert!(art.last_seen_at >= first_seen);
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
// 8. Merge path sanitizes observed metadata, skipping items that fail it.
// ---------------------------------------------------------------------------

#[test]
fn merge_skips_snapshot_items_whose_metadata_fails_sanitation() {
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

    // The snapshot merge path now runs the SAME sanitation `update_track_metadata`
    // enforces. A whitespace-only title fails the "non-blank title" rule, so the
    // item is skipped with a warning (the merge is not aborted) rather than stored
    // verbatim.
    assert_eq!(summary.tracks_created, 0);
    assert_eq!(summary.saved_tracks_seen, 0);
    assert_eq!(summary.warnings.len(), 1);
    assert!(summary.warnings[0].contains("metadata failed validation"));
    assert!(library.tracks.is_empty());
    assert!(library.saved_tracks.is_empty());
}

#[test]
fn merge_skips_playlist_entries_referencing_unsanitizable_tracks() {
    let mut library = LibraryState::new();
    let summary = merge_provider_snapshot(
        &mut library,
        provider_snapshot(
            ProviderKind::Spotify,
            Vec::new(),
            vec![observed_playlist(
                Some("p1"),
                "Mix",
                vec![
                    playlist_track(
                        None,
                        observed(
                            Some("ok"),
                            metadata("Good Song", &["Artist"], None, None, None),
                        ),
                    ),
                    playlist_track(
                        None,
                        observed(Some("bad"), metadata("   ", &["Artist"], None, None, None)),
                    ),
                ],
            )],
        ),
    );

    // The blank-title entry is dropped with the same warning; the playlist keeps
    // only the valid track, and no entry references a track that was never stored.
    assert_eq!(library.tracks.len(), 1);
    assert_eq!(library.playlists.len(), 1);
    assert_eq!(library.playlists[0].entries.len(), 1);
    let stored_track_id = library.playlists[0].entries[0].track_id.clone();
    assert!(library
        .tracks
        .iter()
        .any(|track| track.id == stored_track_id));
    assert!(summary
        .warnings
        .iter()
        .any(|warning| warning.contains("metadata failed validation")));
    library.validate().unwrap();
}

#[test]
fn merge_applies_metadata_sanitation_to_stored_tracks() {
    let mut library = LibraryState::new();
    merge_provider_snapshot(
        &mut library,
        provider_snapshot(
            ProviderKind::Spotify,
            vec![saved(
                None,
                observed(
                    Some("s1"),
                    // Padded title, blank/duplicate/whitespace artists, a blank
                    // album, a zero duration and a lower-case ISRC: sanitation
                    // cleans them all, exactly as a manual edit would.
                    metadata(
                        "  Real Song  ",
                        &["", "Real Artist", "  ", "real artist"],
                        Some("   "),
                        Some(0),
                        Some("usabc1234567"),
                    ),
                ),
            )],
            Vec::new(),
        ),
    );

    assert_eq!(library.tracks.len(), 1);
    let track = &library.tracks[0];
    assert_eq!(track.metadata.title, "Real Song");
    assert_eq!(track.metadata.artists, vec!["Real Artist".to_string()]);
    assert_eq!(track.metadata.album, None);
    assert_eq!(track.metadata.duration_seconds, None);
    assert_eq!(track.metadata.isrc.as_deref(), Some("USABC1234567"));
    library.validate().unwrap();
}

// ---------------------------------------------------------------------------
// 9. Status merges keep only the winning record's own timestamps; added_at
//    merges compare instants.
// ---------------------------------------------------------------------------

#[test]
fn winning_status_record_carries_only_its_own_timestamps() {
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

    // Error outranks Missing, so the source record wins WHOLE. It keeps its own
    // (empty) timestamps and does NOT absorb the discarded Missing record's
    // `seen_at`: a discarded record never advances the kept record's timeline.
    let status = library.tracks[0].provider_state.get(&key).unwrap();
    assert_eq!(status.state, SyncState::Error);
    assert_eq!(status.message.as_deref(), Some("Push failed"));
    assert_eq!(status.last_seen_at, None);
    assert_eq!(status.last_attempt_at, None);
    assert_eq!(status.last_success_at, None);
}

#[test]
fn kept_status_record_does_not_inherit_a_discarded_records_timestamps() {
    let key = ProviderKind::Spotify.as_key().to_string();
    let at = Utc::now();
    // Target is Unmatched: it has never synced, so it holds no last_success_at.
    let mut target = track_entity("target", metadata("Song", &["Artist"], None, None, None));
    target
        .provider_state
        .insert(key.clone(), SyncStatusRecord::unmatched("No match", at));
    // Source is a lower-ranked Skipped record that nonetheless carries a stale
    // success timestamp from an earlier life.
    let mut source = track_entity("source", metadata("Song", &["Artist"], None, None, None));
    source.provider_state.insert(
        key.clone(),
        SyncStatusRecord {
            state: SyncState::Skipped,
            last_success_at: Some(at),
            ..Default::default()
        },
    );
    let mut library = LibraryState::new();
    library.tracks.push(target);
    library.tracks.push(source);

    library.merge_track_into("source", "target").unwrap();

    // Unmatched outranks Skipped, so the Unmatched record is kept. It must NOT
    // pick up the discarded record's last_success_at: an unmatched item has
    // never synced, so a success timestamp would be incoherent.
    let status = library.tracks[0].provider_state.get(&key).unwrap();
    assert_eq!(status.state, SyncState::Unmatched);
    assert_eq!(status.last_success_at, None);
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
