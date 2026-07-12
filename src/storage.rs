use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use csv::{Reader, StringRecord, Writer};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction};
use serde::Deserialize;
use uuid::Uuid;

use crate::domain::{
    new_canonical_id, IdentityConflictStatus, LibraryState, LinkSource, PlaylistEntity,
    PlaylistEntry, ProviderConnection, ProviderConnectionConfig, ProviderCooldown, ProviderHealth,
    ProviderKind, ProviderPlaylistLink, ProviderTrackArtwork, ProviderTrackLink, SavedTrackEntry,
    SpotifyConnectionConfig, SyncStatusRecord, TrackEntity, TrackIdentityConflict, TrackMetadata,
    YoutubeMusicConnectionConfig, LIBRARY_STATE_FORMAT_VERSION,
};

pub const DUMP_DIR: &str = "dump";
pub const LIBRARY_DB_FILE: &str = "library.db";
pub const RUNTIME_DB_FILE: &str = "runtime.db";
pub const LEGACY_LIBRARY_STATE_FILE: &str = "library.json";
pub const DEFAULT_CSV_EXPORT_DIR: &str = "csv";
pub const BACKUP_DIR: &str = "backups";
pub const MANUAL_BACKUP_DIR: &str = "manual-backups";
pub const DATA_DIR_ENV: &str = "SPOTI_DUMP_DATA_DIR";
const BACKUP_RETENTION: usize = 50;
const OPERATION_RETENTION: usize = 100;

/// Key under which the library database's schema version is stored in the
/// `library_metadata` table.
const SCHEMA_VERSION_KEY: &str = "schema_version";

/// The schema version this build of the app understands. A fresh database is
/// created at this version; an older database is migrated up to it; a newer
/// database is rejected. Kept in lockstep with the in-memory
/// `LIBRARY_STATE_FORMAT_VERSION`.
const CURRENT_SCHEMA_VERSION: u32 = LIBRARY_STATE_FORMAT_VERSION;

/// Version assumed for a database that carries library data but no explicit
/// `schema_version` key. Such databases predate the migration framework, so
/// they are treated as the last pre-framework version and migrated forward.
const LEGACY_BASELINE_SCHEMA_VERSION: u32 = 4;

type MigrationFn = fn(&Transaction<'_>) -> Result<()>;

/// Ordered list of schema migrations. Each entry is
/// `(target_version, description, apply)`; a database at version `v` runs every
/// migration whose `target_version` is greater than `v` (up to
/// `CURRENT_SCHEMA_VERSION`), each inside its own transaction that also bumps
/// the stored version. Append new migrations here; never edit or reorder
/// released ones.
const MIGRATIONS: &[(u32, &str, MigrationFn)] = &[(
    5,
    "Introduce first-class track_identity_conflicts and convert legacy status markers",
    migrate_to_v5,
)];

/// DDL for the typed identity-conflict table. Shared by the fresh-database
/// schema and the v5 migration so both create an identical table.
const CREATE_TRACK_IDENTITY_CONFLICTS_TABLE: &str = "
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
";

#[derive(Clone, Debug)]
pub struct DatabaseHealth {
    pub path: PathBuf,
    pub integrity_check: String,
    pub tracks: usize,
    pub saved_tracks: usize,
    pub playlists: usize,
    pub playlist_entries: usize,
}

#[derive(Clone, Debug)]
pub struct LibraryBackup {
    pub file_name: String,
    pub path: PathBuf,
    pub backup_type: &'static str,
    pub size_bytes: u64,
    pub modified_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct RestoreSummary {
    pub restored_backup: LibraryBackup,
    pub pre_restore_backup: LibraryBackup,
}

pub fn data_root() -> PathBuf {
    env::var_os(DATA_DIR_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let local_root = PathBuf::from(".");
            let current_dir = env::current_dir().unwrap_or_else(|_| local_root.clone());
            discover_default_data_root(&local_root, &current_dir)
        })
}

pub fn library_state_path() -> PathBuf {
    database_path_in(&data_root())
}

pub fn database_path_in(root: &Path) -> PathBuf {
    root.join(DUMP_DIR).join(LIBRARY_DB_FILE)
}

pub fn runtime_database_path_in(root: &Path) -> PathBuf {
    root.join(DUMP_DIR).join(RUNTIME_DB_FILE)
}

pub fn default_csv_export_path_in(root: &Path) -> PathBuf {
    root.join(DUMP_DIR).join(DEFAULT_CSV_EXPORT_DIR)
}

pub fn automatic_backup_dir() -> PathBuf {
    automatic_backup_dir_in(&data_root())
}

pub fn automatic_backup_dir_in(root: &Path) -> PathBuf {
    root.join(DUMP_DIR).join(BACKUP_DIR)
}

pub fn manual_backup_dir() -> PathBuf {
    manual_backup_dir_in(&data_root())
}

pub fn manual_backup_dir_in(root: &Path) -> PathBuf {
    root.join(DUMP_DIR).join(MANUAL_BACKUP_DIR)
}

fn discover_default_data_root(local_root: &Path, current_dir: &Path) -> PathBuf {
    if has_existing_library_data(local_root) {
        return local_root.to_path_buf();
    }

    if let Some(parent) = current_dir.parent() {
        if has_existing_library_data(parent) {
            return parent.to_path_buf();
        }
    }

    local_root.to_path_buf()
}

fn has_existing_library_data(root: &Path) -> bool {
    database_path_in(root).exists()
        || legacy_library_state_path_in(root).exists()
        || has_legacy_csv_dump(root)
}

fn has_legacy_csv_dump(root: &Path) -> bool {
    let dump_dir = root.join(DUMP_DIR);
    let Ok(entries) = fs::read_dir(dump_dir) else {
        return false;
    };

    entries.filter_map(|entry| entry.ok()).any(|entry| {
        entry
            .path()
            .extension()
            .and_then(|extension| extension.to_str())
            == Some("csv")
    })
}

pub fn write_library_state(state: &LibraryState) -> Result<PathBuf> {
    write_library_state_in(&data_root(), state)
}

pub fn write_library_state_in(root: &Path, state: &LibraryState) -> Result<PathBuf> {
    state.validate()?;
    ensure_dump_dir(root)?;

    let database_path = database_path_in(root);
    // Rolling pre-write backup of the existing database, independent of any
    // schema migration snapshot below.
    snapshot_existing_database(root, &database_path)?;
    let mut connection = open_database(&database_path)?;
    prepare_library_database(root, &database_path, &mut connection)?;

    let transaction = connection.transaction()?;
    replace_library_state(&transaction, state)?;
    transaction.commit()?;

    Ok(database_path)
}

pub fn read_library_state() -> Result<LibraryState> {
    read_library_state_in(&data_root(), false)
}

pub fn read_library_state_or_new() -> Result<LibraryState> {
    read_library_state_in(&data_root(), true)
}

pub fn read_library_state_in(root: &Path, allow_empty: bool) -> Result<LibraryState> {
    let database_path = database_path_in(root);
    if database_path.exists() {
        let mut connection = open_database(&database_path)?;
        prepare_library_database(root, &database_path, &mut connection)?;
        return load_library_state(&connection);
    }

    if let Some(state) = read_legacy_json_state(root)? {
        write_library_state_in(root, &state)?;
        return Ok(state);
    }

    if legacy_dump_exists(root)? {
        let state = read_legacy_csv_state(root)?;
        write_library_state_in(root, &state)?;
        return Ok(state);
    }

    if allow_empty {
        Ok(LibraryState::new())
    } else {
        anyhow::bail!(
            "No library database found. Expected {} or legacy data in {}.",
            database_path.display(),
            root.join(DUMP_DIR).display()
        )
    }
}

pub fn export_csv(root: &Path, state: &LibraryState, output_dir: Option<&Path>) -> Result<PathBuf> {
    let output_dir = output_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_csv_export_path_in(root));

    if output_dir.exists() {
        for entry in fs::read_dir(&output_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("csv") {
                fs::remove_file(&path)
                    .with_context(|| format!("Failed to remove stale {}", path.display()))?;
            }
        }
    } else {
        fs::create_dir_all(&output_dir)
            .with_context(|| format!("Failed to create {}", output_dir.display()))?;
    }

    write_metadata_csv(&output_dir, state)?;
    write_tracks_csv(&output_dir, state)?;
    write_track_artists_csv(&output_dir, state)?;
    write_track_provider_links_csv(&output_dir, state)?;
    write_track_provider_artwork_csv(&output_dir, state)?;
    write_track_provider_status_csv(&output_dir, state)?;
    write_track_identity_conflicts_csv(&output_dir, state)?;
    write_saved_tracks_csv(&output_dir, state)?;
    write_saved_track_provider_status_csv(&output_dir, state)?;
    write_playlists_csv(&output_dir, state)?;
    write_playlist_provider_links_csv(&output_dir, state)?;
    write_playlist_provider_status_csv(&output_dir, state)?;
    write_playlist_entries_csv(&output_dir, state)?;
    write_playlist_entry_provider_status_csv(&output_dir, state)?;

    Ok(output_dir)
}

