use std::sync::Arc;

use anyhow::Context;
use axum::extract::{Path, State};
use axum::Json;
use chrono::Utc;

use crate::domain::{
    merge_provider_snapshot, LibraryState, ProviderConnection, ProviderConnectionConfig,
    ProviderKind, YoutubeMusicConnectionConfig,
};

use crate::web::conflicts::*;
use crate::web::dto::*;
use crate::web::error::*;
use crate::web::mutations::*;
use crate::web::operations::*;
use crate::web::providers::*;
use crate::web::{persist_library, runtime_db, AppContext, PendingSpotifyAuth};

use crate::storage;

use crate::provider::{ProviderCapability, ProviderProgress};
use crate::providers::spotify::SpotifyProvider;
use crate::providers::youtube_music::YoutubeMusicProvider;
use uuid::Uuid;

pub(crate) async fn api_providers(
    State(context): State<Arc<AppContext>>,
) -> Result<Json<ProvidersResponse>, ApiError> {
    // Gather the runtime-database rows off-lock first, then read the in-memory
    // state without holding it across any await.
    let connections = read_provider_connections().await?;
    let cooldowns = runtime_db(storage::list_provider_cooldowns).await?;
    let healths = runtime_db(storage::list_provider_healths).await?;
    let state = context.library.read().await;
    Ok(Json(ProvidersResponse {
        spotify_redirect_uri: context.spotify_redirect_uri.clone(),
        providers: provider_connection_payloads(&state, &connections, &cooldowns, &healths),
    }))
}

pub(crate) async fn api_provider_preflight(
    State(context): State<Arc<AppContext>>,
    Path(provider): Path<ProviderKind>,
) -> Result<Json<ProviderPreflightDto>, ApiError> {
    let connections = read_provider_connections().await?;
    let cooldowns = runtime_db(storage::list_provider_cooldowns).await?;
    let healths = runtime_db(storage::list_provider_healths).await?;
    let connection = connections
        .iter()
        .find(|connection| connection.provider == provider);
    let cooldown = cooldowns
        .iter()
        .find(|cooldown| cooldown.provider == provider);
    let health = healths.iter().find(|health| health.provider == provider);

    let state = context.library.read().await;
    let identity_conflicts = identity_conflict_rows(&state, None).len();
    Ok(Json(provider_preflight_payload(
        &state,
        provider,
        connection,
        cooldown,
        health,
        identity_conflicts,
    )))
}

pub(crate) async fn api_provider_push_plan(
    State(context): State<Arc<AppContext>>,
    Path(provider): Path<ProviderKind>,
) -> Result<Json<ProviderPushPlanDto>, ApiError> {
    let connections = read_provider_connections().await?;
    let cooldowns = runtime_db(storage::list_provider_cooldowns).await?;
    let healths = runtime_db(storage::list_provider_healths).await?;
    let connection = connections
        .iter()
        .find(|connection| connection.provider == provider);
    let cooldown = cooldowns
        .iter()
        .find(|cooldown| cooldown.provider == provider);
    let health = healths.iter().find(|health| health.provider == provider);

    let state = context.library.read().await;
    Ok(Json(provider_push_plan_payload(
        &state, provider, connection, cooldown, health,
    )))
}

pub(crate) async fn api_start_provider_verify(
    State(context): State<Arc<AppContext>>,
    Path(provider): Path<ProviderKind>,
) -> Result<Json<OperationStartResponse>, ApiError> {
    let operation_id = Uuid::new_v4().to_string();
    insert_operation(
        &context,
        OperationRecord {
            id: operation_id.clone(),
            provider,
            kind: OperationKind::Verify,
            status: OperationStatus::Running,
            stage: "Checking connection".to_string(),
            detail: Some(provider.display_name().to_string()),
            saved_tracks_done: 0,
            saved_tracks_total: None,
            playlists_done: 0,
            playlists_total: None,
            playlist_entries_done: 0,
            playlist_entries_total: None,
            message: None,
            warnings: Vec::new(),
            error: None,
            started_at: Utc::now(),
            finished_at: None,
            last_persisted_at: None,
        },
    )
    .await?;

    let background_context = context.clone();
    let background_operation_id = operation_id.clone();
    tokio::spawn(async move {
        let result = async {
            let provider_client =
                build_connected_provider_allowing_failed_health(provider, ProviderCapability::Read)
                    .await?;
            match provider_client.verify_connection().await {
                Ok(()) => {
                    save_provider_health(provider_health_ok(
                        provider,
                        "Connection check succeeded.",
                    ))
                    .await?;
                    runtime_db(move || storage::clear_provider_cooldown(provider)).await?;
                    Ok::<MessageResponse, ApiError>(MessageResponse::new(format!(
                        "{} connection check succeeded.",
                        provider.display_name()
                    )))
                }
                Err(error) => {
                    let message = sanitize_error_message(&error.to_string());
                    save_provider_health(provider_health_failed(provider, message.clone())).await?;
                    Err(ApiError::bad_request(message).with_source(error))
                }
            }
        }
        .await;
        finish_operation(background_context, &background_operation_id, result).await;
    });

    Ok(Json(OperationStartResponse { operation_id }))
}

