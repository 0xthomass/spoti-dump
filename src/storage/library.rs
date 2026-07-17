//! The canonical library database: a single long-lived SQLite connection and
//! the load/save translation between [`LibraryState`] and the relational
//! schema.
//!
//! A [`LibraryDb`] opens the connection once, switches it to WAL, and runs
//! schema preparation/migrations a single time. Writes are change-guarded: a
//! save whose content matches the last loaded/saved state skips both the write
//! and the pre-write backup.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, Transaction};

use crate::domain::{
    LibraryState, PlaylistEntity, PlaylistEntry, ProviderKind, ProviderPlaylistLink,
    ProviderTrackArtwork, ProviderTrackLink, SavedTrackEntry, SyncStatusRecord, TrackEntity,
    TrackIdentityConflict, TrackMetadata, LIBRARY_STATE_FORMAT_VERSION,
};

use super::{
    backups, checkpoint, database_path_in, encode_datetime, encode_datetime_option,
    ensure_dump_dir, migrations, open_database, parse_datetime, read_metadata, LibraryBackup,
};

#[derive(Clone, Debug)]
pub struct DatabaseHealth {
    pub path: PathBuf,
    pub integrity_check: String,
    pub tracks: usize,
    pub saved_tracks: usize,
    pub playlists: usize,
    pub playlist_entries: usize,
}

/// Mutable state guarded by the handle's mutex.
struct Inner {
    connection: Connection,
    /// Canonical serialization of the state last loaded from or saved to this
    /// handle. Used by the change-guard to skip redundant writes/backups.
    last_serialized: Option<String>,
    /// Whether this handle has itself persisted meaningful content. Together
    /// with `existed_before_open` it decides whether a save takes a pre-write
    /// snapshot: a freshly created, never-written database has nothing worth
    /// backing up on its first save.
    has_persisted: bool,
}

/// A process-wide (or, for the `_in` paths, short-lived) handle to the
/// canonical `library.db`. Holds exactly one SQLite connection, guarded by a
/// mutex so the handle is `Sync` and can back the shared registry.
pub struct LibraryDb {
    root: PathBuf,
    database_path: PathBuf,
    /// Whether the database file existed before this handle opened it. Opening
    /// the connection creates the file, so this is captured up front to
    /// reproduce the "no snapshot on the write that first creates the database"
    /// behaviour.
    existed_before_open: bool,
    inner: Mutex<Inner>,
}

