use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::ProviderKind;
use crate::provider::{ProgressHandler, ProviderProgress};
use crate::providers::policy;
use crate::storage;

use super::dto::{MessageResponse, OperationResponse};
use super::error::ApiError;
use super::providers::{
    ensure_provider_health_allows_operation, ensure_provider_not_cooling_down,
    provider_health_failed, provider_health_ok,
};
use super::{runtime_db, AppContext};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OperationStatus {
    Running,
    Succeeded,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OperationKind {
    Verify,
    Pull,
    Push,
    ResetPush,
    Identity,
    IdentityAll,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct OperationRecord {
    pub(crate) id: String,
    pub(crate) provider: ProviderKind,
    pub(crate) kind: OperationKind,
    pub(crate) status: OperationStatus,
    pub(crate) stage: String,
    pub(crate) detail: Option<String>,
    pub(crate) saved_tracks_done: usize,
    pub(crate) saved_tracks_total: Option<usize>,
    pub(crate) playlists_done: usize,
    pub(crate) playlists_total: Option<usize>,
    pub(crate) playlist_entries_done: usize,
    pub(crate) playlist_entries_total: Option<usize>,
    pub(crate) message: Option<String>,
    pub(crate) warnings: Vec<String>,
    pub(crate) error: Option<String>,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) finished_at: Option<DateTime<Utc>>,
    #[serde(skip)]
    pub(crate) last_persisted_at: Option<DateTime<Utc>>,
}

pub(crate) async fn insert_operation(
    context: &Arc<AppContext>,
    mut operation: OperationRecord,
) -> Result<(), ApiError> {
    if !matches!(operation.kind, OperationKind::IdentityAll) {
        ensure_provider_not_cooling_down(operation.provider).await?;
    }
    if !matches!(
        operation.kind,
        OperationKind::Verify | OperationKind::IdentityAll
    ) {
        ensure_provider_health_allows_operation(operation.provider).await?;
    }
    let persisted = {
        let mut operations = context
            .operations
            .lock()
            .map_err(|_| ApiError::internal("Failed to inspect running operations."))?;
        if operations
            .values()
            .any(|operation| operation.status == OperationStatus::Running)
        {
            return Err(ApiError::bad_request(
                "Another provider operation is already running. Wait for it to finish first.",
            ));
        }
        operation.last_persisted_at = Some(Utc::now());
        operations.insert(operation.id.clone(), operation.clone());
        operation
    };
    runtime_db(move || persist_operation_blocking(&persisted)).await
}

/// Serializes and persists an operation record to `runtime.db`. Runs on a
/// blocking thread via [`runtime_db`] or [`tokio::task::spawn_blocking`]; never
/// call it directly from an async context.
pub(crate) fn persist_operation_blocking(operation: &OperationRecord) -> Result<()> {
    let payload_json = serde_json::to_string(operation)?;
    storage::save_ui_operation_json(
        &operation.id,
        operation_status_key(operation.status),
        &payload_json,
    )
}

pub(crate) fn persist_operation(operation: &OperationRecord) -> Result<(), ApiError> {
    persist_operation_blocking(operation).map_err(ApiError::from)
}

pub(crate) fn load_recovered_operations() -> Result<HashMap<String, OperationRecord>> {
    let mut operations = HashMap::new();
    for payload_json in storage::list_ui_operation_json()? {
        let mut operation: OperationRecord = serde_json::from_str(&payload_json)
            .context("Failed to parse persisted UI operation history")?;
        if operation.status == OperationStatus::Running {
            operation.status = OperationStatus::Failed;
            operation.stage = "Interrupted".to_string();
            operation.error = Some(
                "The app stopped before this operation finished. Review the canonical state and start the operation again."
                    .to_string(),
            );
            operation.finished_at = Some(Utc::now());
            persist_operation(&operation).map_err(|error| anyhow::anyhow!(error.message))?;
        }
        operations.insert(operation.id.clone(), operation);
    }
    Ok(operations)
}

pub(crate) fn operation_status_key(status: OperationStatus) -> &'static str {
    match status {
        OperationStatus::Running => "running",
        OperationStatus::Succeeded => "succeeded",
        OperationStatus::Failed => "failed",
    }
}

