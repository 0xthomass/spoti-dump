use std::sync::Arc;

use anyhow::Context;
use axum::extract::State;
use axum::Json;

use crate::web::dto::*;
use crate::web::error::*;
use crate::web::projections::*;
use crate::web::AppContext;

use crate::storage;

pub(crate) async fn api_overview(
    State(context): State<Arc<AppContext>>,
) -> Result<Json<OverviewResponse>, ApiError> {
    let state = context.library.read().await;
    Ok(Json(overview_payload(&state)))
}

pub(crate) async fn api_health() -> Result<Json<HealthResponse>, ApiError> {
    let health = tokio::task::spawn_blocking(storage::database_health)
        .await
        .context("Failed to join database health task")?
        .map_err(ApiError::from)?;
    let status = if health.integrity_check == "ok" {
        "ok"
    } else {
        "degraded"
    };
    Ok(Json(HealthResponse {
        status,
        database_path: health.path.display().to_string(),
        integrity_check: health.integrity_check,
        tracks: health.tracks,
        saved_tracks: health.saved_tracks,
        playlists: health.playlists,
        playlist_entries: health.playlist_entries,
        durable_operation_history: true,
    }))
}