pub(crate) async fn api_start_spotify_connect(
    State(context): State<Arc<AppContext>>,
    Json(request): Json<SpotifyConnectStartRequest>,
) -> Result<Json<SpotifyConnectStartResponse>, ApiError> {
    let client_id = request.client_id.trim();
    let client_secret = request.client_secret.trim();
    if client_id.is_empty() || client_secret.is_empty() {
        return Err(ApiError::bad_request(
            "Spotify client ID and client secret are both required.",
        ));
    }

    let state = random_state();
    let redirect_uri = url::Url::parse(&context.spotify_redirect_uri)
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let authorization_url = SpotifyProvider::authorization_url(
        ProviderCapability::ReadWrite,
        client_id,
        &redirect_uri,
        &state,
    )
    .map_err(ApiError::from)?;

    context.pending_spotify_auth.lock().await.insert(
        state,
        PendingSpotifyAuth {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
        },
    );

    Ok(Json(SpotifyConnectStartResponse {
        authorization_url: authorization_url.to_string(),
    }))
}

pub(crate) async fn api_connect_youtube_music(
    Json(request): Json<YoutubeMusicConnectRequest>,
) -> Result<Json<MessageResponse>, ApiError> {
    let auth = ytmusicapi::BrowserAuth::from_json(request.headers_json.trim())
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let config = YoutubeMusicConnectionConfig {
        cookie: auth.cookie,
        x_goog_authuser: auth.x_goog_authuser,
        origin: Some(auth.origin),
    };
    if looks_like_placeholder_ytmusic_cookie(&config.cookie) {
        return Err(ApiError::bad_request(
            "Replace the sample YouTube Music cookie values with real browser headers before linking.",
        ));
    }
    if looks_like_placeholder_ytmusic_authuser(&config.x_goog_authuser) {
        return Err(ApiError::bad_request(
            "Replace the sample x-goog-authuser value with the exact account index from your browser request before linking.",
        ));
    }
    let provider = YoutubeMusicProvider::from_connection(&config)
        .map_err(|error| ApiError::bad_request(sanitize_error_message(&error.to_string())))?;
    provider
        .verify_connection()
        .await
        .map_err(|error| ApiError::bad_request(sanitize_error_message(&error.to_string())))?;

    let now = Utc::now();
    runtime_db(move || {
        storage::save_provider_connection(&ProviderConnection {
            provider: ProviderKind::YoutubeMusic,
            connected_at: now,
            updated_at: now,
            config: ProviderConnectionConfig::YoutubeMusic(config),
        })?;
        storage::clear_provider_cooldown(ProviderKind::YoutubeMusic)
    })
    .await?;
    save_provider_health(provider_health_ok(
        ProviderKind::YoutubeMusic,
        "Connection verified during YouTube Music link.",
    ))
    .await?;

    Ok(Json(MessageResponse::new(
        "Linked YouTube Music. Test call succeeded.",
    )))
}

pub(crate) async fn api_disconnect_provider(
    Path(provider): Path<ProviderKind>,
) -> Result<Json<MessageResponse>, ApiError> {
    runtime_db(move || storage::delete_provider_connection(provider)).await?;
    Ok(Json(MessageResponse::new(format!(
        "Disconnected {} from the app.",
        provider.display_name()
    ))))
}