impl LibraryDb {
    /// Opens (creating if absent) and prepares the canonical database. Schema
    /// initialization and any pending migrations run here, once.
    pub fn open(root: &Path) -> Result<LibraryDb> {
        ensure_dump_dir(root)?;
        let database_path = database_path_in(root);
        let existed_before_open = database_path.exists();

        let mut connection = open_database(&database_path)?;
        connection.execute_batch("PRAGMA journal_mode = WAL;")?;
        migrations::prepare_library_database(root, &database_path, &mut connection)?;

        Ok(LibraryDb {
            root: root.to_path_buf(),
            database_path,
            existed_before_open,
            inner: Mutex::new(Inner {
                connection,
                last_serialized: None,
                has_persisted: false,
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Loads the full library state and records its content fingerprint so a
    /// subsequent identical save is skipped.
    pub fn load(&self) -> Result<LibraryState> {
        let mut guard = self.lock();
        let state = load_library_state(&guard.connection)?;
        guard.last_serialized = Some(canonical_serialize(&state)?);
        Ok(state)
    }

    /// Persists the library state, guarded by content change detection. When the
    /// content matches the last loaded/saved value both the write and the
    /// pre-write snapshot are skipped. When it differs, a bounded pre-write
    /// snapshot is taken (except on the write that first creates the database)
    /// before the tables are rewritten in a single transaction.
    pub fn save(&self, state: &LibraryState) -> Result<PathBuf> {
        state.validate()?;
        let serialized = canonical_serialize(state)?;

        let mut guard = self.lock();
        if guard.last_serialized.as_deref() == Some(serialized.as_str()) {
            return Ok(self.database_path.clone());
        }

        if self.existed_before_open || guard.has_persisted {
            checkpoint(&guard.connection)?;
            backups::snapshot_existing_database(&self.root, &self.database_path)?;
        }

        {
            let transaction = guard.connection.transaction()?;
            replace_library_state(&transaction, state)?;
            transaction.commit()?;
        }

        guard.last_serialized = Some(serialized);
        guard.has_persisted = true;
        Ok(self.database_path.clone())
    }

    /// Copies the live database into the manual (unpruned) backup directory.
    /// Fails if the handle only ever saw a freshly created, never-written
    /// database, matching the prior "no canonical library database exists"
    /// behaviour.
    pub fn create_manual_backup(&self) -> Result<LibraryBackup> {
        let guard = self.lock();
        if !self.existed_before_open && !guard.has_persisted {
            anyhow::bail!(
                "No canonical library database exists at {}.",
                self.database_path.display()
            );
        }
        checkpoint(&guard.connection)?;
        backups::copy_manual_backup(&self.root, &self.database_path)
    }

    pub fn health(&self) -> Result<DatabaseHealth> {
        let guard = self.lock();
        Ok(DatabaseHealth {
            path: self.database_path.clone(),
            integrity_check: guard
                .connection
                .query_row("PRAGMA integrity_check", [], |row| row.get(0))?,
            tracks: count_table_rows(&guard.connection, "tracks")? as usize,
            saved_tracks: count_table_rows(&guard.connection, "saved_tracks")? as usize,
            playlists: count_table_rows(&guard.connection, "playlists")? as usize,
            playlist_entries: count_table_rows(&guard.connection, "playlist_entries")? as usize,
        })
    }
}

/// Canonical, deterministic content fingerprint of the library state. Two states
/// serialize identically iff their persisted contents are identical, so exact
/// string comparison drives the change-guard without any collision risk.
fn canonical_serialize(state: &LibraryState) -> Result<String> {
    serde_json::to_string(state).context("Failed to serialize library state for change detection")
}

pub(super) fn count_table_rows(connection: &Connection, table_name: &'static str) -> Result<i64> {
    Ok(
        connection.query_row(&format!("SELECT COUNT(*) FROM {table_name}"), [], |row| {
            row.get(0)
        })?,
    )
}

/// DDL for a fresh database at the current schema version. The identity-conflict
/// table is created here as well as by the v5 migration; the two definitions are
/// kept identical on purpose.
pub(super) fn initialize_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS library_metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS tracks (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            album TEXT,
            duration_seconds INTEGER,
            isrc TEXT
        );

        CREATE TABLE IF NOT EXISTS track_artists (
            track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
            position INTEGER NOT NULL,
            name TEXT NOT NULL,
            PRIMARY KEY (track_id, position)
        );

        CREATE TABLE IF NOT EXISTS track_provider_links (
            track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
            provider TEXT NOT NULL,
            provider_id TEXT NOT NULL,
            source TEXT NOT NULL,
            confidence REAL,
            linked_at TEXT NOT NULL,
            last_seen_at TEXT,
            PRIMARY KEY (track_id, provider),
            UNIQUE (provider, provider_id)
        );

        CREATE TABLE IF NOT EXISTS track_provider_artwork (
            track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
            provider TEXT NOT NULL,
            url TEXT NOT NULL,
            width INTEGER,
            height INTEGER,
            last_seen_at TEXT,
            PRIMARY KEY (track_id, provider)
        );

        CREATE TABLE IF NOT EXISTS track_provider_status (
            track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
            provider TEXT NOT NULL,
            state TEXT NOT NULL,
            message TEXT,
            confidence REAL,
            provider_item_id TEXT,
            last_attempt_at TEXT,
            last_success_at TEXT,
            last_seen_at TEXT,
            PRIMARY KEY (track_id, provider)
        );

        CREATE TABLE IF NOT EXISTS track_identity_conflicts (
            track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
            provider TEXT NOT NULL,
            candidate_provider_id TEXT NOT NULL,
            confidence REAL,
            detected_at TEXT NOT NULL,
            status TEXT NOT NULL,
            rejected_at TEXT,
            PRIMARY KEY (track_id, provider, candidate_provider_id)
        );

        CREATE TABLE IF NOT EXISTS saved_tracks (
            id TEXT PRIMARY KEY,
            track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
            added_at TEXT
        );

        CREATE TABLE IF NOT EXISTS saved_track_provider_status (
            saved_track_id TEXT NOT NULL REFERENCES saved_tracks(id) ON DELETE CASCADE,
            provider TEXT NOT NULL,
            state TEXT NOT NULL,
            message TEXT,
            confidence REAL,
            provider_item_id TEXT,
            last_attempt_at TEXT,
            last_success_at TEXT,
            last_seen_at TEXT,
            PRIMARY KEY (saved_track_id, provider)
        );

        CREATE TABLE IF NOT EXISTS playlists (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT
        );

        CREATE TABLE IF NOT EXISTS playlist_provider_links (
            playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
            provider TEXT NOT NULL,
            provider_id TEXT NOT NULL,
            source TEXT NOT NULL,
            confidence REAL,
            linked_at TEXT NOT NULL,
            last_seen_at TEXT,
            PRIMARY KEY (playlist_id, provider),
            UNIQUE (provider, provider_id)
        );

        CREATE TABLE IF NOT EXISTS playlist_provider_status (
            playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
            provider TEXT NOT NULL,
            state TEXT NOT NULL,
            message TEXT,
            confidence REAL,
            provider_item_id TEXT,
            last_attempt_at TEXT,
            last_success_at TEXT,
            last_seen_at TEXT,
            PRIMARY KEY (playlist_id, provider)
        );

        CREATE TABLE IF NOT EXISTS playlist_entries (
            id TEXT PRIMARY KEY,
            playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
            position INTEGER NOT NULL,
            track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
            added_at TEXT,
            UNIQUE (playlist_id, position)
        );

        CREATE TABLE IF NOT EXISTS playlist_entry_provider_status (
            entry_id TEXT NOT NULL REFERENCES playlist_entries(id) ON DELETE CASCADE,
            provider TEXT NOT NULL,
            state TEXT NOT NULL,
            message TEXT,
            confidence REAL,
            provider_item_id TEXT,
            last_attempt_at TEXT,
            last_success_at TEXT,
            last_seen_at TEXT,
            PRIMARY KEY (entry_id, provider)
        );

        ",
    )?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Write path
// ---------------------------------------------------------------------------

fn replace_library_state(transaction: &Transaction<'_>, state: &LibraryState) -> Result<()> {
    clear_library_state(transaction)?;

    transaction.execute(
        "INSERT OR REPLACE INTO library_metadata (key, value) VALUES (?1, ?2)",
        params!["schema_version", state.format_version.to_string()],
    )?;
    transaction.execute(
        "INSERT OR REPLACE INTO library_metadata (key, value) VALUES (?1, ?2)",
        params!["created_at", encode_datetime(&state.created_at)],
    )?;
    transaction.execute(
        "INSERT OR REPLACE INTO library_metadata (key, value) VALUES (?1, ?2)",
        params!["updated_at", encode_datetime(&state.updated_at)],
    )?;

    for track in &state.tracks {
        transaction.execute(
            "INSERT INTO tracks (id, title, album, duration_seconds, isrc) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                track.id,
                track.metadata.title,
                track.metadata.album,
                track.metadata.duration_seconds.map(i64::from),
                track.metadata.isrc,
            ],
        )?;

        for (position, artist) in track.metadata.artists.iter().enumerate() {
            transaction.execute(
                "INSERT INTO track_artists (track_id, position, name) VALUES (?1, ?2, ?3)",
                params![track.id, position as i64, artist],
            )?;
        }

        for (provider, link) in &track.provider_links {
            transaction.execute(
                "INSERT INTO track_provider_links
                    (track_id, provider, provider_id, source, confidence, linked_at, last_seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    track.id,
                    provider,
                    link.provider_id,
                    link.source.as_str(),
                    link.confidence,
                    encode_datetime(&link.linked_at),
                    encode_datetime_option(link.last_seen_at.as_ref()),
                ],
            )?;
        }

        for (provider, artwork) in &track.provider_artwork {
            transaction.execute(
                "INSERT INTO track_provider_artwork
                    (track_id, provider, url, width, height, last_seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    track.id,
                    provider,
                    artwork.url,
                    artwork.width.map(i64::from),
                    artwork.height.map(i64::from),
                    encode_datetime_option(artwork.last_seen_at.as_ref()),
                ],
            )?;
        }

        insert_status_map(
            transaction,
            "track_provider_status",
            "track_id",
            &track.id,
            &track.provider_state,
        )?;

        for conflict in &track.identity_conflicts {
            transaction.execute(
                "INSERT INTO track_identity_conflicts
                    (track_id, provider, candidate_provider_id, confidence, detected_at, status, rejected_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    track.id,
                    conflict.provider.as_key(),
                    conflict.candidate_provider_id,
                    conflict.confidence,
                    encode_datetime(&conflict.detected_at),
                    conflict.status.as_str(),
                    encode_datetime_option(conflict.rejected_at.as_ref()),
                ],
            )?;
        }
    }

    for saved_track in &state.saved_tracks {
        transaction.execute(
            "INSERT INTO saved_tracks (id, track_id, added_at) VALUES (?1, ?2, ?3)",
            params![saved_track.id, saved_track.track_id, saved_track.added_at],
        )?;

        insert_status_map(
            transaction,
            "saved_track_provider_status",
            "saved_track_id",
            &saved_track.id,
            &saved_track.provider_state,
        )?;
    }

    for playlist in &state.playlists {
        transaction.execute(
            "INSERT INTO playlists (id, name, description) VALUES (?1, ?2, ?3)",
            params![playlist.id, playlist.name, playlist.description],
        )?;

        for (provider, link) in &playlist.provider_links {
            transaction.execute(
                "INSERT INTO playlist_provider_links
                    (playlist_id, provider, provider_id, source, confidence, linked_at, last_seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    playlist.id,
                    provider,
                    link.provider_id,
                    link.source.as_str(),
                    link.confidence,
                    encode_datetime(&link.linked_at),
                    encode_datetime_option(link.last_seen_at.as_ref()),
                ],
            )?;
        }

        insert_status_map(
            transaction,
            "playlist_provider_status",
            "playlist_id",
            &playlist.id,
            &playlist.provider_state,
        )?;

        for (position, entry) in playlist.entries.iter().enumerate() {
            transaction.execute(
                "INSERT INTO playlist_entries (id, playlist_id, position, track_id, added_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    entry.id,
                    playlist.id,
                    position as i64,
                    entry.track_id,
                    entry.added_at,
                ],
            )?;

            insert_status_map(
                transaction,
                "playlist_entry_provider_status",
                "entry_id",
                &entry.id,
                &entry.provider_state,
            )?;
        }
    }

    Ok(())
}

fn insert_status_map(
    transaction: &Transaction<'_>,
    table: &str,
    owner_column: &str,
    owner_id: &str,
    statuses: &BTreeMap<String, SyncStatusRecord>,
) -> Result<()> {
    let sql = format!(
        "INSERT INTO {table}
            ({owner_column}, provider, state, message, confidence, provider_item_id, last_attempt_at, last_success_at, last_seen_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
    );

    for (provider, status) in statuses {
        transaction.execute(
            &sql,
            params![
                owner_id,
                provider,
                status.state.as_str(),
                status.message,
                status.confidence,
                status.provider_item_id,
                encode_datetime_option(status.last_attempt_at.as_ref()),
                encode_datetime_option(status.last_success_at.as_ref()),
                encode_datetime_option(status.last_seen_at.as_ref()),
            ],
        )?;
    }

    Ok(())
}

fn clear_library_state(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute("DELETE FROM playlist_entry_provider_status", [])?;
    transaction.execute("DELETE FROM playlist_entries", [])?;
    transaction.execute("DELETE FROM playlist_provider_status", [])?;
    transaction.execute("DELETE FROM playlist_provider_links", [])?;
    transaction.execute("DELETE FROM playlists", [])?;
    transaction.execute("DELETE FROM saved_track_provider_status", [])?;
    transaction.execute("DELETE FROM saved_tracks", [])?;
    transaction.execute("DELETE FROM track_identity_conflicts", [])?;
    transaction.execute("DELETE FROM track_provider_status", [])?;
    transaction.execute("DELETE FROM track_provider_artwork", [])?;
    transaction.execute("DELETE FROM track_provider_links", [])?;
    transaction.execute("DELETE FROM track_artists", [])?;
    transaction.execute("DELETE FROM tracks", [])?;
    transaction.execute("DELETE FROM library_metadata", [])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Read path
// ---------------------------------------------------------------------------

pub(super) fn load_library_state(connection: &Connection) -> Result<LibraryState> {
    let schema_version = read_metadata(connection, "schema_version")?
        .unwrap_or_else(|| LIBRARY_STATE_FORMAT_VERSION.to_string())
        .parse::<u32>()
        .context("Failed to parse schema version")?;
    let created_at = read_metadata(connection, "created_at")?
        .map(|value| parse_datetime(&value))
        .transpose()?
        .unwrap_or_else(Utc::now);
    let updated_at = read_metadata(connection, "updated_at")?
        .map(|value| parse_datetime(&value))
        .transpose()?
        .unwrap_or_else(Utc::now);

    let mut tracks = load_tracks(connection)?;
    let saved_tracks = load_saved_tracks(connection)?;
    let playlists = load_playlists(connection)?;

    tracks.sort_by(|left, right| left.id.cmp(&right.id));

    let mut state = LibraryState {
        format_version: schema_version,
        created_at,
        updated_at,
        tracks,
        saved_tracks,
        playlists,
    };
    if state.format_version < LIBRARY_STATE_FORMAT_VERSION {
        state.format_version = LIBRARY_STATE_FORMAT_VERSION;
    }
    state.validate()?;
    Ok(state)
}

fn load_tracks(connection: &Connection) -> Result<Vec<TrackEntity>> {
    let mut tracks = Vec::new();
    let mut statement = connection
        .prepare("SELECT id, title, album, duration_seconds, isrc FROM tracks ORDER BY rowid")?;
    let rows = statement.query_map([], |row| {
        Ok(TrackEntity {
            id: row.get(0)?,
            metadata: TrackMetadata {
                title: row.get(1)?,
                artists: Vec::new(),
                album: row.get(2)?,
                duration_seconds: row.get::<_, Option<i64>>(3)?.map(|value| value as u32),
                isrc: row.get(4)?,
            },
            provider_links: BTreeMap::new(),
            provider_artwork: BTreeMap::new(),
            provider_state: BTreeMap::new(),
            identity_conflicts: Vec::new(),
        })
    })?;

    for row in rows {
        let mut track = row?;
        track.metadata.artists = load_track_artists(connection, &track.id)?;
        track.provider_links = load_track_provider_links(connection, &track.id)?;
        track.provider_artwork = load_track_provider_artwork(connection, &track.id)?;
        track.provider_state =
            load_status_map(connection, "track_provider_status", "track_id", &track.id)?;
        track.identity_conflicts = load_track_identity_conflicts(connection, &track.id)?;
        tracks.push(track);
    }

    Ok(tracks)
}

fn load_track_identity_conflicts(
    connection: &Connection,
    track_id: &str,
) -> Result<Vec<TrackIdentityConflict>> {
    let mut statement = connection.prepare(
        "SELECT provider, candidate_provider_id, confidence, detected_at, status, rejected_at
         FROM track_identity_conflicts
         WHERE track_id = ?1
         ORDER BY provider, candidate_provider_id",
    )?;
    let rows = statement.query_map(params![track_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<f64>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })?;

    let mut conflicts = Vec::new();
    for row in rows {
        let (provider, candidate_provider_id, confidence, detected_at, status, rejected_at) = row?;
        // Drop rows naming an unknown provider kind rather than failing the
        // whole load; the typed model only represents providers the app knows.
        let Ok(provider) = ProviderKind::from_key(&provider) else {
            continue;
        };
        conflicts.push(TrackIdentityConflict {
            provider,
            candidate_provider_id,
            confidence,
            detected_at: parse_datetime(&detected_at)?,
            status: status.parse()?,
            rejected_at: rejected_at
                .map(|value| parse_datetime(&value))
                .transpose()?,
        });
    }
    Ok(conflicts)
}

fn load_track_artists(connection: &Connection, track_id: &str) -> Result<Vec<String>> {
    let mut statement = connection
        .prepare("SELECT name FROM track_artists WHERE track_id = ?1 ORDER BY position")?;
    let rows = statement.query_map(params![track_id], |row| row.get(0))?;
    let mut artists = Vec::new();
    for row in rows {
        artists.push(row?);
    }
    Ok(artists)
}

fn load_track_provider_links(
    connection: &Connection,
    track_id: &str,
) -> Result<BTreeMap<String, ProviderTrackLink>> {
    let mut statement = connection.prepare(
        "SELECT provider, provider_id, source, confidence, linked_at, last_seen_at
         FROM track_provider_links
         WHERE track_id = ?1
         ORDER BY provider",
    )?;
    let rows = statement.query_map(params![track_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<f64>>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })?;

    let mut links = BTreeMap::new();
    for row in rows {
        let (provider, provider_id, source, confidence, linked_at, last_seen_at) = row?;
        links.insert(
            provider,
            ProviderTrackLink {
                provider_id,
                source: source.parse()?,
                confidence,
                linked_at: parse_datetime(&linked_at)?,
                last_seen_at: last_seen_at
                    .map(|value| parse_datetime(&value))
                    .transpose()?,
            },
        );
    }
    Ok(links)
}

fn load_track_provider_artwork(
    connection: &Connection,
    track_id: &str,
) -> Result<BTreeMap<String, ProviderTrackArtwork>> {
    let mut statement = connection.prepare(
        "SELECT provider, url, width, height, last_seen_at
         FROM track_provider_artwork
         WHERE track_id = ?1
         ORDER BY provider",
    )?;
    let rows = statement.query_map(params![track_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;

    let mut artwork = BTreeMap::new();
    for row in rows {
        let (provider, url, width, height, last_seen_at) = row?;
        artwork.insert(
            provider,
            ProviderTrackArtwork {
                url,
                width: width.map(|value| value as u32),
                height: height.map(|value| value as u32),
                last_seen_at: last_seen_at
                    .map(|value| parse_datetime(&value))
                    .transpose()?,
            },
        );
    }
    Ok(artwork)
}

fn load_saved_tracks(connection: &Connection) -> Result<Vec<SavedTrackEntry>> {
    let mut statement =
        connection.prepare("SELECT id, track_id, added_at FROM saved_tracks ORDER BY rowid")?;
    let rows = statement.query_map([], |row| {
        Ok(SavedTrackEntry {
            id: row.get(0)?,
            track_id: row.get(1)?,
            added_at: row.get(2)?,
            provider_state: BTreeMap::new(),
        })
    })?;

    let mut saved_tracks = Vec::new();
    for row in rows {
        let mut saved_track = row?;
        saved_track.provider_state = load_status_map(
            connection,
            "saved_track_provider_status",
            "saved_track_id",
            &saved_track.id,
        )?;
        saved_tracks.push(saved_track);
    }
    Ok(saved_tracks)
}

fn load_playlists(connection: &Connection) -> Result<Vec<PlaylistEntity>> {
    let mut statement =
        connection.prepare("SELECT id, name, description FROM playlists ORDER BY rowid")?;
    let rows = statement.query_map([], |row| {
        Ok(PlaylistEntity {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            provider_links: BTreeMap::new(),
            provider_state: BTreeMap::new(),
            entries: Vec::new(),
        })
    })?;

    let mut playlists = Vec::new();
    for row in rows {
        let mut playlist = row?;
        playlist.provider_links = load_playlist_provider_links(connection, &playlist.id)?;
        playlist.provider_state = load_status_map(
            connection,
            "playlist_provider_status",
            "playlist_id",
            &playlist.id,
        )?;
        playlist.entries = load_playlist_entries(connection, &playlist.id)?;
        playlists.push(playlist);
    }
    Ok(playlists)
}

fn load_playlist_provider_links(
    connection: &Connection,
    playlist_id: &str,
) -> Result<BTreeMap<String, ProviderPlaylistLink>> {
    let mut statement = connection.prepare(
        "SELECT provider, provider_id, source, confidence, linked_at, last_seen_at
         FROM playlist_provider_links
         WHERE playlist_id = ?1
         ORDER BY provider",
    )?;
    let rows = statement.query_map(params![playlist_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<f64>>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })?;

    let mut links = BTreeMap::new();
    for row in rows {
        let (provider, provider_id, source, confidence, linked_at, last_seen_at) = row?;
        links.insert(
            provider,
            ProviderPlaylistLink {
                provider_id,
                source: source.parse()?,
                confidence,
                linked_at: parse_datetime(&linked_at)?,
                last_seen_at: last_seen_at
                    .map(|value| parse_datetime(&value))
                    .transpose()?,
            },
        );
    }
    Ok(links)
}

fn load_playlist_entries(connection: &Connection, playlist_id: &str) -> Result<Vec<PlaylistEntry>> {
    let mut statement = connection.prepare(
        "SELECT id, track_id, added_at FROM playlist_entries WHERE playlist_id = ?1 ORDER BY position",
    )?;
    let rows = statement.query_map(params![playlist_id], |row| {
        Ok(PlaylistEntry {
            id: row.get(0)?,
            track_id: row.get(1)?,
            added_at: row.get(2)?,
            provider_state: BTreeMap::new(),
        })
    })?;

    let mut entries = Vec::new();
    for row in rows {
        let mut entry = row?;
        entry.provider_state = load_status_map(
            connection,
            "playlist_entry_provider_status",
            "entry_id",
            &entry.id,
        )?;
        entries.push(entry);
    }

    Ok(entries)
}

fn load_status_map(
    connection: &Connection,
    table: &str,
    owner_column: &str,
    owner_id: &str,
) -> Result<BTreeMap<String, SyncStatusRecord>> {
    let sql = format!(
        "SELECT provider, state, message, confidence, provider_item_id, last_attempt_at, last_success_at, last_seen_at
         FROM {table}
         WHERE {owner_column} = ?1
         ORDER BY provider"
    );

    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params![owner_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<f64>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, Option<String>>(7)?,
        ))
    })?;

    let mut statuses = BTreeMap::new();
    for row in rows {
        let (
            provider,
            state,
            message,
            confidence,
            provider_item_id,
            last_attempt_at,
            last_success_at,
            last_seen_at,
        ) = row?;
        statuses.insert(
            provider,
            SyncStatusRecord {
                state: state.parse()?,
                message,
                confidence,
                provider_item_id,
                last_attempt_at: last_attempt_at
                    .map(|value| parse_datetime(&value))
                    .transpose()?,
                last_success_at: last_success_at
                    .map(|value| parse_datetime(&value))
                    .transpose()?,
                last_seen_at: last_seen_at
                    .map(|value| parse_datetime(&value))
                    .transpose()?,
            },
        );
    }
    Ok(statuses)
}