pub fn create_manual_library_backup() -> Result<LibraryBackup> {
    create_manual_library_backup_in(&data_root())
}

pub fn create_manual_library_backup_in(root: &Path) -> Result<LibraryBackup> {
    ensure_dump_dir(root)?;
    let database_path = database_path_in(root);
    if !database_path.exists() {
        anyhow::bail!(
            "No canonical library database exists at {}.",
            database_path.display()
        );
    }

    let backup_dir = manual_backup_dir_in(root);
    fs::create_dir_all(&backup_dir)
        .with_context(|| format!("Failed to create {}", backup_dir.display()))?;
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.9fZ");
    let backup_path = backup_dir.join(format!("manual-library-{timestamp}-{}.db", Uuid::new_v4()));
    fs::copy(&database_path, &backup_path).with_context(|| {
        format!(
            "Failed to copy {} to {}",
            database_path.display(),
            backup_path.display()
        )
    })?;
    backup_record_from_path(backup_path, "manual")
}

pub fn list_library_backups() -> Result<Vec<LibraryBackup>> {
    list_library_backups_in(&data_root())
}

pub fn list_library_backups_in(root: &Path) -> Result<Vec<LibraryBackup>> {
    let mut backups = Vec::new();
    backups.extend(list_backups_from_dir(
        automatic_backup_dir_in(root),
        "automatic",
        "library-",
    )?);
    backups.extend(list_backups_from_dir(
        manual_backup_dir_in(root),
        "manual",
        "manual-library-",
    )?);
    backups.sort_by(|left, right| {
        right
            .modified_at
            .cmp(&left.modified_at)
            .then_with(|| right.file_name.cmp(&left.file_name))
    });
    Ok(backups)
}

pub fn restore_library_backup(backup_type: &str, file_name: &str) -> Result<RestoreSummary> {
    restore_library_backup_in(&data_root(), backup_type, file_name)
}

pub fn restore_library_backup_in(
    root: &Path,
    backup_type: &str,
    file_name: &str,
) -> Result<RestoreSummary> {
    ensure_dump_dir(root)?;
    let backup_path = resolve_backup_path(root, backup_type, file_name)?;
    validate_backup_database(&backup_path)?;
    let restored_state = load_restorable_backup_state(root, &backup_path)?;

    let pre_restore_backup = create_manual_library_backup_in(root)
        .context("Failed to create pre-restore manual backup")?;
    write_library_state_in(root, &restored_state).with_context(|| {
        format!(
            "Failed to restore validated backup {} into {}",
            backup_path.display(),
            database_path_in(root).display()
        )
    })?;

    let restored_backup = backup_record_from_path(backup_path, backup_type_label(backup_type)?)?;
    Ok(RestoreSummary {
        restored_backup,
        pre_restore_backup,
    })
}

pub fn list_provider_connections() -> Result<Vec<ProviderConnection>> {
    list_provider_connections_in(&data_root())
}

pub fn list_provider_connections_in(root: &Path) -> Result<Vec<ProviderConnection>> {
    let connection = open_runtime_database(root)?;
    load_provider_connections(&connection)
}

pub fn read_provider_connection(provider: ProviderKind) -> Result<Option<ProviderConnection>> {
    Ok(list_provider_connections()?
        .into_iter()
        .find(|connection| connection.provider == provider))
}

pub fn save_provider_connection(connection: &ProviderConnection) -> Result<()> {
    save_provider_connection_in(&data_root(), connection)
}

pub fn save_provider_connection_in(root: &Path, connection: &ProviderConnection) -> Result<()> {
    let database = open_runtime_database(root)?;

    database.execute(
        "INSERT OR REPLACE INTO provider_connections
            (provider, config_json, connected_at, updated_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            connection.provider.as_key(),
            serde_json::to_string(&connection.config)?,
            encode_datetime(&connection.connected_at),
            encode_datetime(&connection.updated_at),
        ],
    )?;

    Ok(())
}

pub fn delete_provider_connection(provider: ProviderKind) -> Result<()> {
    delete_provider_connection_in(&data_root(), provider)
}

