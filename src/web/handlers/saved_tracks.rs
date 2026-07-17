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

pub(crate) async fn api_saved_tracks(
    State(context): State<Arc<AppContext>>,
    Query(query): Query<SavedTracksQuery>,
) -> Result<Json<PageResponse<SavedTrackItemDto>>, ApiError> {
    let page = normalized_page(query.page);
    let (page_rows, total, track_ids) = {
        let state = context.library.read().await;
        let rows = saved_track_rows(&state, query.q.as_deref());
        let page_rows = paginate_vec(&rows, page, PAGE_SIZE);
        let track_ids = page_rows
            .iter()
            .map(|item| item.track_id.clone())
            .collect::<Vec<_>>();
        (page_rows, rows.len(), track_ids)
    };
    schedule_artwork_enrichment(&context, track_ids);
    Ok(Json(PageResponse::new(page_rows, total, page, PAGE_SIZE)))
}

pub(crate) async fn api_delete_saved_track(
    State(context): State<Arc<AppContext>>,
    Path(saved_track_id): Path<String>,
) -> Result<Json<MessageResponse>, ApiError> {
    let connections = read_provider_connections().await?;
    let linked_track_ids = {
        let mut state = context.library.write().await;
        let linked_track_ids = saved_track_provider_links(&state, &saved_track_id)?;
        if !state.remove_saved_track(&saved_track_id) {
            return Err(ApiError::not_found(format!(
                "Unknown saved track '{saved_track_id}'."
            )));
        }
        persist_library(&state).await?;
        context.bump_library_version();
        linked_track_ids
    };
    // Provider network propagation runs with no library lock held.
    let warnings = propagate_saved_track_delete(&connections, &linked_track_ids).await;
    Ok(Json(MessageResponse::with_warnings(
        "Removed saved track from the canonical library.",
        warnings,
    )))
}