pub(crate) async fn api_start_provider_export(
    State(context): State<Arc<AppContext>>,
    Path(provider): Path<ProviderKind>,
) -> Result<Json<OperationStartResponse>, ApiError> {
    let operation_id = Uuid::new_v4().to_string();
    insert_operation(
        &context,
        OperationRecord {
            id: operation_id.clone(),
            provider,
            kind: OperationKind::Pull,
            status: OperationStatus::Running,
            stage: "Starting pull".to_string(),
            detail: None,
            saved_tracks_done: 0,
            saved_tracks_total: None,
            playlists_done: 0,
            playlists_total: None,
            playlist_entries_done: 0,
            playlist_entries_total: None,
            message: None,
            warnings: Vec::new(),
            error: None,
            started_at: Utc::now(),
            finished_at: None,
            last_persisted_at: None,
        },
    )
    .await?;

    let background_context = context.clone();
    let background_operation_id = operation_id.clone();
    tokio::spawn(async move {
        let progress = progress_handler_for_operation(
            background_context.clone(),
            background_operation_id.clone(),
        );
        let result = async {
            let provider_client =
                build_connected_provider(provider, ProviderCapability::Read).await?;
            // Provider network I/O runs with no library lock held; the snapshot
            // is merged onto the current in-memory state afterwards.
            let snapshot = provider_client
                .export_library_with_progress(Some(progress))
                .await
                .map_err(ApiError::from)?;
            let summary = {
                let mut guard = background_context.library.write().await;
                let summary = merge_provider_snapshot(&mut guard, snapshot);
                persist_library(&guard).await?;
                summary
            };
            Ok::<MessageResponse, ApiError>(MessageResponse::with_warnings(
                format!(
                    "Merged {} saved tracks and {} playlists from {}.",
                    summary.saved_tracks_seen,
                    summary.playlists_seen,
                    provider.display_name()
                ),
                summary.warnings,
            ))
        }
        .await;
        finish_operation(background_context, &background_operation_id, result).await;
    });

    Ok(Json(OperationStartResponse { operation_id }))
}

pub(crate) async fn api_start_provider_identity(
    State(context): State<Arc<AppContext>>,
    Path(provider): Path<ProviderKind>,
) -> Result<Json<OperationStartResponse>, ApiError> {
    let operation_id = Uuid::new_v4().to_string();
    insert_operation(
        &context,
        OperationRecord {
            id: operation_id.clone(),
            provider,
            kind: OperationKind::Identity,
            status: OperationStatus::Running,
            stage: "Starting identity sync".to_string(),
            detail: None,
            saved_tracks_done: 0,
            saved_tracks_total: None,
            playlists_done: 0,
            playlists_total: None,
            playlist_entries_done: 0,
            playlist_entries_total: None,
            message: None,
            warnings: Vec::new(),
            error: None,
            started_at: Utc::now(),
            finished_at: None,
            last_persisted_at: None,
        },
    )
    .await?;

    let background_context = context.clone();
    let background_operation_id = operation_id.clone();
    tokio::spawn(async move {
        let progress = progress_handler_for_operation(
            background_context.clone(),
            background_operation_id.clone(),
        );
        let result = async {
            let provider_client =
                build_connected_provider(provider, ProviderCapability::Read).await?;
            // Snapshot the state and its version under a brief read lock, then
            // run the provider identity I/O on the clone with no lock held.
            let (mut working, base_version) = {
                let guard = background_context.library.read().await;
                (guard.clone(), background_context.library_version())
            };
            let summary = crate::identity::reconcile_provider_identities(
                provider_client.as_ref(),
                &mut working,
                Some(progress),
            )
            .await
            .map_err(ApiError::from)?;
            let deferred =
                summary.unprocessed_due_rate_limit + summary.unprocessed_due_safety_limit;
            let concurrent_warning =
                commit_identity_results(&background_context, working, base_version).await?;
            let mut warnings = summary.warnings;
            warnings.extend(concurrent_warning);
            Ok::<MessageResponse, ApiError>(MessageResponse::with_warnings(
                format!(
                    "{} identity sync performed {} provider identity lookups, added {} links, merged {} duplicate track rows, skipped {} merge conflicts, flagged {} invalid metadata rows, removed {} duplicate saved rows, left {} unmatched, and deferred {} tracks for a later run.",
                    provider.display_name(),
                    summary.provider_searches,
                    summary.provider_links_added,
                    summary.tracks_merged,
                    summary.merge_conflicts,
                    summary.invalid_metadata,
                    summary.duplicate_saved_tracks_removed,
                    summary.unmatched,
                    deferred
                ),
                warnings,
            ))
        }
        .await;
        finish_operation(background_context, &background_operation_id, result).await;
    });

    Ok(Json(OperationStartResponse { operation_id }))
}