pub(crate) fn progress_handler_for_operation(
    context: Arc<AppContext>,
    operation_id: String,
) -> ProgressHandler {
    Arc::new(move |progress: ProviderProgress| {
        let persist = if let Ok(mut operations) = context.operations.lock() {
            if let Some(operation) = operations.get_mut(&operation_id) {
                operation.stage = progress.stage;
                operation.detail = progress.detail;
                operation.saved_tracks_done = progress.saved_tracks_done;
                operation.saved_tracks_total = progress.saved_tracks_total;
                operation.playlists_done = progress.playlists_done;
                operation.playlists_total = progress.playlists_total;
                operation.playlist_entries_done = progress.playlist_entries_done;
                operation.playlist_entries_total = progress.playlist_entries_total;
                let now = Utc::now();
                let should_persist = operation
                    .last_persisted_at
                    .map(|last| (now - last).num_seconds() >= 1)
                    .unwrap_or(true);
                if should_persist {
                    operation.last_persisted_at = Some(now);
                    Some(operation.clone())
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        if let Some(operation) = persist {
            // Persist on a blocking thread so the provider's network loop (which
            // invokes this synchronous callback) is never parked on SQLite.
            tokio::task::spawn_blocking(move || {
                if let Err(error) = persist_operation_blocking(&operation) {
                    eprintln!("Failed to persist operation progress: {error}");
                }
            });
        }
    })
}

pub(crate) async fn finish_operation(
    context: Arc<AppContext>,
    operation_id: &str,
    result: Result<MessageResponse, ApiError>,
) {
    let mut cooldown_to_save = None;
    let mut cooldown_to_clear = None;
    let mut health_to_save = None;
    let persisted = if let Ok(mut operations) = context.operations.lock() {
        if let Some(operation) = operations.get_mut(operation_id) {
            match result {
                Ok(response) => {
                    if !matches!(operation.kind, OperationKind::IdentityAll) {
                        cooldown_to_clear = Some(operation.provider);
                        health_to_save = Some(provider_health_ok(
                            operation.provider,
                            format!("{} operation succeeded.", operation.provider.display_name()),
                        ));
                    }
                    operation.status = OperationStatus::Succeeded;
                    operation.stage = "Done".to_string();
                    operation.message = Some(response.message);
                    operation.warnings = response.warnings;
                    operation.finished_at = Some(Utc::now());
                }
                Err(error) => {
                    if !matches!(operation.kind, OperationKind::IdentityAll) {
                        if let Some(source) = &error.source {
                            cooldown_to_save =
                                policy::cooldown_from_error(operation.provider, source);
                        }
                        if matches!(operation.kind, OperationKind::Verify)
                            || error
                                .source
                                .as_ref()
                                .is_some_and(policy::is_connection_health_failure)
                        {
                            health_to_save = Some(provider_health_failed(
                                operation.provider,
                                error.message.clone(),
                            ));
                        }
                    }
                    operation.status = OperationStatus::Failed;
                    operation.stage = "Failed".to_string();
                    operation.error = Some(error.message);
                    operation.finished_at = Some(Utc::now());
                    if let Some(cooldown) = &cooldown_to_save {
                        operation.warnings.push(format!(
                            "{} will be held until {} to avoid hammering the provider.",
                            operation.provider.display_name(),
                            cooldown.blocked_until.to_rfc3339()
                        ));
                    }
                }
            }
            operation.last_persisted_at = Some(Utc::now());
            Some(operation.clone())
        } else {
            None
        }
    } else {
        None
    };
    // The remaining work is all blocking runtime-database I/O; run it on a
    // blocking thread so this finaliser never parks the async worker on SQLite.
    let _ = tokio::task::spawn_blocking(move || {
        if let Some(provider) = cooldown_to_clear {
            if let Err(error) = storage::clear_provider_cooldown(provider) {
                eprintln!("Failed to clear provider cooldown: {error}");
            }
        }
        if let Some(cooldown) = cooldown_to_save {
            if let Err(error) = storage::save_provider_cooldown(&cooldown) {
                eprintln!("Failed to persist provider cooldown: {error}");
            }
        }
        if let Some(health) = health_to_save {
            if let Err(error) = storage::save_provider_health(&health) {
                eprintln!("Failed to persist provider health: {error}");
            }
        }
        if let Some(operation) = persisted {
            if let Err(error) = persist_operation_blocking(&operation) {
                eprintln!("Failed to persist finished operation: {error}");
            }
        }
    })
    .await;
}

pub(crate) fn operation_response(operation: &OperationRecord) -> OperationResponse {
    let (provider_key, provider_name) = if matches!(operation.kind, OperationKind::IdentityAll) {
        ("library".to_string(), "Library".to_string())
    } else {
        (
            operation.provider.as_key().to_string(),
            operation.provider.display_name().to_string(),
        )
    };

    OperationResponse {
        operation_id: operation.id.clone(),
        provider_key,
        provider_name,
        kind: operation.kind,
        status: operation.status,
        stage: operation.stage.clone(),
        detail: operation.detail.clone(),
        saved_tracks_done: operation.saved_tracks_done,
        saved_tracks_total: operation.saved_tracks_total,
        playlists_done: operation.playlists_done,
        playlists_total: operation.playlists_total,
        playlist_entries_done: operation.playlist_entries_done,
        playlist_entries_total: operation.playlist_entries_total,
        message: operation.message.clone(),
        warnings: operation.warnings.clone(),
        error: operation.error.clone(),
        started_at: operation.started_at.to_rfc3339(),
        finished_at: operation.finished_at.map(|at| at.to_rfc3339()),
    }
}
