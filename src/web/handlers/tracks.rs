use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::Utc;

use crate::domain::{LinkSource, SyncStatusRecord, TrackIdentityApplyResult, TrackMetadata};

use crate::web::artwork::schedule_artwork_enrichment;
use crate::web::conflicts::*;
use crate::web::dto::*;
use crate::web::error::*;
use crate::web::mutations::*;
use crate::web::parse::normalize_manual_provider_track_id;
use crate::web::projections::*;
use crate::web::providers::*;
use crate::web::{persist_library, AppContext, PAGE_SIZE};

pub(crate) async fn api_tracks(
    State(context): State<Arc<AppContext>>,
    Query(query): Query<TracksQuery>,
) -> Result<Json<PageResponse<TrackListItemDto>>, ApiError> {
    let page = normalized_page(query.page);
    let (page_rows, total, track_ids) = {
        let state = context.library.read().await;
        let rows = track_rows(&state, query.q.as_deref(), query.coverage.as_deref());
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

pub(crate) async fn api_track_detail(
    State(context): State<Arc<AppContext>>,
    Path(track_id): Path<String>,
) -> Result<Json<TrackDetailDto>, ApiError> {
    let detail = {
        let state = context.library.read().await;
        build_track_detail(&state, &track_id)?
    };
    schedule_artwork_enrichment(&context, vec![track_id]);
    Ok(Json(detail))
}

pub(crate) async fn api_update_track(
    State(context): State<Arc<AppContext>>,
    Path(track_id): Path<String>,
    Json(request): Json<UpdateTrackRequest>,
) -> Result<Json<MessageResponse>, ApiError> {
    let metadata = TrackMetadata {
        title: request.title,
        artists: request
            .artists
            .into_iter()
            .map(|artist| artist.trim().to_string())
            .filter(|artist| !artist.is_empty())
            .collect(),
        album: normalize_optional_text(request.album),
        duration_seconds: request.duration_seconds,
        isrc: normalize_optional_text(request.isrc),
    };
    let mut state = context.library.write().await;
    state
        .update_track_metadata(&track_id, metadata)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    persist_library(&state).await?;
    context.bump_library_version();
    Ok(Json(MessageResponse::new(
        "Updated canonical track metadata.",
    )))
}

pub(crate) async fn api_apply_track_identity(
    State(context): State<Arc<AppContext>>,
    Path(track_id): Path<String>,
    Json(request): Json<ApplyTrackIdentityRequest>,
) -> Result<Json<ApplyTrackIdentityResponse>, ApiError> {
    let provider_id = normalize_manual_provider_track_id(request.provider, &request.provider_id)?;
    let now = Utc::now();
    let mut state = context.library.write().await;
    let result = state
        .apply_track_identity(
            &track_id,
            request.provider,
            provider_id.clone(),
            LinkSource::Manual,
            Some(1.0),
            now,
        )
        .map_err(|error| ApiError::bad_request(sanitize_error_message(&error.to_string())))?;
    let result_track_id = result.track_id().to_string();
    state.set_track_status(
        &result_track_id,
        request.provider,
        SyncStatusRecord::synced(
            Some(provider_id.clone()),
            Some(1.0),
            Some(format!(
                "Manually linked {} identity.",
                request.provider.display_name()
            )),
            now,
        ),
    );
    persist_library(&state).await?;
    context.bump_library_version();
    drop(state);

    let (result_key, message) = match &result {
        TrackIdentityApplyResult::Linked { .. } => (
            "linked",
            format!(
                "Linked {} ID {} to the canonical track.",
                request.provider.display_name(),
                provider_id
            ),
        ),
        TrackIdentityApplyResult::AlreadyLinked { .. } => (
            "already_linked",
            format!(
                "{} ID {} was already linked to this canonical track.",
                request.provider.display_name(),
                provider_id
            ),
        ),
        TrackIdentityApplyResult::Merged {
            source_track_id,
            target_track_id,
        } => (
            "merged",
            format!(
                "Merged canonical track {source_track_id} into {target_track_id} through {} ID {}.",
                request.provider.display_name(),
                provider_id
            ),
        ),
    };

    Ok(Json(ApplyTrackIdentityResponse {
        message,
        result: result_key.to_string(),
        provider: request.provider.as_key().to_string(),
        provider_id,
        track_id: result_track_id,
    }))
}

pub(crate) async fn api_merge_track(
    State(context): State<Arc<AppContext>>,
    Path(source_track_id): Path<String>,
    Json(request): Json<MergeTrackRequest>,
) -> Result<Json<MergeTrackResponse>, ApiError> {
    let now = Utc::now();
    let resolution = track_merge_conflict_resolution(request.conflict_resolution);
    let mut state = context.library.write().await;
    let result = state
        .merge_track_into_resolving_conflicts(
            &source_track_id,
            &request.target_track_id,
            resolution,
            now,
        )
        .map_err(|error| ApiError::bad_request(sanitize_error_message(&error.to_string())))?;
    state.validate().map_err(ApiError::from)?;
    persist_library(&state).await?;
    context.bump_library_version();
    drop(state);

    let resolved_conflicts = resolved_provider_conflict_dtos(&result.resolved_conflicts);
    let conflict_count = resolved_conflicts.len();

    Ok(Json(MergeTrackResponse {
        message: format!(
            "Merged canonical track {} into {} and resolved {} provider ID conflict(s). Provider accounts were not changed.",
            result.source_track_id, result.target_track_id, conflict_count
        ),
        source_track_id: result.source_track_id,
        target_track_id: result.target_track_id,
        resolved_conflicts,
    }))
}

pub(crate) async fn api_delete_track(
    State(context): State<Arc<AppContext>>,
    Path(track_id): Path<String>,
) -> Result<Json<MessageResponse>, ApiError> {
    let connections = read_provider_connections().await?;
    let (mut working, linked_track_ids, affected_playlist_ids) = {
        let mut state = context.library.write().await;
        let linked_track_ids = track_provider_links(&state, &track_id)?;
        let affected_playlist_ids = playlist_ids_for_track(&state, &track_id);
        if !state.remove_track_everywhere(&track_id) {
            return Err(ApiError::not_found(format!("Unknown track '{track_id}'.")));
        }
        persist_library(&state).await?;
        context.bump_library_version();
        (state.clone(), linked_track_ids, affected_playlist_ids)
    };

    // Provider propagation runs off-lock against the detached clone.
    let mut warnings = propagate_saved_track_delete(&connections, &linked_track_ids).await;
    warnings.extend(
        propagate_playlist_subset_to_connected_providers(
            &connections,
            &mut working,
            &affected_playlist_ids,
        )
        .await,
    );

    // Reconcile the affected playlists' provider bookkeeping back into the
    // current state (scoped to those playlists, so concurrent edits elsewhere
    // survive), then persist.
    if !affected_playlist_ids.is_empty() {
        let synced = playlist_subset_state(&working, &affected_playlist_ids);
        let mut state = context.library.write().await;
        merge_subset_state(&mut state, &synced);
        persist_library(&state).await?;
        context.bump_library_version();
    }

    Ok(Json(MessageResponse::with_warnings(
        "Removed track from saved tracks and playlists.",
        warnings,
    )))
}