pub fn delete_provider_connection_in(root: &Path, provider: ProviderKind) -> Result<()> {
    let database = open_runtime_database(root)?;
    database.execute(
        "DELETE FROM provider_connections WHERE provider = ?1",
        params![provider.as_key()],
    )?;
    database.execute(
        "DELETE FROM provider_cooldowns WHERE provider = ?1",
        params![provider.as_key()],
    )?;
    database.execute(
        "DELETE FROM provider_health WHERE provider = ?1",
        params![provider.as_key()],
    )?;
    Ok(())
}

pub fn list_provider_healths() -> Result<Vec<ProviderHealth>> {
    list_provider_healths_in(&data_root())
}

pub fn list_provider_healths_in(root: &Path) -> Result<Vec<ProviderHealth>> {
    let database = open_runtime_database(root)?;
    load_provider_healths(&database)
}

pub fn read_provider_health(provider: ProviderKind) -> Result<Option<ProviderHealth>> {
    read_provider_health_in(&data_root(), provider)
}

pub fn read_provider_health_in(
    root: &Path,
    provider: ProviderKind,
) -> Result<Option<ProviderHealth>> {
    let database = open_runtime_database(root)?;
    load_provider_health(&database, provider)
}

pub fn save_provider_health(health: &ProviderHealth) -> Result<()> {
    save_provider_health_in(&data_root(), health)
}

pub fn save_provider_health_in(root: &Path, health: &ProviderHealth) -> Result<()> {
    let database = open_runtime_database(root)?;
    database.execute(
        "INSERT OR REPLACE INTO provider_health
            (provider, checked_at, ok, message)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            health.provider.as_key(),
            encode_datetime(&health.checked_at),
            if health.ok { 1_i64 } else { 0_i64 },
            health.message.as_deref(),
        ],
    )?;
    Ok(())
}

pub fn clear_provider_health(provider: ProviderKind) -> Result<()> {
    clear_provider_health_in(&data_root(), provider)
}

pub fn clear_provider_health_in(root: &Path, provider: ProviderKind) -> Result<()> {
    let database = open_runtime_database(root)?;
    database.execute(
        "DELETE FROM provider_health WHERE provider = ?1",
        params![provider.as_key()],
    )?;
    Ok(())
}

pub fn list_provider_cooldowns() -> Result<Vec<ProviderCooldown>> {
    list_provider_cooldowns_in(&data_root())
}

pub fn list_provider_cooldowns_in(root: &Path) -> Result<Vec<ProviderCooldown>> {
    let database = open_runtime_database(root)?;
    clear_expired_provider_cooldowns(&database)?;
    load_provider_cooldowns(&database)
}

pub fn read_provider_cooldown(provider: ProviderKind) -> Result<Option<ProviderCooldown>> {
    read_provider_cooldown_in(&data_root(), provider)
}

pub fn read_provider_cooldown_in(
    root: &Path,
    provider: ProviderKind,
) -> Result<Option<ProviderCooldown>> {
    let database = open_runtime_database(root)?;
    clear_expired_provider_cooldowns(&database)?;
    load_provider_cooldown(&database, provider)
}

pub fn save_provider_cooldown(cooldown: &ProviderCooldown) -> Result<()> {
    save_provider_cooldown_in(&data_root(), cooldown)
}

pub fn save_provider_cooldown_in(root: &Path, cooldown: &ProviderCooldown) -> Result<()> {
    let database = open_runtime_database(root)?;
    database.execute(
        "INSERT OR REPLACE INTO provider_cooldowns
            (provider, blocked_until, reason, updated_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            cooldown.provider.as_key(),
            encode_datetime(&cooldown.blocked_until),
            cooldown.reason,
            encode_datetime(&cooldown.updated_at),
        ],
    )?;
    Ok(())
}

pub fn clear_provider_cooldown(provider: ProviderKind) -> Result<()> {
    clear_provider_cooldown_in(&data_root(), provider)
}

pub fn clear_provider_cooldown_in(root: &Path, provider: ProviderKind) -> Result<()> {
    let database = open_runtime_database(root)?;
    database.execute(
        "DELETE FROM provider_cooldowns WHERE provider = ?1",
        params![provider.as_key()],
    )?;
    Ok(())
}

pub fn save_ui_operation_json(operation_id: &str, status: &str, payload_json: &str) -> Result<()> {
    save_ui_operation_json_in(&data_root(), operation_id, status, payload_json)
}

pub fn save_ui_operation_json_in(
    root: &Path,
    operation_id: &str,
    status: &str,
    payload_json: &str,
) -> Result<()> {
    let database = open_runtime_database(root)?;
    let now = encode_datetime(&Utc::now());
    database.execute(
        "INSERT INTO ui_operations (id, status, payload_json, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?4)
         ON CONFLICT(id) DO UPDATE SET
            status = excluded.status,
            payload_json = excluded.payload_json,
            updated_at = excluded.updated_at",
        params![operation_id, status, payload_json, now],
    )?;
    database.execute(
        "DELETE FROM ui_operations
         WHERE id NOT IN (
            SELECT id FROM ui_operations ORDER BY updated_at DESC LIMIT ?1
         )",
        params![OPERATION_RETENTION as i64],
    )?;
    Ok(())
}

pub fn read_ui_operation_json(operation_id: &str) -> Result<Option<String>> {
    read_ui_operation_json_in(&data_root(), operation_id)
}

