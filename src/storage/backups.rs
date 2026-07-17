//! Backup lifecycle: rolling pre-write/pre-migration snapshots (bounded
//! retention), manual backups (unpruned), listing, and traversal-guarded
//! restore with migrate-on-restore.
//!
//! Snapshots and manual backups are plain file copies of the live database.
//! Callers that hold the live connection must checkpoint it (via
//! [`super::checkpoint`]) before invoking [`snapshot_existing_database`] or
//! [`copy_manual_backup`] so the copy captures the whole WAL.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use uuid::Uuid;

use crate::domain::LibraryState;

use super::{
    automatic_backup_dir_in, ensure_dump_dir, library, manual_backup_dir_in, migrations,
    open_database, BACKUP_DIR, DUMP_DIR,
};

const BACKUP_RETENTION: usize = 50;

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

/// Rolling pre-write/pre-migration snapshot of the existing database, bounded to
/// [`BACKUP_RETENTION`]. No-op when the database file does not yet exist.
///
/// The caller must have checkpointed the live connection first.
pub(super) fn snapshot_existing_database(root: &Path, database_path: &Path) -> Result<()> {
    if !database_path.exists() {
        return Ok(());
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
    Ok(())
}

/// Copies the live database into the manual (unpruned) backup directory and
/// returns its record. The caller must have checkpointed the live connection.
pub(super) fn copy_manual_backup(root: &Path, database_path: &Path) -> Result<LibraryBackup> {
    let backup_dir = manual_backup_dir_in(root);
    fs::create_dir_all(&backup_dir)
        .with_context(|| format!("Failed to create {}", backup_dir.display()))?;
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.9fZ");
    let backup_path = backup_dir.join(format!("manual-library-{timestamp}-{}.db", Uuid::new_v4()));
    fs::copy(database_path, &backup_path).with_context(|| {
        format!(
            "Failed to copy {} to {}",
            database_path.display(),
            backup_path.display()
        )
    })?;
    backup_record_from_path(backup_path, "manual")
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

pub(super) fn list_library_backups(root: &Path) -> Result<Vec<LibraryBackup>> {
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

pub(super) fn backup_record_from_path(
    path: PathBuf,
    backup_type: &'static str,
) -> Result<LibraryBackup> {
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

pub(super) fn resolve_backup_path(
    root: &Path,
    backup_type: &str,
    file_name: &str,
) -> Result<PathBuf> {
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

pub(super) fn backup_type_label(backup_type: &str) -> Result<&'static str> {
    match backup_type {
        "automatic" => Ok("automatic"),
        "manual" => Ok("manual"),
        _ => anyhow::bail!("Unsupported backup type '{backup_type}'."),
    }
}

pub(super) fn validate_backup_database(path: &Path) -> Result<()> {
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

/// Copies a validated backup into a throwaway staging file, migrates it up to
/// the current schema, and loads it. The staging copy is opened in rollback
/// (DELETE) mode so it never leaves `-wal`/`-shm` sidecars in the dump
/// directory, and the staging file is always removed before returning.
pub(super) fn load_restorable_backup_state(
    root: &Path,
    backup_path: &Path,
) -> Result<LibraryState> {
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
        connection.execute_batch("PRAGMA journal_mode = DELETE;")?;
        migrations::prepare_staging_database(&mut connection, &staging_path)?;
        library::load_library_state(&connection)
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
