use std::sync::Arc;

use anyhow::Context;
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::Utc;

use crate::web::artwork::schedule_artwork_enrichment;
use crate::web::conflicts::*;
use crate::web::dto::*;
use crate::web::error::*;
use crate::web::projections::*;
use crate::web::{persist_library, AppContext, PAGE_SIZE};

use crate::storage;

const BULK_IDENTITY_CONFLICT_EXAMPLE_LIMIT: usize = 10;
const BULK_IDENTITY_CONFLICT_MERGE_LIMIT: usize = 250;

pub(crate) async fn api_identity_conflicts(
    State(context): State<Arc<AppContext>>,
    Query(query): Query<IdentityConflictsQuery>,
) -> Result<Json<PageResponse<TrackIdentityConflictQueueItemDto>>, ApiError> {
    let page = normalized_page(query.page);
    let filters = IdentityConflictFilters {
        query: query.q.as_deref(),
        provider: query.provider,
        recommendation: query.recommendation.as_deref(),
        impact: query.impact.as_deref(),
    };
    let (page_rows, total, track_ids) = {
        let state = context.library.read().await;
        let rows = identity_conflict_rows_filtered(&state, filters);
        let page_rows = paginate_vec(&rows, page, PAGE_SIZE);
        let track_ids = page_rows
            .iter()
            .flat_map(|item| {
                [
                    item.source_track.track_id.clone(),
                    item.conflict.owner_track.track_id.clone(),
                ]
            })
            .collect::<Vec<_>>();
        (page_rows, rows.len(), track_ids)
    };
    schedule_artwork_enrichment(&context, track_ids);
    Ok(Json(PageResponse::new(page_rows, total, page, PAGE_SIZE)))
}

pub(crate) async fn api_identity_gaps(
    State(context): State<Arc<AppContext>>,
    Query(query): Query<IdentityGapsQuery>,
) -> Result<Json<PageResponse<TrackIdentityGapQueueItemDto>>, ApiError> {
    let page = normalized_page(query.page);
    let (page_rows, total, track_ids) = {
        let state = context.library.read().await;
        let rows = identity_gap_rows(&state, query.provider, query.q.as_deref());
        let page_rows = paginate_vec(&rows, page, PAGE_SIZE);
        let track_ids = page_rows
            .iter()
            .map(|item| item.track.track_id.clone())
            .collect::<Vec<_>>();
        (page_rows, rows.len(), track_ids)
    };
    schedule_artwork_enrichment(&context, track_ids);
    Ok(Json(PageResponse::new(page_rows, total, page, PAGE_SIZE)))
}

pub(crate) async fn api_identity_conflicts_bulk_merge_plan(
    State(context): State<Arc<AppContext>>,
    Query(query): Query<BulkMergeIdentityConflictsPlanQuery>,
) -> Result<Json<BulkMergeIdentityConflictsPlanDto>, ApiError> {
    let state = context.library.read().await;
    let rows = bulk_merge_identity_conflict_rows(
        &state,
        query.q.as_deref(),
        query.provider,
        query.impact.as_deref(),
    );

    Ok(Json(BulkMergeIdentityConflictsPlanDto {
        eligible_count: rows.len(),
        examples: rows
            .into_iter()
            .take(BULK_IDENTITY_CONFLICT_EXAMPLE_LIMIT)
            .collect(),
        warnings: bulk_merge_identity_conflict_warnings(),
    }))
}