pub fn read_ui_operation_json_in(root: &Path, operation_id: &str) -> Result<Option<String>> {
    let database_path = runtime_database_path_in(root);
    if !database_path.exists() && !database_path_in(root).exists() {
        return Ok(None);
    }
    let database = open_runtime_database(root)?;
    database
        .query_row(
            "SELECT payload_json FROM ui_operations WHERE id = ?1",
            params![operation_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

pub fn list_ui_operation_json() -> Result<Vec<String>> {
    list_ui_operation_json_in(&data_root())
}

pub fn list_ui_operation_json_in(root: &Path) -> Result<Vec<String>> {
    let database_path = runtime_database_path_in(root);
    if !database_path.exists() && !database_path_in(root).exists() {
        return Ok(Vec::new());
    }
    let database = open_runtime_database(root)?;
    let mut statement =
        database.prepare("SELECT payload_json FROM ui_operations ORDER BY updated_at DESC")?;
    let rows = statement.query_map([], |row| row.get(0))?;
    let mut operations = Vec::new();
    for row in rows {
        operations.push(row?);
    }
    Ok(operations)
}

pub fn database_health() -> Result<DatabaseHealth> {
    database_health_in(&data_root())
}

pub fn database_health_in(root: &Path) -> Result<DatabaseHealth> {
    let path = database_path_in(root);
    let mut database = open_database(&path)?;
    prepare_library_database(root, &path, &mut database)?;
    Ok(DatabaseHealth {
        path,
        integrity_check: database.query_row("PRAGMA integrity_check", [], |row| row.get(0))?,
        tracks: count_table_rows(&database, "tracks")? as usize,
        saved_tracks: count_table_rows(&database, "saved_tracks")? as usize,
        playlists: count_table_rows(&database, "playlists")? as usize,
        playlist_entries: count_table_rows(&database, "playlist_entries")? as usize,
    })
}

fn ensure_dump_dir(root: &Path) -> Result<PathBuf> {
    let dump_dir = root.join(DUMP_DIR);
    if !dump_dir.exists() {
        fs::create_dir_all(&dump_dir)
            .with_context(|| format!("Failed to create {}", dump_dir.display()))?;
    }
    Ok(dump_dir)
}

fn legacy_library_state_path_in(root: &Path) -> PathBuf {
    root.join(DUMP_DIR).join(LEGACY_LIBRARY_STATE_FILE)
}

fn open_database(database_path: &Path) -> Result<Connection> {
    let connection = Connection::open(database_path)
        .with_context(|| format!("Failed to open {}", database_path.display()))?;
    connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")?;
    Ok(connection)
}

fn open_runtime_database(root: &Path) -> Result<Connection> {
    ensure_dump_dir(root)?;
    let runtime_path = runtime_database_path_in(root);
    let database = open_database(&runtime_path)?;
    harden_runtime_file_permissions(&runtime_path)?;
    initialize_runtime_schema(&database)?;
    migrate_legacy_runtime_state(root, &database)?;
    Ok(database)
}

#[cfg(unix)]
fn harden_runtime_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn harden_runtime_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn snapshot_existing_database(root: &Path, database_path: &Path) -> Result<Option<PathBuf>> {
    if !database_path.exists() {
        return Ok(None);
    }

    let backup_dir = root.join(DUMP_DIR).join(BACKUP_DIR);
    fs::create_dir_all(&backup_dir)
        .with_context(|| format!("Failed to create {}", backup_dir.display()))?;
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.9fZ");
    let backup_path = backup_dir.join(format!("library-{timestamp}-{}.db", Uuid::new_v4()));
    fs::copy(database_path, &backup_path).with_context(|| {
        format!(
            "Failed to snapshot {} to {}",
            database_path.display(),
            backup_path.display()
        )
    })?;
    prune_old_database_backups(&backup_dir)?;
    Ok(Some(backup_path))
}

fn prune_old_database_backups(backup_dir: &Path) -> Result<()> {
    let mut backups = fs::read_dir(backup_dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("library-") && name.ends_with(".db"))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    backups.sort();

    let excess = backups.len().saturating_sub(BACKUP_RETENTION);
    for backup in backups.into_iter().take(excess) {
        fs::remove_file(&backup)
            .with_context(|| format!("Failed to prune old backup {}", backup.display()))?;
    }
    Ok(())
}

fn list_backups_from_dir(
    backup_dir: PathBuf,
    backup_type: &'static str,
    file_prefix: &str,
) -> Result<Vec<LibraryBackup>> {
    if !backup_dir.exists() {
        return Ok(Vec::new());
    }

    let mut backups = Vec::new();
    for entry in fs::read_dir(&backup_dir)
        .with_context(|| format!("Failed to read {}", backup_dir.display()))?
    {
        let path = entry?.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !file_name.starts_with(file_prefix) || !file_name.ends_with(".db") {
            continue;
        }
        backups.push(backup_record_from_path(path, backup_type)?);
    }
    Ok(backups)
}

fn backup_record_from_path(path: PathBuf, backup_type: &'static str) -> Result<LibraryBackup> {
    let metadata = fs::metadata(&path)
        .with_context(|| format!("Failed to inspect backup {}", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .with_context(|| format!("Backup path {} has no file name", path.display()))?;
    let modified_at = metadata.modified().ok().map(DateTime::<Utc>::from);

    Ok(LibraryBackup {
        file_name,
        path,
        backup_type,
        size_bytes: metadata.len(),
        modified_at,
    })
}

fn resolve_backup_path(root: &Path, backup_type: &str, file_name: &str) -> Result<PathBuf> {
    validate_backup_file_name(file_name)?;
    let (backup_dir, expected_prefix) = match backup_type {
        "automatic" => (automatic_backup_dir_in(root), "library-"),
        "manual" => (manual_backup_dir_in(root), "manual-library-"),
        _ => anyhow::bail!("Unsupported backup type '{backup_type}'."),
    };
    if !file_name.starts_with(expected_prefix) || !file_name.ends_with(".db") {
        anyhow::bail!("Backup file name '{file_name}' does not match type '{backup_type}'.");
    }

    let backup_path = backup_dir.join(file_name);
    let metadata = fs::symlink_metadata(&backup_path)
        .with_context(|| format!("Failed to inspect backup {}", backup_path.display()))?;
    if !metadata.file_type().is_file() {
        anyhow::bail!("Backup {} is not a regular file.", backup_path.display());
    }

    let canonical_dir = fs::canonicalize(&backup_dir)
        .with_context(|| format!("Failed to resolve {}", backup_dir.display()))?;
    let canonical_path = fs::canonicalize(&backup_path)
        .with_context(|| format!("Failed to resolve {}", backup_path.display()))?;
    if !canonical_path.starts_with(canonical_dir) {
        anyhow::bail!(
            "Backup {} resolves outside the managed backup directory.",
            backup_path.display()
        );
    }

    Ok(backup_path)
}

fn validate_backup_file_name(file_name: &str) -> Result<()> {
    if file_name.trim().is_empty()
        || file_name.contains('/')
        || file_name.contains('\\')
        || file_name == "."
        || file_name == ".."
        || Path::new(file_name)
            .file_name()
            .and_then(|name| name.to_str())
            != Some(file_name)
    {
        anyhow::bail!("Invalid backup file name '{file_name}'.");
    }
    Ok(())
}

fn backup_type_label(backup_type: &str) -> Result<&'static str> {
    match backup_type {
        "automatic" => Ok("automatic"),
        "manual" => Ok("manual"),
        _ => anyhow::bail!("Unsupported backup type '{backup_type}'."),
    }
}

fn validate_backup_database(path: &Path) -> Result<()> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("Failed to open backup {}", path.display()))?;
    let integrity_check: String =
        connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity_check != "ok" {
        anyhow::bail!(
            "Backup {} failed integrity check: {}",
            path.display(),
            integrity_check
        );
    }
    Ok(())
}

fn load_restorable_backup_state(root: &Path, backup_path: &Path) -> Result<LibraryState> {
    let dump_dir = ensure_dump_dir(root)?;
    let staging_path = dump_dir.join(format!(".restore-staging-{}.db", Uuid::new_v4()));
    fs::copy(backup_path, &staging_path).with_context(|| {
        format!(
            "Failed to stage backup {} at {}",
            backup_path.display(),
            staging_path.display()
        )
    })?;

    let result = (|| {
        let mut connection = open_database(&staging_path)?;
        prepare_staging_database(&mut connection, &staging_path)?;
        load_library_state(&connection)
    })();
    let cleanup_result = fs::remove_file(&staging_path);

    match (result, cleanup_result) {
        (Ok(state), Ok(())) => Ok(state),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error).with_context(|| {
            format!(
                "Failed to remove staged restore database {}",
                staging_path.display()
            )
        }),
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup_error)) => Err(error).context(format!(
            "Additionally failed to remove staged restore database {}: {cleanup_error}",
            staging_path.display()
        )),
    }
}

