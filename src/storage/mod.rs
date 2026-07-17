//! Persistence layer for the local-first music library.
//!
//! The layer is split across focused submodules:
//!
//! * [`library`] — the [`LibraryDb`] handle wrapping a single SQLite connection
//!   to the canonical `library.db`, plus load/save of [`LibraryState`].
//! * [`runtime`] — the [`RuntimeDb`] handle for `runtime.db` (provider
//!   connections, health, cooldowns, UI operation history).
//! * [`migrations`] — the schema-version framework and forward migrations.
//! * [`backups`] — snapshot/list/restore/prune and backup-name validation.
//! * [`csv`] — normalized CSV export.
//! * [`legacy`] — one-shot import of the retired JSON/CSV dump formats.
//!
//! Callers use the bare functions ([`write_library_state`], ...), which operate
//! on a process-wide handle keyed by [`data_root`]. The `_in(root)` variants are
//! the real implementations behind those wrappers; they construct a short-lived
//! handle for an explicit root and are what the test-suite drives. Keeping one
//! connection per process means the schema is prepared (and migrations run) once
//! at open, not on every call.

mod backups;
mod csv;
mod legacy;
mod library;
mod migrations;
mod runtime;

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use crate::domain::{
    LibraryState, ProviderConnection, ProviderCooldown, ProviderHealth, ProviderKind,
};

pub use backups::{LibraryBackup, RestoreSummary};
pub use csv::export_csv;
pub use library::{DatabaseHealth, LibraryDb};
pub use runtime::RuntimeDb;

pub const DUMP_DIR: &str = "dump";
pub const LIBRARY_DB_FILE: &str = "library.db";
pub const RUNTIME_DB_FILE: &str = "runtime.db";
pub const LEGACY_LIBRARY_STATE_FILE: &str = "library.json";
pub const DEFAULT_CSV_EXPORT_DIR: &str = "csv";
pub const BACKUP_DIR: &str = "backups";
pub const MANUAL_BACKUP_DIR: &str = "manual-backups";
pub const DATA_DIR_ENV: &str = "SPOTI_DUMP_DATA_DIR";

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

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

