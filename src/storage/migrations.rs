//! Schema-version framework for the canonical library database.
//!
//! A fresh database is created at [`CURRENT_SCHEMA_VERSION`]; an older database
//! is snapshotted then migrated forward one version at a time; a newer database
//! is rejected. Append new migrations to [`MIGRATIONS`]; never edit or reorder
//! released ones.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, Transaction};

use crate::domain::{IdentityConflictStatus, ProviderKind, LIBRARY_STATE_FORMAT_VERSION};

use super::{backups, checkpoint, encode_datetime, library, read_metadata, schema_table_exists};

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
/// the stored version.
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

/// Brings an opened library database to `CURRENT_SCHEMA_VERSION`.
///
/// * A brand-new/empty database (no `library_metadata` table) is created at the
///   current schema directly, with no migrations run.
/// * A database already at the current version is left as-is.
/// * An older database is snapshotted (reusing the standard backup helper) and
///   then each pending migration is applied in its own transaction, bumping the
///   stored version as it goes.
/// * A newer database is a hard error, telling the user to upgrade the app.
pub(super) fn prepare_library_database(
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
pub(super) fn prepare_staging_database(
    connection: &mut Connection,
    database_path: &Path,
) -> Result<()> {
    prepare_opened_database(connection, database_path, None)
}

fn prepare_opened_database(
    connection: &mut Connection,
    database_path: &Path,
    snapshot_into: Option<(&Path, &Path)>,
) -> Result<()> {
    if !schema_table_exists(connection, "library_metadata")? {
        library::initialize_schema(connection)?;
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
        // Flush the WAL first so the raw-file snapshot is self-contained.
        checkpoint(connection)?;
        backups::snapshot_existing_database(root, path)?;
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