fn initialize_schema(connection: &Connection) -> Result<()> {
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

/// Brings an opened library database to `CURRENT_SCHEMA_VERSION`.
///
/// * A brand-new/empty database (no `library_metadata` table) is created at the
///   current schema directly, with no migrations run.
/// * A database already at the current version is left as-is.
/// * An older database is snapshotted (reusing the standard backup helper) and
///   then each pending migration is applied in its own transaction, bumping the
///   stored version as it goes.
/// * A newer database is a hard error, telling the user to upgrade the app.
fn prepare_library_database(
    root: &Path,
    database_path: &Path,
    connection: &mut Connection,
) -> Result<()> {
    prepare_opened_database(connection, database_path, Some((root, database_path)))
}

/// Like [`prepare_library_database`] but never records a pre-migration
/// snapshot. Used for throwaway staging copies (backup restore), where the
/// source data is already a backup and an extra snapshot would only clutter the
/// managed backup directory. The migration itself still runs so old backups are
/// converted to the current schema before they are read.
fn prepare_staging_database(connection: &mut Connection, database_path: &Path) -> Result<()> {
    prepare_opened_database(connection, database_path, None)
}

fn prepare_opened_database(
    connection: &mut Connection,
    database_path: &Path,
    snapshot_into: Option<(&Path, &Path)>,
) -> Result<()> {
    if !schema_table_exists(connection, "library_metadata")? {
        initialize_schema(connection)?;
        write_schema_version(connection, CURRENT_SCHEMA_VERSION)?;
        return Ok(());
    }

    let db_version = read_schema_version(connection)?.unwrap_or(LEGACY_BASELINE_SCHEMA_VERSION);
    if db_version > CURRENT_SCHEMA_VERSION {
        anyhow::bail!(
            "Library database at {} uses schema version {} which is newer than this app supports (version {}). Upgrade spoti-dump to open it.",
            database_path.display(),
            db_version,
            CURRENT_SCHEMA_VERSION
        );
    }

    if db_version == CURRENT_SCHEMA_VERSION {
        return Ok(());
    }

    if let Some((root, path)) = snapshot_into {
        snapshot_existing_database(root, path)?;
    }
    apply_pending_migrations(connection, db_version)?;
    Ok(())
}

/// Applies every migration whose target version is greater than `from_version`
/// (and at most `CURRENT_SCHEMA_VERSION`), each in its own transaction that also
/// records the new version.
fn apply_pending_migrations(connection: &mut Connection, from_version: u32) -> Result<()> {
    for (version, description, migrate) in MIGRATIONS {
        if *version <= from_version || *version > CURRENT_SCHEMA_VERSION {
            continue;
        }
        let transaction = connection.transaction()?;
        migrate(&transaction).with_context(|| {
            format!("Failed to apply schema migration to version {version} ({description})")
        })?;
        write_schema_version_tx(&transaction, *version)?;
        transaction.commit()?;
    }
    Ok(())
}

/// Migration to schema v5: create the typed `track_identity_conflicts` table
/// and convert legacy message-encoded conflicts stored in `track_provider_status`
/// into typed rows. This is the ONLY place the retired message conventions are
/// parsed. The originating status rows are kept as display-only history.
fn migrate_to_v5(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute_batch(CREATE_TRACK_IDENTITY_CONFLICTS_TABLE)?;

    let legacy_rows = {
        let mut statement = transaction.prepare(
            "SELECT track_id, provider, state, message, confidence, provider_item_id, last_attempt_at
             FROM track_provider_status",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<f64>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;
        let mut collected = Vec::new();
        for row in rows {
            collected.push(row?);
        }
        collected
    };

    for (track_id, provider, state, message, confidence, provider_item_id, last_attempt_at) in
        legacy_rows
    {
        // Only migrate rows whose provider names a known provider kind.
        if ProviderKind::from_key(&provider).is_err() {
            continue;
        }
        let message_ref = message.as_deref().unwrap_or("");
        let Some((status, candidate)) =
            classify_legacy_conflict_row(&state, message_ref, provider_item_id.as_deref())
        else {
            continue;
        };
        let detected_at = last_attempt_at.unwrap_or_else(|| encode_datetime(&Utc::now()));
        let rejected_at = if status == IdentityConflictStatus::Rejected {
            Some(detected_at.clone())
        } else {
            None
        };
        transaction.execute(
            "INSERT OR IGNORE INTO track_identity_conflicts
                (track_id, provider, candidate_provider_id, confidence, detected_at, status, rejected_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                track_id,
                provider,
                candidate,
                confidence,
                detected_at,
                status.as_str(),
                rejected_at,
            ],
        )?;
    }

    Ok(())
}

/// Classifies a legacy `track_provider_status` row as an identity conflict and
/// resolves the candidate provider ID, or returns `None` when the row is not a
/// conflict marker or the candidate cannot be recovered (such rows are dropped).
fn classify_legacy_conflict_row(
    state: &str,
    message: &str,
    provider_item_id: Option<&str>,
) -> Option<(IdentityConflictStatus, String)> {
    let (status, candidate) =
        if state == "unmatched" && message.starts_with("Rejected identity candidate") {
            (
                IdentityConflictStatus::Rejected,
                provider_item_id
                    .map(ToOwned::to_owned)
                    .or_else(|| parse_legacy_conflict_candidate(message)),
            )
        } else if state == "error"
            && (message.contains("conflicting provider IDs")
                || message.contains("Cannot merge tracks because provider"))
        {
            (
                IdentityConflictStatus::Open,
                provider_item_id
                    .map(ToOwned::to_owned)
                    .or_else(|| parse_legacy_conflict_candidate(message)),
            )
        } else {
            return None;
        };

    let candidate = candidate?;
    if candidate.trim().is_empty() {
        return None;
    }
    Some((status, candidate))
}

/// Recovers the candidate provider ID from a legacy conflict message of the
/// form `... identity '<id>' ...`. Only used during migration.
fn parse_legacy_conflict_candidate(message: &str) -> Option<String> {
    let marker = "identity '";
    let start = message.find(marker)? + marker.len();
    let suffix = &message[start..];
    let end = suffix.find('\'')?;
    let candidate = suffix[..end].trim();
    if candidate.is_empty() {
        None
    } else {
        Some(candidate.to_string())
    }
}

fn read_schema_version(connection: &Connection) -> Result<Option<u32>> {
    read_metadata(connection, SCHEMA_VERSION_KEY)?
        .map(|value| {
            value
                .parse::<u32>()
                .context("Failed to parse stored schema version")
        })
        .transpose()
}

fn write_schema_version(connection: &Connection, version: u32) -> Result<()> {
    connection.execute(
        "INSERT OR REPLACE INTO library_metadata (key, value) VALUES (?1, ?2)",
        params![SCHEMA_VERSION_KEY, version.to_string()],
    )?;
    Ok(())
}

fn write_schema_version_tx(transaction: &Transaction<'_>, version: u32) -> Result<()> {
    transaction.execute(
        "INSERT OR REPLACE INTO library_metadata (key, value) VALUES (?1, ?2)",
        params![SCHEMA_VERSION_KEY, version.to_string()],
    )?;
    Ok(())
}

fn initialize_runtime_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS runtime_metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS provider_connections (
            provider TEXT PRIMARY KEY,
            config_json TEXT NOT NULL,
            connected_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS provider_cooldowns (
            provider TEXT PRIMARY KEY,
            blocked_until TEXT NOT NULL,
            reason TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS provider_health (
            provider TEXT PRIMARY KEY,
            checked_at TEXT NOT NULL,
            ok INTEGER NOT NULL,
            message TEXT
        );

        CREATE TABLE IF NOT EXISTS ui_operations (
            id TEXT PRIMARY KEY,
            status TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ui_operations_updated_at
            ON ui_operations(updated_at DESC);
        ",
    )?;
    Ok(())
}

fn migrate_legacy_runtime_state(root: &Path, runtime: &Connection) -> Result<()> {
    if runtime
        .query_row(
            "SELECT value FROM runtime_metadata WHERE key = 'legacy_library_state_migrated'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .is_some()
    {
        return Ok(());
    }

    let library_path = database_path_in(root);
    if !library_path.exists() {
        mark_legacy_runtime_migration_complete(runtime)?;
        return Ok(());
    }

    let library = open_database(&library_path)?;
    let has_connections = schema_table_exists(&library, "provider_connections")?;
    let has_operations = schema_table_exists(&library, "ui_operations")?;
    if !has_connections && !has_operations {
        mark_legacy_runtime_migration_complete(runtime)?;
        return Ok(());
    }

    let mut migrated = false;
    let transaction = runtime.unchecked_transaction()?;
    if has_connections {
        let mut statement = library.prepare(
            "SELECT provider, config_json, connected_at, updated_at FROM provider_connections",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (provider, config_json, connected_at, updated_at) = row?;
            if ProviderKind::from_key(&provider).is_err() {
                migrated = true;
                continue;
            }
            transaction.execute(
                "INSERT OR REPLACE INTO provider_connections
                    (provider, config_json, connected_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![provider, config_json, connected_at, updated_at],
            )?;
            migrated = true;
        }
    }
    if has_operations {
        let mut statement = library.prepare(
            "SELECT id, status, payload_json, created_at, updated_at FROM ui_operations",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (id, status, payload_json, created_at, updated_at) = row?;
            transaction.execute(
                "INSERT OR REPLACE INTO ui_operations
                    (id, status, payload_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, status, payload_json, created_at, updated_at],
            )?;
            migrated = true;
        }
    }
    transaction.commit()?;

    if migrated {
        snapshot_existing_database(root, &library_path)?;
        if has_connections {
            library.execute("DELETE FROM provider_connections", [])?;
        }
        if has_operations {
            library.execute("DELETE FROM ui_operations", [])?;
        }
    }
    mark_legacy_runtime_migration_complete(runtime)?;
    Ok(())
}

fn mark_legacy_runtime_migration_complete(runtime: &Connection) -> Result<()> {
    runtime.execute(
        "INSERT OR REPLACE INTO runtime_metadata (key, value) VALUES (?1, ?2)",
        params!["legacy_library_state_migrated", "1"],
    )?;
    Ok(())
}

fn schema_table_exists(connection: &Connection, table_name: &str) -> Result<bool> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
            params![table_name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn count_table_rows(connection: &Connection, table_name: &'static str) -> Result<i64> {
    Ok(
        connection.query_row(&format!("SELECT COUNT(*) FROM {table_name}"), [], |row| {
            row.get(0)
        })?,
    )
}

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

fn load_library_state(connection: &Connection) -> Result<LibraryState> {
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

fn read_metadata(connection: &Connection, key: &str) -> Result<Option<String>> {
    connection
        .query_row(
            "SELECT value FROM library_metadata WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn load_provider_connections(connection: &Connection) -> Result<Vec<ProviderConnection>> {
    let mut statement = connection.prepare(
        "SELECT provider, config_json, connected_at, updated_at
         FROM provider_connections
         WHERE provider IN ('spotify', 'youtube-music')
         ORDER BY provider",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;

    let mut connections = Vec::new();
    for row in rows {
        let (provider_key, config_json, connected_at, updated_at) = row?;
        let provider = ProviderKind::from_key(&provider_key)?;
        let config = parse_provider_connection_config(provider, &config_json)?;
        connections.push(ProviderConnection {
            provider,
            connected_at: parse_datetime(&connected_at)?,
            updated_at: parse_datetime(&updated_at)?,
            config,
        });
    }

    Ok(connections)
}

fn load_provider_healths(connection: &Connection) -> Result<Vec<ProviderHealth>> {
    let mut statement = connection.prepare(
        "SELECT provider, checked_at, ok, message
         FROM provider_health
         WHERE provider IN ('spotify', 'youtube-music')
         ORDER BY provider",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;

    let mut healths = Vec::new();
    for row in rows {
        let (provider_key, checked_at, ok, message) = row?;
        healths.push(ProviderHealth {
            provider: ProviderKind::from_key(&provider_key)?,
            checked_at: parse_datetime(&checked_at)?,
            ok: ok != 0,
            message,
        });
    }

    Ok(healths)
}

fn load_provider_health(
    connection: &Connection,
    provider: ProviderKind,
) -> Result<Option<ProviderHealth>> {
    connection
        .query_row(
            "SELECT provider, checked_at, ok, message
             FROM provider_health
             WHERE provider = ?1",
            params![provider.as_key()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?
        .map(|(provider_key, checked_at, ok, message)| {
            Ok(ProviderHealth {
                provider: ProviderKind::from_key(&provider_key)?,
                checked_at: parse_datetime(&checked_at)?,
                ok: ok != 0,
                message,
            })
        })
        .transpose()
}

fn clear_expired_provider_cooldowns(connection: &Connection) -> Result<()> {
    connection.execute(
        "DELETE FROM provider_cooldowns WHERE blocked_until <= ?1",
        params![encode_datetime(&Utc::now())],
    )?;
    Ok(())
}

fn load_provider_cooldowns(connection: &Connection) -> Result<Vec<ProviderCooldown>> {
    let mut statement = connection.prepare(
        "SELECT provider, blocked_until, reason, updated_at
         FROM provider_cooldowns
         WHERE provider IN ('spotify', 'youtube-music')
         ORDER BY provider",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;

    let mut cooldowns = Vec::new();
    for row in rows {
        let (provider_key, blocked_until, reason, updated_at) = row?;
        cooldowns.push(ProviderCooldown {
            provider: ProviderKind::from_key(&provider_key)?,
            blocked_until: parse_datetime(&blocked_until)?,
            reason,
            updated_at: parse_datetime(&updated_at)?,
        });
    }

    Ok(cooldowns)
}

fn load_provider_cooldown(
    connection: &Connection,
    provider: ProviderKind,
) -> Result<Option<ProviderCooldown>> {
    connection
        .query_row(
            "SELECT provider, blocked_until, reason, updated_at
             FROM provider_cooldowns
             WHERE provider = ?1",
            params![provider.as_key()],
            |row| {
                let provider_key = row.get::<_, String>(0)?;
                Ok((
                    provider_key,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?
        .map(|(provider_key, blocked_until, reason, updated_at)| {
            Ok(ProviderCooldown {
                provider: ProviderKind::from_key(&provider_key)?,
                blocked_until: parse_datetime(&blocked_until)?,
                reason,
                updated_at: parse_datetime(&updated_at)?,
            })
        })
        .transpose()
}

fn parse_provider_connection_config(
    provider: ProviderKind,
    config_json: &str,
) -> Result<ProviderConnectionConfig> {
    match provider {
        ProviderKind::Spotify => Ok(ProviderConnectionConfig::Spotify(
            serde_json::from_str::<SpotifyConnectionConfig>(config_json)
                .context("Failed to parse stored Spotify connection config")?,
        )),
        ProviderKind::YoutubeMusic => Ok(ProviderConnectionConfig::YoutubeMusic(
            serde_json::from_str::<YoutubeMusicConnectionConfig>(config_json)
                .context("Failed to parse stored YouTube Music connection config")?,
        )),
    }
}

fn read_legacy_json_state(root: &Path) -> Result<Option<LibraryState>> {
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

fn legacy_dump_exists(root: &Path) -> Result<bool> {
    Ok(has_legacy_csv_dump(root))
}

fn read_legacy_csv_state(root: &Path) -> Result<LibraryState> {
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

fn write_metadata_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("library_metadata.csv"))?;
    writer.write_record(["key", "value"])?;
    writer.write_record(["schema_version", &state.format_version.to_string()])?;
    writer.write_record(["created_at", &encode_datetime(&state.created_at)])?;
    writer.write_record(["updated_at", &encode_datetime(&state.updated_at)])?;
    writer.flush()?;
    Ok(())
}

fn write_tracks_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("tracks.csv"))?;
    writer.write_record(["id", "title", "album", "duration_seconds", "isrc"])?;
    for track in &state.tracks {
        writer.write_record(vec![
            track.id.clone(),
            track.metadata.title.clone(),
            track.metadata.album.clone().unwrap_or_default(),
            optional_number(track.metadata.duration_seconds),
            track.metadata.isrc.clone().unwrap_or_default(),
        ])?;
    }
    writer.flush()?;
    Ok(())
}

fn write_track_artists_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("track_artists.csv"))?;
    writer.write_record(["track_id", "position", "name"])?;
    for track in &state.tracks {
        for (position, artist) in track.metadata.artists.iter().enumerate() {
            writer.write_record([track.id.as_str(), &position.to_string(), artist.as_str()])?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn write_track_provider_links_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("track_provider_links.csv"))?;
    writer.write_record([
        "track_id",
        "provider",
        "provider_id",
        "source",
        "confidence",
        "linked_at",
        "last_seen_at",
    ])?;
    for track in &state.tracks {
        for (provider, link) in &track.provider_links {
            writer.write_record(vec![
                track.id.clone(),
                provider.clone(),
                link.provider_id.clone(),
                link.source.as_str().to_string(),
                optional_float(link.confidence),
                encode_datetime(&link.linked_at),
                optional_datetime(link.last_seen_at.as_ref()),
            ])?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn write_track_provider_artwork_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("track_provider_artwork.csv"))?;
    writer.write_record([
        "track_id",
        "provider",
        "url",
        "width",
        "height",
        "last_seen_at",
    ])?;
    for track in &state.tracks {
        for (provider, artwork) in &track.provider_artwork {
            writer.write_record(vec![
                track.id.clone(),
                provider.clone(),
                artwork.url.clone(),
                optional_number(artwork.width),
                optional_number(artwork.height),
                optional_datetime(artwork.last_seen_at.as_ref()),
            ])?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn write_track_provider_status_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("track_provider_status.csv"))?;
    writer.write_record(status_header("track_id"))?;
    for track in &state.tracks {
        write_status_rows(&mut writer, "track_id", &track.id, &track.provider_state)?;
    }
    writer.flush()?;
    Ok(())
}

fn write_track_identity_conflicts_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("track_identity_conflicts.csv"))?;
    writer.write_record([
        "track_id",
        "provider",
        "candidate_provider_id",
        "confidence",
        "detected_at",
        "status",
        "rejected_at",
    ])?;
    for track in &state.tracks {
        for conflict in &track.identity_conflicts {
            writer.write_record(vec![
                track.id.clone(),
                conflict.provider.as_key().to_string(),
                conflict.candidate_provider_id.clone(),
                optional_float(conflict.confidence),
                encode_datetime(&conflict.detected_at),
                conflict.status.as_str().to_string(),
                optional_datetime(conflict.rejected_at.as_ref()),
            ])?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn write_saved_tracks_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("saved_tracks.csv"))?;
    writer.write_record(["id", "track_id", "added_at"])?;
    for saved_track in &state.saved_tracks {
        writer.write_record([
            saved_track.id.as_str(),
            saved_track.track_id.as_str(),
            optional_str(saved_track.added_at.as_deref()),
        ])?;
    }
    writer.flush()?;
    Ok(())
}

fn write_saved_track_provider_status_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("saved_track_provider_status.csv"))?;
    writer.write_record(status_header("saved_track_id"))?;
    for saved_track in &state.saved_tracks {
        write_status_rows(
            &mut writer,
            "saved_track_id",
            &saved_track.id,
            &saved_track.provider_state,
        )?;
    }
    writer.flush()?;
    Ok(())
}

fn write_playlists_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("playlists.csv"))?;
    writer.write_record(["id", "name", "description"])?;
    for playlist in &state.playlists {
        writer.write_record([
            playlist.id.as_str(),
            playlist.name.as_str(),
            optional_str(playlist.description.as_deref()),
        ])?;
    }
    writer.flush()?;
    Ok(())
}

fn write_playlist_provider_links_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("playlist_provider_links.csv"))?;
    writer.write_record([
        "playlist_id",
        "provider",
        "provider_id",
        "source",
        "confidence",
        "linked_at",
        "last_seen_at",
    ])?;
    for playlist in &state.playlists {
        for (provider, link) in &playlist.provider_links {
            writer.write_record(vec![
                playlist.id.clone(),
                provider.clone(),
                link.provider_id.clone(),
                link.source.as_str().to_string(),
                optional_float(link.confidence),
                encode_datetime(&link.linked_at),
                optional_datetime(link.last_seen_at.as_ref()),
            ])?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn write_playlist_provider_status_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("playlist_provider_status.csv"))?;
    writer.write_record(status_header("playlist_id"))?;
    for playlist in &state.playlists {
        write_status_rows(
            &mut writer,
            "playlist_id",
            &playlist.id,
            &playlist.provider_state,
        )?;
    }
    writer.flush()?;
    Ok(())
}

fn write_playlist_entries_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("playlist_entries.csv"))?;
    writer.write_record(["id", "playlist_id", "position", "track_id", "added_at"])?;
    for playlist in &state.playlists {
        for (position, entry) in playlist.entries.iter().enumerate() {
            writer.write_record([
                entry.id.as_str(),
                playlist.id.as_str(),
                &position.to_string(),
                entry.track_id.as_str(),
                optional_str(entry.added_at.as_deref()),
            ])?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn write_playlist_entry_provider_status_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("playlist_entry_provider_status.csv"))?;
    writer.write_record(status_header("entry_id"))?;
    for playlist in &state.playlists {
        for entry in &playlist.entries {
            write_status_rows(&mut writer, "entry_id", &entry.id, &entry.provider_state)?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn csv_writer(path: PathBuf) -> Result<Writer<std::fs::File>> {
    Writer::from_path(&path).with_context(|| format!("Failed to write {}", path.display()))
}

fn status_header(owner_column: &'static str) -> [&'static str; 9] {
    [
        owner_column,
        "provider",
        "state",
        "message",
        "confidence",
        "provider_item_id",
        "last_attempt_at",
        "last_success_at",
        "last_seen_at",
    ]
}

fn write_status_rows(
    writer: &mut Writer<std::fs::File>,
    _owner_column: &str,
    owner_id: &str,
    statuses: &BTreeMap<String, SyncStatusRecord>,
) -> Result<()> {
    for (provider, status) in statuses {
        writer.write_record(vec![
            owner_id.to_string(),
            provider.clone(),
            status.state.as_str().to_string(),
            status.message.clone().unwrap_or_default(),
            optional_float(status.confidence),
            status.provider_item_id.clone().unwrap_or_default(),
            optional_datetime(status.last_attempt_at.as_ref()),
            optional_datetime(status.last_success_at.as_ref()),
            optional_datetime(status.last_seen_at.as_ref()),
        ])?;
    }
    Ok(())
}

fn encode_datetime(value: &DateTime<Utc>) -> String {
    value.to_rfc3339()
}

fn encode_datetime_option(value: Option<&DateTime<Utc>>) -> Option<String> {
    value.map(encode_datetime)
}

fn parse_datetime(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

fn optional_str(value: Option<&str>) -> &str {
    value.unwrap_or("")
}

fn optional_number(value: Option<u32>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn optional_float(value: Option<f64>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn optional_datetime(value: Option<&DateTime<Utc>>) -> String {
    value.map(encode_datetime).unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{database_path_in, discover_default_data_root, DUMP_DIR, LIBRARY_DB_FILE};

    #[test]
    fn default_data_root_prefers_existing_local_dump() {
        let temp = tempdir().unwrap();
        let app_root = temp.path().join("spoti-dump");
        fs::create_dir_all(app_root.join(DUMP_DIR)).unwrap();
        fs::write(database_path_in(&app_root), []).unwrap();
        fs::create_dir_all(temp.path().join(DUMP_DIR)).unwrap();
        fs::write(database_path_in(temp.path()), []).unwrap();

        assert_eq!(discover_default_data_root(&app_root, &app_root), app_root);
    }

    #[test]
    fn default_data_root_discovers_existing_adjacent_parent_dump() {
        let temp = tempdir().unwrap();
        let app_root = temp.path().join("spoti-dump");
        fs::create_dir_all(&app_root).unwrap();
        fs::create_dir_all(temp.path().join(DUMP_DIR)).unwrap();
        fs::write(database_path_in(temp.path()), []).unwrap();

        assert_eq!(
            discover_default_data_root(&app_root, &app_root),
            temp.path()
        );
    }

    #[test]
    fn default_data_root_stays_local_for_fresh_installs() {
        let temp = tempdir().unwrap();
        let app_root = temp.path().join("spoti-dump");
        fs::create_dir_all(&app_root).unwrap();
        fs::create_dir_all(temp.path().join(DUMP_DIR)).unwrap();

        assert_eq!(discover_default_data_root(&app_root, &app_root), app_root);
    }

    #[test]
    fn default_data_root_discovers_adjacent_legacy_csv_dump() {
        let temp = tempdir().unwrap();
        let app_root = temp.path().join("spoti-dump");
        fs::create_dir_all(&app_root).unwrap();
        let dump_dir = temp.path().join(DUMP_DIR);
        fs::create_dir_all(&dump_dir).unwrap();
        fs::write(dump_dir.join("saved_tracks.csv"), "title,artist\n").unwrap();

        assert_eq!(
            discover_default_data_root(&app_root, &app_root),
            temp.path()
        );
    }

    #[test]
    fn library_database_file_constant_matches_discovery_path() {
        let temp = tempdir().unwrap();
        assert_eq!(
            database_path_in(temp.path())
                .file_name()
                .and_then(|value| value.to_str()),
            Some(LIBRARY_DB_FILE)
        );
    }
}