/// Commits the results of an identity run that mutated `working` (a clone taken
/// while `base_version` was current) back into the canonical state.
///
/// Identity's mutations (track merges, duplicate removals, conflict tombstones,
/// link additions) reshape the library across dimensions and cannot be cleanly
/// re-applied item-by-item onto a concurrently edited state. So this uses an
/// optimistic version check: if no user mutation happened while the provider I/O
/// ran (`library_version` unchanged), the current state still equals the clone's
/// base and committing the clone wholesale is exactly correct. If a user edit
/// did land, clobbering it would lose data, so the network-derived identity
/// results are discarded; only the cheap, safe, non-network duplicate-saved
/// consolidation is re-run against the *current* state, and a warning tells the
/// user to re-run identity. This never loses a user edit and never corrupts
/// state, at the cost of an occasional identity re-run in the rare
/// edited-during-sync case. Returns an extra warning to surface, if any.
pub(crate) async fn commit_identity_results(
    context: &Arc<AppContext>,
    working: LibraryState,
    base_version: u64,
) -> Result<Option<String>, ApiError> {
    let mut guard = context.library.write().await;
    if context.library_version() == base_version {
        *guard = working;
        persist_library(&guard).await?;
        Ok(None)
    } else {
        if guard.consolidate_duplicate_saved_tracks() > 0 {
            persist_library(&guard).await?;
        }
        Ok(Some(
            "Detected library edits made while identity sync was running, so the provider identity results were not applied to avoid overwriting your changes. Re-run identity sync.".to_string(),
        ))
    }
}

