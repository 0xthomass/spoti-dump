use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::Json;

use crate::web::artwork::schedule_artwork_enrichment;
use crate::web::dto::*;
use crate::web::error::*;
use crate::web::mutations::*;
use crate::web::projections::*;
use crate::web::providers::*;
use crate::web::{persist_library, AppContext, PAGE_SIZE};

pub(crate) async fn api_playlists(
    State(context): State<Arc<AppContext>>,
    Query(query): Query<PlaylistsQuery>,
) -> Result<Json<PageResponse<PlaylistSummaryDto>>, ApiError> {
    let page = normalized_page(query.page);
    let (page_rows, total, track_ids) = {
        let state = context.library.read().await;
        let rows = playlist_summaries(&state, query.q.as_deref());
        let page_rows = paginate_vec(&rows, page, PAGE_SIZE);
        let playlist_ids = page_rows
            .iter()
            .map(|item| item.playlist_id.clone())
            .collect::<Vec<_>>();
        let track_ids = playlist_artwork_track_ids(&state, &playlist_ids);
        (page_rows, rows.len(), track_ids)
    };
    schedule_artwork_enrichment(&context, track_ids);
    Ok(Json(PageResponse::new(page_rows, total, page, PAGE_SIZE)))
}

pub(crate) async fn api_playlist_detail(
    State(context): State<Arc<AppContext>>,
    Path(playlist_id): Path<String>,
) -> Result<Json<PlaylistDetailDto>, ApiError> {
    let (detail, track_ids) = {
        let state = context.library.read().await;
        let track_ids = state
            .playlists
            .iter()
            .find(|playlist| playlist.id == playlist_id)
            .map(|playlist| {
                playlist
                    .entries
                    .iter()
                    .map(|entry| entry.track_id.clone())
                    .collect::<Vec<_>>()
            })
            .ok_or_else(|| ApiError::not_found(format!("Unknown playlist '{playlist_id}'.")))?;
        let detail = build_playlist_detail(&state, &playlist_id)?;
        (detail, track_ids)
    };
    schedule_artwork_enrichment(&context, track_ids);
    Ok(Json(detail))
}

pub(crate) async fn api_update_playlist(
    State(context): State<Arc<AppContext>>,
    Path(playlist_id): Path<String>,
    Json(request): Json<UpdatePlaylistRequest>,
) -> Result<Json<MessageResponse>, ApiError> {
    let mut state = context.library.write().await;
    state
        .update_playlist_details(
            &playlist_id,
            request.name,
            normalize_optional_text(request.description),
        )
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    persist_library(&state).await?;
    context.bump_library_version();
    Ok(Json(MessageResponse::new(
        "Updated canonical playlist details.",
    )))
}

pub(crate) async fn api_delete_playlist(
    State(context): State<Arc<AppContext>>,
    Path(playlist_id): Path<String>,
) -> Result<Json<MessageResponse>, ApiError> {
    let connections = read_provider_connections().await?;
    let linked_playlist_ids = {
        let mut state = context.library.write().await;
        let linked_playlist_ids = playlist_provider_links(&state, &playlist_id)?;
        if !state.remove_playlist(&playlist_id) {
            return Err(ApiError::not_found(format!(
                "Unknown playlist '{playlist_id}'."
            )));
        }
        persist_library(&state).await?;
        context.bump_library_version();
        linked_playlist_ids
    };
    let warnings = propagate_playlist_delete(&connections, &linked_playlist_ids).await;
    Ok(Json(MessageResponse::with_warnings(
        "Deleted playlist from the canonical library.",
        warnings,
    )))
}

pub(crate) async fn api_delete_playlist_entry(
    State(context): State<Arc<AppContext>>,
    Path((playlist_id, entry_id)): Path<(String, String)>,
) -> Result<Json<MessageResponse>, ApiError> {
    let connections = read_provider_connections().await?;
    let mut working = {
        let mut state = context.library.write().await;
        if !state.remove_playlist_entry(&playlist_id, &entry_id) {
            return Err(ApiError::not_found(format!(
                "Unknown playlist entry '{entry_id}' for playlist '{playlist_id}'."
            )));
        }
        persist_library(&state).await?;
        context.bump_library_version();
        state.clone()
    };
    let warnings = propagate_playlist_subset_to_connected_providers(
        &connections,
        &mut working,
        std::slice::from_ref(&playlist_id),
    )
    .await;
    // Reconcile the affected playlist's provider bookkeeping back into current.
    let synced = playlist_subset_state(&working, std::slice::from_ref(&playlist_id));
    {
        let mut state = context.library.write().await;
        merge_subset_state(&mut state, &synced);
        persist_library(&state).await?;
        context.bump_library_version();
    }
    Ok(Json(MessageResponse::with_warnings(
        "Removed playlist entry from the canonical library.",
        warnings,
    )))
}