pub(crate) async fn api_identity_conflicts_bulk_merge(
    State(context): State<Arc<AppContext>>,
    Json(request): Json<BulkMergeIdentityConflictsRequest>,
) -> Result<Json<BulkMergeIdentityConflictsResponse>, ApiError> {
    let mut state = context.library.write().await;
    let candidates = bulk_merge_identity_conflict_rows(
        &state,
        request.q.as_deref(),
        request.provider,
        request.impact.as_deref(),
    );
    let eligible_count = candidates.len();
    let max_merges = request
        .max_merges
        .unwrap_or(BULK_IDENTITY_CONFLICT_MERGE_LIMIT)
        .min(BULK_IDENTITY_CONFLICT_MERGE_LIMIT);
    let resolution = track_merge_conflict_resolution(request.conflict_resolution);
    // Only snapshot when there is something to merge; an empty plan must not
    // accumulate no-op backups in the manual-backups directory.
    let backup = if eligible_count > 0 {
        Some(
            tokio::task::spawn_blocking(storage::create_manual_library_backup)
                .await
                .context("Failed to join pre-merge backup task")?
                .map_err(ApiError::from)?,
        )
    } else {
        None
    };

    let mut merged_examples = Vec::new();
    let mut merged_count = 0_usize;
    let mut skipped_count = eligible_count.saturating_sub(max_merges);
    let mut resolved_provider_conflicts = 0_usize;
    let mut warnings = bulk_merge_identity_conflict_warnings();

    for candidate in candidates.into_iter().take(max_merges) {
        // Re-validate each candidate against the current (mutating) state, using
        // freshly built indexes because every merge changes the track set.
        let active_conflict = {
            let indexes = ConflictIndexes::build(&state);
            active_bulk_merge_identity_conflict(
                &state,
                &candidate.source_track.track_id,
                &candidate.conflict.owner_track.track_id,
                &candidate.conflict.provider,
                &candidate.conflict.provider_id,
                &indexes,
            )
        };
        let Some(active_conflict) = active_conflict else {
            skipped_count += 1;
            continue;
        };

        let result = match state.merge_track_into_resolving_conflicts(
            &active_conflict.source_track.track_id,
            &active_conflict.conflict.owner_track.track_id,
            resolution,
            Utc::now(),
        ) {
            Ok(result) => result,
            Err(error) => {
                skipped_count += 1;
                warnings.push(format!(
                    "Skipped {} because it could no longer be merged safely: {}",
                    active_conflict.source_track.title,
                    sanitize_error_message(&error.to_string())
                ));
                continue;
            }
        };

        let resolved_conflicts = resolved_provider_conflict_dtos(&result.resolved_conflicts);
        resolved_provider_conflicts += resolved_conflicts.len();
        merged_count += 1;
        if merged_examples.len() < BULK_IDENTITY_CONFLICT_EXAMPLE_LIMIT {
            merged_examples.push(BulkMergedIdentityConflictDto {
                source_track_id: result.source_track_id,
                target_track_id: result.target_track_id,
                title: active_conflict.source_track.title,
                provider: active_conflict.conflict.provider,
                provider_id: active_conflict.conflict.provider_id,
                resolved_conflicts,
            });
        }
    }

    state.validate().map_err(ApiError::from)?;
    persist_library(&state).await?;
    context.bump_library_version();
    drop(state);

    Ok(Json(BulkMergeIdentityConflictsResponse {
        message: format!(
            "Merged {merged_count} likely-same identity conflict(s). Provider accounts were not changed."
        ),
        eligible_count,
        merged_count,
        skipped_count,
        resolved_provider_conflicts,
        conflict_resolution: merge_conflict_resolution_key(request.conflict_resolution).to_string(),
        conflict_resolution_label: merge_conflict_resolution_label(request.conflict_resolution)
            .to_string(),
        pre_merge_backup_path: backup.map(|backup| backup.path.display().to_string()),
        merged_examples,
        warnings,
    }))
}

pub(crate) async fn api_reject_track_identity_conflict(
    State(context): State<Arc<AppContext>>,
    Path(track_id): Path<String>,
    Json(request): Json<RejectTrackIdentityConflictRequest>,
) -> Result<Json<MessageResponse>, ApiError> {
    let provider_id = request.provider_id.trim().to_string();
    if provider_id.is_empty() {
        return Err(ApiError::bad_request("Provider ID cannot be empty."));
    }

    let mut state = context.library.write().await;
    let indexes = ConflictIndexes::build(&state);
    let track = state
        .tracks
        .iter()
        .find(|track| track.id == track_id)
        .ok_or_else(|| ApiError::not_found(format!("Unknown track '{track_id}'.")))?;
    let conflict = identity_conflicts_for_track(track, &indexes)
        .into_iter()
        .find(|conflict| {
            conflict.provider == request.provider.as_key()
                && conflict.provider_id == provider_id
                && conflict.owner_track.track_id == request.owner_track_id
        })
        .ok_or_else(|| {
            ApiError::bad_request(
                "That identity conflict is no longer active for this track.".to_string(),
            )
        })?;
    let provider_name = conflict.provider_name.clone();
    let candidate_provider_id = conflict.provider_id.clone();
    drop(indexes);
    let now = Utc::now();
    // Flip the typed conflict into a permanent rejected tombstone so the
    // candidate is never re-proposed by future identity syncs.
    if !state.reject_track_identity_conflict(
        &track_id,
        request.provider,
        &candidate_provider_id,
        now,
    ) {
        return Err(ApiError::bad_request(
            "That identity conflict is no longer active for this track.".to_string(),
        ));
    }
    persist_library(&state).await?;
    context.bump_library_version();
    drop(state);

    Ok(Json(MessageResponse::new(format!(
        "Marked {provider_name} candidate {candidate_provider_id} as not the same track. The source row remains unresolved for that provider."
    ))))
}