pub(crate) async fn api_start_library_identity(
    State(context): State<Arc<AppContext>>,
) -> Result<Json<OperationStartResponse>, ApiError> {
    let operation_id = Uuid::new_v4().to_string();
    insert_operation(
        &context,
        OperationRecord {
            id: operation_id.clone(),
            provider: ProviderKind::Spotify,
            kind: OperationKind::IdentityAll,
            status: OperationStatus::Running,
            stage: "Starting library identity sync".to_string(),
            detail: None,
            saved_tracks_done: 0,
            saved_tracks_total: None,
            playlists_done: 0,
            playlists_total: None,
            playlist_entries_done: 0,
            playlist_entries_total: None,
            message: None,
            warnings: Vec::new(),
            error: None,
            started_at: Utc::now(),
            finished_at: None,
            last_persisted_at: None,
        },
    )
    .await?;

    let background_context = context.clone();
    let background_operation_id = operation_id.clone();
    tokio::spawn(async move {
        let progress = progress_handler_for_operation(
            background_context.clone(),
            background_operation_id.clone(),
        );
        let result = async {
            // Snapshot state + version once, then run every provider's identity
            // I/O against the clone with no lock held. Results are committed once
            // at the end via the same version-checked path as a single provider.
            let (mut working, base_version) = {
                let guard = background_context.library.read().await;
                (guard.clone(), background_context.library_version())
            };
            let mut warnings = Vec::new();
            let mut providers_ran = 0_usize;
            let mut links_added = 0_usize;
            let mut tracks_merged = 0_usize;
            let mut merge_conflicts = 0_usize;
            let mut invalid_metadata = 0_usize;
            let mut duplicate_saved_removed = 0_usize;
            let mut unmatched = 0_usize;
            let mut provider_searches = 0_usize;
            let mut deferred = 0_usize;

            for provider in ProviderKind::all().iter().copied() {
                progress(ProviderProgress {
                    stage: format!("Preparing {} identity sync", provider.display_name()),
                    detail: Some("Checking connection, health, and cooldown state.".to_string()),
                    saved_tracks_done: 0,
                    saved_tracks_total: Some(working.tracks.len()),
                    ..Default::default()
                });

                if let Some(reason) = library_identity_skip_reason(provider).await? {
                    warnings.push(reason);
                    continue;
                }

                let provider_client =
                    match build_connected_provider(provider, ProviderCapability::Read).await {
                        Ok(provider_client) => provider_client,
                        Err(error) => {
                            let message = sanitize_error_message(&error.message);
                            remember_identity_provider_failure(provider, error.source.as_ref())
                                .await?;
                            warnings.push(format!(
                                "Skipped {} identity sync: {}",
                                provider.display_name(),
                                message
                            ));
                            continue;
                        }
                    };

                progress(ProviderProgress {
                    stage: format!("Resolving {} identities", provider.display_name()),
                    detail: Some("Searching provider catalog for missing canonical IDs.".to_string()),
                    saved_tracks_done: 0,
                    saved_tracks_total: Some(working.tracks.len()),
                    ..Default::default()
                });

                let summary = match crate::identity::reconcile_provider_identities(
                    provider_client.as_ref(),
                    &mut working,
                    Some(progress.clone()),
                )
                .await
                {
                    Ok(summary) => summary,
                    Err(error) => {
                        let message = sanitize_error_message(&error.to_string());
                        remember_identity_provider_failure(provider, Some(&error)).await?;
                        warnings.push(format!(
                            "{} identity sync stopped early: {}",
                            provider.display_name(),
                            message
                        ));
                        continue;
                    }
                };

                providers_ran += 1;
                links_added += summary.provider_links_added;
                tracks_merged += summary.tracks_merged;
                merge_conflicts += summary.merge_conflicts;
                invalid_metadata += summary.invalid_metadata;
                duplicate_saved_removed += summary.duplicate_saved_tracks_removed;
                unmatched += summary.unmatched;
                provider_searches += summary.provider_searches;
                deferred +=
                    summary.unprocessed_due_rate_limit + summary.unprocessed_due_safety_limit;
                warnings.extend(summary.warnings);
                runtime_db(move || storage::clear_provider_cooldown(provider)).await?;
                save_provider_health(provider_health_ok(
                    provider,
                    format!("{} identity sync succeeded.", provider.display_name()),
                ))
                .await?;
            }

            let track_total = working.tracks.len();
            progress(ProviderProgress {
                stage: "Library identity sync complete".to_string(),
                saved_tracks_done: track_total,
                saved_tracks_total: Some(track_total),
                ..Default::default()
            });

            let concurrent_warning =
                commit_identity_results(&background_context, working, base_version).await?;
            warnings.extend(concurrent_warning);

            let message = if providers_ran == 0 {
                "Library identity sync did not run against any provider.".to_string()
            } else {
                format!(
                    "Library identity sync ran against {providers_ran} provider(s), performed {provider_searches} provider identity lookups, added {links_added} links, merged {tracks_merged} duplicate track rows, skipped {merge_conflicts} merge conflicts, flagged {invalid_metadata} invalid metadata rows, removed {duplicate_saved_removed} duplicate saved rows, left {unmatched} unmatched, and deferred {deferred} tracks for a later run."
                )
            };

            Ok::<MessageResponse, ApiError>(MessageResponse::with_warnings(
                message, warnings,
            ))
        }
        .await;
        finish_operation(background_context, &background_operation_id, result).await;
    });

    Ok(Json(OperationStartResponse { operation_id }))
}

pub(crate) async fn api_start_provider_sync(
    State(context): State<Arc<AppContext>>,
    Path(provider): Path<ProviderKind>,
) -> Result<Json<OperationStartResponse>, ApiError> {
    start_provider_sync_operation(context, provider, false).await
}

pub(crate) async fn api_start_provider_reset_sync(
    State(context): State<Arc<AppContext>>,
    Path(provider): Path<ProviderKind>,
) -> Result<Json<OperationStartResponse>, ApiError> {
    if !provider.supports_library_reset() {
        return Err(ApiError::bad_request(format!(
            "{} does not support account-wide library reset in this app.",
            provider.display_name()
        )));
    }
    start_provider_sync_operation(context, provider, true).await
}

