use std::sync::Arc;

use anyhow::Context;
use axum::extract::State;
use axum::Json;

use crate::web::dto::*;
use crate::web::error::*;
use crate::web::AppContext;

use crate::storage;

pub(crate) async fn api_backups() -> Result<Json<BackupsResponse>, ApiError> {
    let backups = tokio::task::spawn_blocking(storage::list_library_backups)
        .await
        .context("Failed to join backup listing task")?
        .map_err(ApiError::from)?;
    Ok(Json(BackupsResponse {
        automatic_backup_dir: storage::automatic_backup_dir().display().to_string(),
        manual_backup_dir: storage::manual_backup_dir().display().to_string(),
        backups: backups.into_iter().map(backup_dto).collect(),
    }))
}

pub(crate) async fn api_create_manual_backup() -> Result<Json<CreateBackupResponse>, ApiError> {
    let backup = tokio::task::spawn_blocking(storage::create_manual_library_backup)
        .await
        .context("Failed to join manual backup task")?
        .map_err(ApiError::from)?;
    Ok(Json(CreateBackupResponse {
        message: format!(
            "Created manual source-of-truth backup at {}.",
            backup.path.display()
        ),
        backup: backup_dto(backup),
    }))
}

pub(crate) async fn api_restore_backup(
    State(context): State<Arc<AppContext>>,
    Json(request): Json<RestoreBackupRequest>,
) -> Result<Json<RestoreBackupResponse>, ApiError> {
    let backup_type = request.backup_type;
    let file_name = request.file_name;
    // A restore replaces the entire canonical database, so it must exclude all
    // other access: hold the write lock across the (local, blocking) restore and
    // the reload that refreshes the in-memory copy from the restored file.
    let mut guard = context.library.write().await;
    let summary = tokio::task::spawn_blocking(move || {
        storage::restore_library_backup(&backup_type, &file_name)
    })
    .await
    .context("Failed to join restore backup task")?
    .map_err(|error| ApiError::bad_request(sanitize_error_message(&error.to_string())))?;
    let reloaded = tokio::task::spawn_blocking(storage::read_library_state)
        .await
        .context("Failed to join post-restore reload task")?
        .map_err(ApiError::from)?;
    *guard = reloaded;
    context.bump_library_version();
    drop(guard);

    Ok(Json(RestoreBackupResponse {
        message: format!(
            "Restored {} from backup. A pre-restore manual backup was saved at {}.",
            storage::library_state_path().display(),
            summary.pre_restore_backup.path.display()
        ),
        restored_backup: backup_dto(summary.restored_backup),
        pre_restore_backup: backup_dto(summary.pre_restore_backup),
    }))
}

pub(crate) fn backup_dto(backup: storage::LibraryBackup) -> BackupDto {
    BackupDto {
        file_name: backup.file_name,
        path: backup.path.display().to_string(),
        backup_type: backup.backup_type.to_string(),
        size_bytes: backup.size_bytes,
        modified_at: backup.modified_at.map(|value| value.to_rfc3339()),
    }
}
