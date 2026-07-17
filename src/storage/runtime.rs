//! The runtime database: provider connections, health, cooldowns, and UI
//! operation history. Holds one connection (WAL, `0600`-hardened on Unix) opened
//! once per handle; schema init and the one-shot legacy-credential migration run
//! at open.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};

use crate::domain::{
    ProviderConnection, ProviderConnectionConfig, ProviderCooldown, ProviderHealth, ProviderKind,
    SpotifyConnectionConfig, YoutubeMusicConnectionConfig,
};

use super::{
    backups, checkpoint, database_path_in, encode_datetime, ensure_dump_dir, open_database,
    parse_datetime, runtime_database_path_in, schema_table_exists,
};

const OPERATION_RETENTION: usize = 100;

/// A handle to `runtime.db` wrapping a single SQLite connection.
pub struct RuntimeDb {
    connection: Mutex<Connection>,
}

impl RuntimeDb {
    pub fn open(root: &Path) -> Result<RuntimeDb> {
        ensure_dump_dir(root)?;
        let runtime_path = runtime_database_path_in(root);
        let connection = open_database(&runtime_path)?;
        connection.execute_batch("PRAGMA journal_mode = WAL;")?;
        harden_runtime_file_permissions(&runtime_path)?;
        initialize_runtime_schema(&connection)?;
        migrate_legacy_runtime_state(root, &connection)?;
        Ok(RuntimeDb {
            connection: Mutex::new(connection),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    // ---- provider connections ----

    pub fn list_connections(&self) -> Result<Vec<ProviderConnection>> {
        load_provider_connections(&self.lock())
    }

    pub fn save_connection(&self, connection: &ProviderConnection) -> Result<()> {
        self.lock().execute(
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

    pub fn delete_connection(&self, provider: ProviderKind) -> Result<()> {
        let database = self.lock();
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

    // ---- provider health ----

    pub fn list_healths(&self) -> Result<Vec<ProviderHealth>> {
        load_provider_healths(&self.lock())
    }

    pub fn read_health(&self, provider: ProviderKind) -> Result<Option<ProviderHealth>> {
        load_provider_health(&self.lock(), provider)
    }

    pub fn save_health(&self, health: &ProviderHealth) -> Result<()> {
        self.lock().execute(
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

    pub fn clear_health(&self, provider: ProviderKind) -> Result<()> {
        self.lock().execute(
            "DELETE FROM provider_health WHERE provider = ?1",
            params![provider.as_key()],
        )?;
        Ok(())
    }

    // ---- provider cooldowns ----

    pub fn list_cooldowns(&self) -> Result<Vec<ProviderCooldown>> {
        let database = self.lock();
        clear_expired_provider_cooldowns(&database)?;
        load_provider_cooldowns(&database)
    }

    pub fn read_cooldown(&self, provider: ProviderKind) -> Result<Option<ProviderCooldown>> {
        let database = self.lock();
        clear_expired_provider_cooldowns(&database)?;
        load_provider_cooldown(&database, provider)
    }

    pub fn save_cooldown(&self, cooldown: &ProviderCooldown) -> Result<()> {
        self.lock().execute(
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

    pub fn clear_cooldown(&self, provider: ProviderKind) -> Result<()> {
        self.lock().execute(
            "DELETE FROM provider_cooldowns WHERE provider = ?1",
            params![provider.as_key()],
        )?;
        Ok(())
    }

    // ---- UI operation history ----

    pub fn save_ui_operation(
        &self,
        operation_id: &str,
        status: &str,
        payload_json: &str,
    ) -> Result<()> {
        let database = self.lock();
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

    pub fn read_ui_operation(&self, operation_id: &str) -> Result<Option<String>> {
        self.lock()
            .query_row(
                "SELECT payload_json FROM ui_operations WHERE id = ?1",
                params![operation_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_ui_operations(&self) -> Result<Vec<String>> {
        let database = self.lock();
        let mut statement =
            database.prepare("SELECT payload_json FROM ui_operations ORDER BY updated_at DESC")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        let mut operations = Vec::new();
        for row in rows {
            operations.push(row?);
        }
        Ok(operations)
    }
}

#[cfg(unix)]
fn harden_runtime_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(windows)]
fn harden_runtime_file_permissions(path: &Path) -> Result<()> {
    // No std API for Windows ACLs, so shell out to icacls: drop inherited ACEs
    // and grant only the current user, mirroring the Unix 0600 intent. Best
    // effort — if the user can't be resolved or icacls is unavailable, the file
    // keeps the (owner+admins) ACLs it inherits from the profile directory,
    // which is no worse than before. Never fatal.
    let Ok(user) = std::env::var("USERNAME") else {
        return Ok(());
    };
    if user.is_empty() {
        return Ok(());
    }
    let _ = std::process::Command::new("icacls")
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(format!("{user}:F"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn harden_runtime_file_permissions(_path: &Path) -> Result<()> {
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

/// One-shot migration of provider credentials and UI operation history out of
/// the (older) canonical library database and into the runtime database, so
/// secrets no longer live alongside library data. Guarded by a marker in
/// `runtime_metadata` so it runs at most once.
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
        // Strip the migrated tables from the canonical database BEFORE taking the
        // safety snapshot, so the pre-migration backup in dump/backups/ never
        // retains cleartext provider credentials. The credentials were already
        // copied into runtime.db above, so the scrubbed snapshot still backs up
        // all canonical library data without leaking secrets at rest.
        if has_connections {
            library.execute("DELETE FROM provider_connections", [])?;
        }
        if has_operations {
            library.execute("DELETE FROM ui_operations", [])?;
        }
        checkpoint(&library)?;
        backups::snapshot_existing_database(root, &library_path)?;
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

fn clear_expired_provider_cooldowns(connection: &Connection) -> Result<()> {
    connection.execute(
        "DELETE FROM provider_cooldowns WHERE blocked_until <= ?1",
        params![encode_datetime(&Utc::now())],
    )?;
    Ok(())
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