pub(crate) fn legacy_library_state_path_in(root: &Path) -> PathBuf {
    root.join(DUMP_DIR).join(LEGACY_LIBRARY_STATE_FILE)
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

pub(crate) fn has_legacy_csv_dump(root: &Path) -> bool {
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

// ---------------------------------------------------------------------------
// Shared low-level helpers
// ---------------------------------------------------------------------------

pub(crate) fn ensure_dump_dir(root: &Path) -> Result<PathBuf> {
    let dump_dir = root.join(DUMP_DIR);
    if !dump_dir.exists() {
        fs::create_dir_all(&dump_dir)
            .with_context(|| format!("Failed to create {}", dump_dir.display()))?;
    }
    Ok(dump_dir)
}

/// Opens a connection with the app's baseline pragmas (`foreign_keys`,
/// `busy_timeout`). Journal-mode selection is left to the caller: the persistent
/// [`LibraryDb`]/[`RuntimeDb`] handles switch to WAL, while throwaway staging
/// copies stay in rollback mode to avoid leaving `-wal`/`-shm` sidecars behind.
pub(crate) fn open_database(database_path: &Path) -> Result<Connection> {
    let connection = Connection::open(database_path)
        .with_context(|| format!("Failed to open {}", database_path.display()))?;
    connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")?;
    Ok(connection)
}

/// Flushes the write-ahead log back into the main database file. Callers run
/// this on the live connection immediately before copying the raw `.db` file so
/// the copy is self-contained (a plain `fs::copy` of a WAL database would
/// otherwise miss committed-but-uncheckpointed frames).
pub(crate) fn checkpoint(connection: &Connection) -> Result<()> {
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(())
}

pub(crate) fn read_metadata(connection: &Connection, key: &str) -> Result<Option<String>> {
    connection
        .query_row(
            "SELECT value FROM library_metadata WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

pub(crate) fn schema_table_exists(connection: &Connection, table_name: &str) -> Result<bool> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
            params![table_name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

pub(crate) fn encode_datetime(value: &DateTime<Utc>) -> String {
    value.to_rfc3339()
}

pub(crate) fn encode_datetime_option(value: Option<&DateTime<Utc>>) -> Option<String> {
    value.map(encode_datetime)
}

pub(crate) fn parse_datetime(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

// ---------------------------------------------------------------------------
// Process-wide handles
// ---------------------------------------------------------------------------
//
// The bare public functions share one handle per `data_root` for the process
// lifetime, so the single SQLite connection (and its prepared schema) is reused
// across calls and the change-guard can compare against the previously
// persisted content. The `_in(root)` functions construct a fresh handle per
// call, which is what the tests rely on to exercise isolated temp roots.

static LIBRARY_DBS: OnceLock<Mutex<HashMap<PathBuf, Arc<LibraryDb>>>> = OnceLock::new();
static RUNTIME_DBS: OnceLock<Mutex<HashMap<PathBuf, Arc<RuntimeDb>>>> = OnceLock::new();

fn library_handle(root: &Path, cached: bool) -> Result<Arc<LibraryDb>> {
    if !cached {
        return Ok(Arc::new(LibraryDb::open(root)?));
    }
    let registry = LIBRARY_DBS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = registry.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(existing) = guard.get(root) {
        return Ok(Arc::clone(existing));
    }
    let handle = Arc::new(LibraryDb::open(root)?);
    guard.insert(root.to_path_buf(), Arc::clone(&handle));
    Ok(handle)
}

fn runtime_handle(root: &Path, cached: bool) -> Result<Arc<RuntimeDb>> {
    if !cached {
        return Ok(Arc::new(RuntimeDb::open(root)?));
    }
    let registry = RUNTIME_DBS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = registry.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(existing) = guard.get(root) {
        return Ok(Arc::clone(existing));
    }
    let handle = Arc::new(RuntimeDb::open(root)?);
    guard.insert(root.to_path_buf(), Arc::clone(&handle));
    Ok(handle)
}

// ---------------------------------------------------------------------------
// Library state
// ---------------------------------------------------------------------------

pub fn write_library_state(state: &LibraryState) -> Result<PathBuf> {
    library_handle(&data_root(), true)?.save(state)
}

pub fn write_library_state_in(root: &Path, state: &LibraryState) -> Result<PathBuf> {
    library_handle(root, false)?.save(state)
}

pub fn read_library_state() -> Result<LibraryState> {
    load_library_state_from(&data_root(), false, true)
}

pub fn read_library_state_or_new() -> Result<LibraryState> {
    load_library_state_from(&data_root(), true, true)
}

pub fn read_library_state_in(root: &Path, allow_empty: bool) -> Result<LibraryState> {
    load_library_state_from(root, allow_empty, false)
}

/// Shared read path for both the bare and `_in` readers. When the canonical
/// database is absent it falls back to a one-shot import of the retired JSON or
/// CSV dump formats (persisting the imported state), and otherwise honours
/// `allow_empty` exactly as the previous per-call implementation did — crucially
/// without creating the database first, so a fresh install still reports "no
/// database" instead of silently materialising an empty one.
fn load_library_state_from(root: &Path, allow_empty: bool, cached: bool) -> Result<LibraryState> {
    if database_path_in(root).exists() {
        return library_handle(root, cached)?.load();
    }

    if let Some(state) = legacy::read_legacy_json_state(root)? {
        library_handle(root, cached)?.save(&state)?;
        return Ok(state);
    }

    if has_legacy_csv_dump(root) {
        let state = legacy::read_legacy_csv_state(root)?;
        library_handle(root, cached)?.save(&state)?;
        return Ok(state);
    }

    if allow_empty {
        Ok(LibraryState::new())
    } else {
        anyhow::bail!(
            "No library database found. Expected {} or legacy data in {}.",
            database_path_in(root).display(),
            root.join(DUMP_DIR).display()
        )
    }
}

pub fn database_health() -> Result<DatabaseHealth> {
    library_handle(&data_root(), true)?.health()
}

pub fn database_health_in(root: &Path) -> Result<DatabaseHealth> {
    library_handle(root, false)?.health()
}

// ---------------------------------------------------------------------------
// Backups
// ---------------------------------------------------------------------------

pub fn create_manual_library_backup() -> Result<LibraryBackup> {
    let root = data_root();
    require_existing_library(&root)?;
    library_handle(&root, true)?.create_manual_backup()
}

pub fn create_manual_library_backup_in(root: &Path) -> Result<LibraryBackup> {
    require_existing_library(root)?;
    library_handle(root, false)?.create_manual_backup()
}

fn require_existing_library(root: &Path) -> Result<()> {
    ensure_dump_dir(root)?;
    if !database_path_in(root).exists() {
        anyhow::bail!(
            "No canonical library database exists at {}.",
            database_path_in(root).display()
        );
    }
    Ok(())
}

pub fn list_library_backups() -> Result<Vec<LibraryBackup>> {
    backups::list_library_backups(&data_root())
}

pub fn list_library_backups_in(root: &Path) -> Result<Vec<LibraryBackup>> {
    backups::list_library_backups(root)
}

pub fn restore_library_backup(backup_type: &str, file_name: &str) -> Result<RestoreSummary> {
    restore_library_backup_impl(&data_root(), backup_type, file_name, true)
}

pub fn restore_library_backup_in(
    root: &Path,
    backup_type: &str,
    file_name: &str,
) -> Result<RestoreSummary> {
    restore_library_backup_impl(root, backup_type, file_name, false)
}

fn restore_library_backup_impl(
    root: &Path,
    backup_type: &str,
    file_name: &str,
    cached: bool,
) -> Result<RestoreSummary> {
    ensure_dump_dir(root)?;
    let backup_path = backups::resolve_backup_path(root, backup_type, file_name)?;
    backups::validate_backup_database(&backup_path)?;
    let restored_state = backups::load_restorable_backup_state(root, &backup_path)?;

    let handle = library_handle(root, cached)?;
    let pre_restore_backup = handle
        .create_manual_backup()
        .context("Failed to create pre-restore manual backup")?;
    handle.save(&restored_state).with_context(|| {
        format!(
            "Failed to restore validated backup {} into {}",
            backup_path.display(),
            database_path_in(root).display()
        )
    })?;

    let restored_backup =
        backups::backup_record_from_path(backup_path, backups::backup_type_label(backup_type)?)?;
    Ok(RestoreSummary {
        restored_backup,
        pre_restore_backup,
    })
}

// ---------------------------------------------------------------------------
// Provider connections
// ---------------------------------------------------------------------------

pub fn list_provider_connections() -> Result<Vec<ProviderConnection>> {
    runtime_handle(&data_root(), true)?.list_connections()
}

pub fn list_provider_connections_in(root: &Path) -> Result<Vec<ProviderConnection>> {
    runtime_handle(root, false)?.list_connections()
}

pub fn read_provider_connection(provider: ProviderKind) -> Result<Option<ProviderConnection>> {
    Ok(list_provider_connections()?
        .into_iter()
        .find(|connection| connection.provider == provider))
}

pub fn save_provider_connection(connection: &ProviderConnection) -> Result<()> {
    runtime_handle(&data_root(), true)?.save_connection(connection)
}

pub fn save_provider_connection_in(root: &Path, connection: &ProviderConnection) -> Result<()> {
    runtime_handle(root, false)?.save_connection(connection)
}

pub fn delete_provider_connection(provider: ProviderKind) -> Result<()> {
    runtime_handle(&data_root(), true)?.delete_connection(provider)
}

pub fn delete_provider_connection_in(root: &Path, provider: ProviderKind) -> Result<()> {
    runtime_handle(root, false)?.delete_connection(provider)
}

// ---------------------------------------------------------------------------
// Provider health
// ---------------------------------------------------------------------------

pub fn list_provider_healths() -> Result<Vec<ProviderHealth>> {
    runtime_handle(&data_root(), true)?.list_healths()
}

pub fn list_provider_healths_in(root: &Path) -> Result<Vec<ProviderHealth>> {
    runtime_handle(root, false)?.list_healths()
}

pub fn read_provider_health(provider: ProviderKind) -> Result<Option<ProviderHealth>> {
    runtime_handle(&data_root(), true)?.read_health(provider)
}

pub fn read_provider_health_in(
    root: &Path,
    provider: ProviderKind,
) -> Result<Option<ProviderHealth>> {
    runtime_handle(root, false)?.read_health(provider)
}

pub fn save_provider_health(health: &ProviderHealth) -> Result<()> {
    runtime_handle(&data_root(), true)?.save_health(health)
}

pub fn save_provider_health_in(root: &Path, health: &ProviderHealth) -> Result<()> {
    runtime_handle(root, false)?.save_health(health)
}

pub fn clear_provider_health(provider: ProviderKind) -> Result<()> {
    runtime_handle(&data_root(), true)?.clear_health(provider)
}

pub fn clear_provider_health_in(root: &Path, provider: ProviderKind) -> Result<()> {
    runtime_handle(root, false)?.clear_health(provider)
}

// ---------------------------------------------------------------------------
// Provider cooldowns
// ---------------------------------------------------------------------------

pub fn list_provider_cooldowns() -> Result<Vec<ProviderCooldown>> {
    runtime_handle(&data_root(), true)?.list_cooldowns()
}

pub fn list_provider_cooldowns_in(root: &Path) -> Result<Vec<ProviderCooldown>> {
    runtime_handle(root, false)?.list_cooldowns()
}

pub fn read_provider_cooldown(provider: ProviderKind) -> Result<Option<ProviderCooldown>> {
    runtime_handle(&data_root(), true)?.read_cooldown(provider)
}

pub fn read_provider_cooldown_in(
    root: &Path,
    provider: ProviderKind,
) -> Result<Option<ProviderCooldown>> {
    runtime_handle(root, false)?.read_cooldown(provider)
}

pub fn save_provider_cooldown(cooldown: &ProviderCooldown) -> Result<()> {
    runtime_handle(&data_root(), true)?.save_cooldown(cooldown)
}

pub fn save_provider_cooldown_in(root: &Path, cooldown: &ProviderCooldown) -> Result<()> {
    runtime_handle(root, false)?.save_cooldown(cooldown)
}

pub fn clear_provider_cooldown(provider: ProviderKind) -> Result<()> {
    runtime_handle(&data_root(), true)?.clear_cooldown(provider)
}

pub fn clear_provider_cooldown_in(root: &Path, provider: ProviderKind) -> Result<()> {
    runtime_handle(root, false)?.clear_cooldown(provider)
}

// ---------------------------------------------------------------------------
// UI operation history
// ---------------------------------------------------------------------------

pub fn save_ui_operation_json(operation_id: &str, status: &str, payload_json: &str) -> Result<()> {
    runtime_handle(&data_root(), true)?.save_ui_operation(operation_id, status, payload_json)
}

pub fn save_ui_operation_json_in(
    root: &Path,
    operation_id: &str,
    status: &str,
    payload_json: &str,
) -> Result<()> {
    runtime_handle(root, false)?.save_ui_operation(operation_id, status, payload_json)
}

pub fn read_ui_operation_json(operation_id: &str) -> Result<Option<String>> {
    let root = data_root();
    if runtime_state_absent(&root) {
        return Ok(None);
    }
    runtime_handle(&root, true)?.read_ui_operation(operation_id)
}

pub fn read_ui_operation_json_in(root: &Path, operation_id: &str) -> Result<Option<String>> {
    if runtime_state_absent(root) {
        return Ok(None);
    }
    runtime_handle(root, false)?.read_ui_operation(operation_id)
}

pub fn list_ui_operation_json() -> Result<Vec<String>> {
    let root = data_root();
    if runtime_state_absent(&root) {
        return Ok(Vec::new());
    }
    runtime_handle(&root, true)?.list_ui_operations()
}

pub fn list_ui_operation_json_in(root: &Path) -> Result<Vec<String>> {
    if runtime_state_absent(root) {
        return Ok(Vec::new());
    }
    runtime_handle(root, false)?.list_ui_operations()
}

/// UI-operation reads must not materialise `runtime.db` just to answer that
/// there is nothing stored yet, matching the previous behaviour.
fn runtime_state_absent(root: &Path) -> bool {
    !runtime_database_path_in(root).exists() && !database_path_in(root).exists()
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