pub(crate) async fn start_provider_sync_operation(
    context: Arc<AppContext>,
    provider: ProviderKind,
    reset_destination: bool,
) -> Result<Json<OperationStartResponse>, ApiError> {
    let operation_id = Uuid::new_v4().to_string();
    insert_operation(
        &context,
        OperationRecord {
            id: operation_id.clone(),
            provider,
            kind: if reset_destination {
                OperationKind::ResetPush
            } else {
                OperationKind::Push
            },
            status: OperationStatus::Running,
            stage: if reset_destination {
                "Starting reset and push".to_string()
            } else {
                "Starting push".to_string()
            },
            detail: None,
            saved_tracks_done: 0,
            saved_tracks_total: None,
            playlists_done: 0,
            playlists_total: None,
            playlist_entries_done: 0,
            playlist_entries_total: None,
            message: None,
            warnings: Vec::new(),
            error: None,
            started_at: Utc::now(),
            finished_at: None,
            last_persisted_at: None,
        },
    )
    .await?;

    let background_context = context.clone();
    let background_operation_id = operation_id.clone();
    tokio::spawn(async move {
        let progress = progress_handler_for_operation(
            background_context.clone(),
            background_operation_id.clone(),
        );
        let result = async {
            let provider_client = build_connected_provider(
                provider,
                if reset_destination {
                    ProviderCapability::ReadWrite
                } else {
                    ProviderCapability::Write
                },
            )
            .await?;
            let purge_report = if reset_destination {
                progress(ProviderProgress {
                    stage: "Resetting destination".to_string(),
                    detail: Some(format!(
                        "Removing saved tracks and playlists from {} before push.",
                        provider.display_name()
                    )),
                    ..Default::default()
                });
                Some(
                    provider_client
                        .purge_library(true)
                        .await
                        .map_err(ApiError::from)?,
                )
            } else {
                None
            };
            // Snapshot the state (clearing this provider's playlist links for a
            // reset), run the push against the clone with no lock held, then
            // reconcile the provider-scoped results back into the current state.
            let mut working = {
                let guard = background_context.library.read().await;
                guard.clone()
            };
            if reset_destination {
                working.clear_playlist_provider_state(provider);
            }
            let sync_result = provider_client
                .sync_library_with_progress(&mut working, true, Some(progress.clone()))
                .await;
            {
                let mut guard = background_context.library.write().await;
                reapply_provider_sync(&mut guard, &working, provider);
                persist_library(&guard).await?;
            }
            let summary = sync_result.map_err(ApiError::from)?;
            let reset_prefix = purge_report
                .map(|report| {
                    format!(
                        "Reset removed {} saved tracks and {} playlists. ",
                        report.saved_tracks, report.playlists
                    )
                })
                .unwrap_or_default();
            Ok::<MessageResponse, ApiError>(MessageResponse::with_warnings(
                format!(
                    "{}Synced {} saved tracks and {} playlist entries into {}.",
                    reset_prefix,
                    summary.saved_tracks_synced,
                    summary.playlist_entries_synced,
                    provider.display_name()
                ),
                summary.warnings,
            ))
        }
        .await;
        finish_operation(background_context, &background_operation_id, result).await;
    });

    Ok(Json(OperationStartResponse { operation_id }))
}

pub(crate) async fn api_operation(
    State(context): State<Arc<AppContext>>,
    Path(operation_id): Path<String>,
) -> Result<Json<OperationResponse>, ApiError> {
    let operation = {
        let operations = context
            .operations
            .lock()
            .map_err(|_| ApiError::internal("Failed to read operation state."))?;
        operations.get(&operation_id).cloned()
    };
    let operation = match operation {
        Some(operation) => operation,
        None => tokio::task::spawn_blocking({
            let operation_id = operation_id.clone();
            move || storage::read_ui_operation_json(&operation_id)
        })
        .await
        .context("Failed to join operation history read task")?
        .map_err(ApiError::from)?
        .map(|json| serde_json::from_str::<OperationRecord>(&json))
        .transpose()
        .map_err(|error| ApiError::internal(error.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("Unknown operation '{operation_id}'.")))?,
    };
    Ok(Json(operation_response(&operation)))
}
