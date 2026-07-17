use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use chrono::Utc;
use rand::{distributions::Alphanumeric, Rng};

use crate::provider::{ProviderCapability, StreamingProvider};
use crate::providers::policy;
use crate::providers::spotify::SpotifyProvider;
use crate::providers::youtube_music::YoutubeMusicProvider;
use crate::storage;

use crate::domain::{
    LibraryState, ProviderConnection, ProviderConnectionConfig, ProviderCooldown, ProviderHealth,
    ProviderKind,
};

use super::conflicts::*;
use super::dto::*;
use super::error::*;
use super::projections::*;
use super::{runtime_db, AppContext};

pub(crate) async fn ensure_provider_not_cooling_down(
    provider: ProviderKind,
) -> Result<(), ApiError> {
    if let Some(cooldown) = runtime_db(move || storage::read_provider_cooldown(provider)).await? {
        return Err(ApiError::rate_limited(format!(
            "{} is cooling down until {} because the provider recently rejected requests: {}",
            provider.display_name(),
            cooldown.blocked_until.to_rfc3339(),
            cooldown.reason
        )));
    }
    Ok(())
}

pub(crate) async fn ensure_provider_health_allows_operation(
    provider: ProviderKind,
) -> Result<(), ApiError> {
    if let Some(health) = runtime_db(move || storage::read_provider_health(provider)).await? {
        if !health.ok {
            return Err(ApiError::bad_request(format!(
                "Last {} connection check failed: {}. Relink or run Check Connection before starting sync.",
                provider.display_name(),
                health
                    .message
                    .as_deref()
                    .unwrap_or("No detailed provider message was recorded.")
            )));
        }
    }
    Ok(())
}

pub(crate) async fn library_identity_skip_reason(
    provider: ProviderKind,
) -> Result<Option<String>, ApiError> {
    if runtime_db(move || storage::read_provider_connection(provider))
        .await?
        .is_none()
    {
        return Ok(Some(format!(
            "Skipped {} identity sync because the provider is not linked.",
            provider.display_name()
        )));
    }

    if let Some(cooldown) = runtime_db(move || storage::read_provider_cooldown(provider)).await? {
        return Ok(Some(format!(
            "Skipped {} identity sync because the provider is cooling down until {}: {}",
            provider.display_name(),
            cooldown.blocked_until.to_rfc3339(),
            cooldown.reason
        )));
    }

    if let Some(health) = runtime_db(move || storage::read_provider_health(provider)).await? {
        if !health.ok {
            return Ok(Some(format!(
                "Skipped {} identity sync because the last connection check failed: {}",
                provider.display_name(),
                health
                    .message
                    .as_deref()
                    .unwrap_or("No detailed provider message was recorded.")
            )));
        }
    }

    Ok(None)
}

pub(crate) fn provider_health_ok(
    provider: ProviderKind,
    message: impl Into<String>,
) -> ProviderHealth {
    ProviderHealth {
        provider,
        checked_at: Utc::now(),
        ok: true,
        message: Some(message.into()),
    }
}

pub(crate) fn provider_health_failed(
    provider: ProviderKind,
    message: impl Into<String>,
) -> ProviderHealth {
    ProviderHealth {
        provider,
        checked_at: Utc::now(),
        ok: false,
        message: Some(message.into()),
    }
}

pub(crate) async fn save_provider_health(health: ProviderHealth) -> Result<(), ApiError> {
    runtime_db(move || storage::save_provider_health(&health)).await
}

/// Records a cooldown and/or unhealthy state after an identity-sync provider
/// failure, classifying the failure from the typed provider error in the
/// `anyhow` chain. A missing source (e.g. a plain bad-request) records nothing.
pub(crate) async fn remember_identity_provider_failure(
    provider: ProviderKind,
    error: Option<&anyhow::Error>,
) -> Result<(), ApiError> {
    let Some(error) = error else {
        return Ok(());
    };

    if let Some(cooldown) = policy::cooldown_from_error(provider, error) {
        runtime_db(move || storage::save_provider_cooldown(&cooldown)).await?;
    }

    if policy::is_connection_health_failure(error) {
        save_provider_health(provider_health_failed(
            provider,
            sanitize_error_message(&error.to_string()),
        ))
        .await?;
    }

    Ok(())
}

pub(crate) fn looks_like_placeholder_ytmusic_cookie(cookie: &str) -> bool {
    let lowered = cookie.to_ascii_lowercase();
    lowered.contains("your_cookie_here") || lowered.contains("dummy") || lowered.contains("paste")
}

pub(crate) fn looks_like_placeholder_ytmusic_authuser(authuser: &str) -> bool {
    let lowered = authuser.trim().to_ascii_lowercase();
    lowered.is_empty() || lowered.contains("paste")
}

pub(crate) async fn spotify_callback(
    State(context): State<Arc<AppContext>>,
    Query(query): Query<SpotifyCallbackQuery>,
) -> Result<Response, ApiError> {
    if let Some(error) = query.error {
        return Ok(Redirect::to(&app_notice_redirect(Some(format!(
            "Spotify connection failed: {error}"
        ))))
        .into_response());
    }

    let code = query
        .code
        .ok_or_else(|| ApiError::bad_request("Spotify callback did not include a code."))?;
    let state = query
        .state
        .ok_or_else(|| ApiError::bad_request("Spotify callback did not include a state."))?;

    let pending = context
        .pending_spotify_auth
        .lock()
        .await
        .remove(&state)
        .ok_or_else(|| ApiError::bad_request("Spotify authorization state expired."))?;

    let config = SpotifyProvider::exchange_authorization_code(
        &pending.client_id,
        &pending.client_secret,
        &context.spotify_redirect_uri,
        &code,
    )
    .await
    .map_err(ApiError::from)?;

    let spotify =
        match SpotifyProvider::from_connection(&config, ProviderCapability::ReadWrite).await {
            Ok(provider) => provider,
            Err(error) => {
                return Ok(Redirect::to(&app_notice_redirect(Some(format!(
                    "Spotify link failed: {}",
                    sanitize_error_message(&error.to_string())
                ))))
                .into_response())
            }
        };

    if let Err(error) = spotify.verify_connection().await {
        return Ok(Redirect::to(&app_notice_redirect(Some(format!(
            "Spotify link failed: {}",
            sanitize_error_message(&error.to_string())
        ))))
        .into_response());
    }

    let now = Utc::now();
    runtime_db(move || {
        storage::save_provider_connection(&ProviderConnection {
            provider: ProviderKind::Spotify,
            connected_at: now,
            updated_at: now,
            config: ProviderConnectionConfig::Spotify(config),
        })?;
        storage::clear_provider_cooldown(ProviderKind::Spotify)
    })
    .await?;
    save_provider_health(provider_health_ok(
        ProviderKind::Spotify,
        "Connection verified during Spotify link.",
    ))
    .await?;

    Ok(Html(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta http-equiv="refresh" content="0; url=/app/overview?notice=Spotify%20connected">
    <title>Spotify Connected</title>
  </head>
  <body style="font-family: system-ui, sans-serif; background: #0d0d12; color: #f6f4ef; display:grid; place-items:center; min-height:100vh;">
    <p>Spotify connected. Returning to the app…</p>
  </body>
</html>"#
            .to_string(),
    )
    .into_response())
}

pub(crate) async fn read_provider_connections() -> Result<Vec<ProviderConnection>, ApiError> {
    tokio::task::spawn_blocking(storage::list_provider_connections)
        .await
        .context("Failed to join provider connection read task")?
        .map_err(ApiError::from)
}

pub(crate) async fn build_provider_from_connection(
    connection: &ProviderConnection,
    capability: ProviderCapability,
) -> Result<Box<dyn StreamingProvider>, ApiError> {
    match &connection.config {
        ProviderConnectionConfig::Spotify(config) => Ok(Box::new(
            SpotifyProvider::from_connection(config, capability)
                .await
                .map_err(ApiError::from)?,
        )),
        ProviderConnectionConfig::YoutubeMusic(config) => Ok(Box::new(
            YoutubeMusicProvider::from_connection(config).map_err(ApiError::from)?,
        )),
    }
}

pub(crate) async fn build_connected_provider(
    provider: ProviderKind,
    capability: ProviderCapability,
) -> Result<Box<dyn StreamingProvider>, ApiError> {
    ensure_provider_not_cooling_down(provider).await?;
    ensure_provider_health_allows_operation(provider).await?;
    build_connected_provider_allowing_failed_health(provider, capability).await
}

pub(crate) async fn build_connected_provider_allowing_failed_health(
    provider: ProviderKind,
    capability: ProviderCapability,
) -> Result<Box<dyn StreamingProvider>, ApiError> {
    ensure_provider_not_cooling_down(provider).await?;
    let connection = runtime_db(move || storage::read_provider_connection(provider))
        .await?
        .ok_or_else(|| {
            ApiError::bad_request(format!(
                "{} is not connected in the app yet.",
                provider.display_name()
            ))
        })?;

    build_provider_from_connection(&connection, capability).await
}

pub(crate) fn provider_connection_payloads(
    state: &LibraryState,
    connections: &[ProviderConnection],
    cooldowns: &[ProviderCooldown],
    healths: &[ProviderHealth],
) -> Vec<ProviderConnectionDto> {
    // The open-conflict count is provider-independent, so compute the whole
    // conflict set once and reuse it for every provider's preflight instead of
    // rescanning per provider on each `/api/providers` poll.
    let identity_conflicts = identity_conflict_rows(state, None).len();
    ProviderKind::all()
        .iter()
        .copied()
        .map(|provider| {
            let connection = connections
                .iter()
                .find(|connection| connection.provider == provider);
            let cooldown = cooldowns
                .iter()
                .find(|cooldown| cooldown.provider == provider);
            let health = healths.iter().find(|health| health.provider == provider);
            ProviderConnectionDto {
                key: provider.as_key().to_string(),
                name: provider.display_name().to_string(),
                connected: connection.is_some(),
                connected_at: connection.map(|connection| connection.connected_at.to_rfc3339()),
                updated_at: connection.map(|connection| connection.updated_at.to_rfc3339()),
                health_checked_at: health.map(|health| health.checked_at.to_rfc3339()),
                health_ok: health.map(|health| health.ok),
                health_message: health.and_then(|health| health.message.clone()),
                cooldown_until: cooldown.map(|cooldown| cooldown.blocked_until.to_rfc3339()),
                cooldown_reason: cooldown.map(|cooldown| cooldown.reason.clone()),
                preflight: provider_preflight_payload(
                    state,
                    provider,
                    connection,
                    cooldown,
                    health,
                    identity_conflicts,
                ),
            }
        })
        .collect()
}

pub(crate) fn provider_preflight_payload(
    state: &LibraryState,
    provider: ProviderKind,
    connection: Option<&ProviderConnection>,
    cooldown: Option<&ProviderCooldown>,
    health: Option<&ProviderHealth>,
    identity_conflicts: usize,
) -> ProviderPreflightDto {
    let provider_key = provider.as_key();
    let track_index = build_track_index(state);
    let track_ids_linked = state
        .tracks
        .iter()
        .filter(|track| track.provider_links.contains_key(provider_key))
        .count();
    let saved_tracks_pushable = state
        .saved_tracks
        .iter()
        .filter(|entry| indexed_track_has_provider_link(&track_index, &entry.track_id, provider))
        .count();
    let playlist_entries_pushable = state
        .playlists
        .iter()
        .flat_map(|playlist| playlist.entries.iter())
        .filter(|entry| indexed_track_has_provider_link(&track_index, &entry.track_id, provider))
        .count();
    let saved_tracks_missing_identity = state
        .saved_tracks
        .len()
        .saturating_sub(saved_tracks_pushable);
    let playlist_entries_total = state.playlist_entry_count();
    let playlist_entries_missing_identity =
        playlist_entries_total.saturating_sub(playlist_entries_pushable);
    let track_ids_missing = state.tracks.len().saturating_sub(track_ids_linked);

    let mut blockers = Vec::new();
    if connection.is_none() {
        blockers.push(format!("{} is not linked.", provider.display_name()));
    }
    if let Some(cooldown) = cooldown {
        blockers.push(format!(
            "{} is cooling down until {}.",
            provider.display_name(),
            cooldown.blocked_until.to_rfc3339()
        ));
    }
    if let Some(health) = health {
        if !health.ok {
            blockers.push(format!(
                "Last {} connection check failed: {}",
                provider.display_name(),
                health
                    .message
                    .as_deref()
                    .unwrap_or("No detailed provider message was recorded.")
            ));
        }
    }
    if state.saved_tracks.is_empty() && playlist_entries_total == 0 {
        blockers.push(
            "The canonical library has no saved tracks or playlist entries to push.".to_string(),
        );
    }

    let mut warnings = Vec::new();
    if saved_tracks_missing_identity > 0 {
        warnings.push(format!(
            "{saved_tracks_missing_identity} saved tracks do not have a {} ID and will be skipped during push.",
            provider.display_name()
        ));
    }
    if playlist_entries_missing_identity > 0 {
        warnings.push(format!(
            "{playlist_entries_missing_identity} playlist entries do not have a {} ID and will be skipped during push.",
            provider.display_name()
        ));
    }
    if track_ids_missing > 0 {
        warnings.push(format!(
            "{track_ids_missing} canonical tracks are still missing a {} identity.",
            provider.display_name()
        ));
    }
    if provider == ProviderKind::YoutubeMusic {
        warnings.push(
            "YouTube Music browser headers can expire; relink if a pull or push reports an authentication error."
                .to_string(),
        );
    }

    let mut reset_blockers = Vec::new();
    if provider.supports_library_reset() {
        if saved_tracks_missing_identity > 0 || playlist_entries_missing_identity > 0 {
            reset_blockers.push(format!(
                "Reset & Push is blocked because {} saved tracks and {} playlist entries would be skipped after purging {}.",
                saved_tracks_missing_identity,
                playlist_entries_missing_identity,
                provider.display_name()
            ));
        }
        if identity_conflicts > 0 {
            reset_blockers.push(format!(
                "Reset & Push is blocked while {identity_conflicts} identity conflicts need merge review."
            ));
        }
    }

    let can_pull = connection.is_some()
        && cooldown.is_none()
        && health.map(|health| health.ok).unwrap_or(true);
    let can_push = can_pull
        && blockers.is_empty()
        && (saved_tracks_pushable > 0 || playlist_entries_pushable > 0);
    let can_reset_push = can_push && provider.supports_library_reset() && reset_blockers.is_empty();
    ProviderPreflightDto {
        can_pull,
        can_push,
        can_reset_push,
        blockers,
        reset_blockers,
        warnings,
        saved_tracks_total: state.saved_tracks.len(),
        saved_tracks_pushable,
        saved_tracks_missing_identity,
        playlists_total: state.playlists.len(),
        linked_playlists: state
            .playlists
            .iter()
            .filter(|playlist| playlist.provider_links.contains_key(provider_key))
            .count(),
        playlist_entries_total,
        playlist_entries_pushable,
        playlist_entries_missing_identity,
        track_ids_total: state.tracks.len(),
        track_ids_linked,
        track_ids_missing,
    }
}

pub(crate) fn provider_push_plan_payload(
    state: &LibraryState,
    provider: ProviderKind,
    connection: Option<&ProviderConnection>,
    cooldown: Option<&ProviderCooldown>,
    health: Option<&ProviderHealth>,
) -> ProviderPushPlanDto {
    let identity_conflicts = identity_conflict_rows(state, None).len();
    ProviderPushPlanDto {
        provider: provider.as_key().to_string(),
        provider_name: provider.display_name().to_string(),
        preflight: provider_preflight_payload(
            state,
            provider,
            connection,
            cooldown,
            health,
            identity_conflicts,
        ),
        saved_tracks: push_saved_track_plan_section(state, provider),
        playlist_entries: push_playlist_entry_plan_section(state, provider),
        playlists: push_playlist_plan_section(state, provider),
    }
}

pub(crate) fn push_saved_track_plan_section(
    state: &LibraryState,
    provider: ProviderKind,
) -> PushPlanSectionDto {
    let track_index = build_track_index(state);
    let indexes = ConflictIndexes::build(state);
    let skipped_examples = state
        .saved_tracks
        .iter()
        .filter_map(|entry| {
            let track = track_index.get(entry.track_id.as_str())?;
            if track.provider_links.contains_key(provider.as_key()) {
                return None;
            }
            Some(conflict_track_dto(track, &indexes))
        })
        .take(10)
        .collect::<Vec<_>>();
    let pushable = state
        .saved_tracks
        .iter()
        .filter(|entry| indexed_track_has_provider_link(&track_index, &entry.track_id, provider))
        .count();

    PushPlanSectionDto {
        total: state.saved_tracks.len(),
        pushable,
        skipped_missing_identity: state.saved_tracks.len().saturating_sub(pushable),
        skipped_examples,
    }
}

pub(crate) fn push_playlist_entry_plan_section(
    state: &LibraryState,
    provider: ProviderKind,
) -> PushPlanSectionDto {
    let track_index = build_track_index(state);
    let indexes = ConflictIndexes::build(state);
    let total = state.playlist_entry_count();
    let mut pushable = 0;
    let mut skipped_examples = Vec::new();

    for entry in state
        .playlists
        .iter()
        .flat_map(|playlist| playlist.entries.iter())
    {
        let Some(track) = track_index.get(entry.track_id.as_str()) else {
            continue;
        };
        if track.provider_links.contains_key(provider.as_key()) {
            pushable += 1;
        } else if skipped_examples.len() < 10 {
            skipped_examples.push(conflict_track_dto(track, &indexes));
        }
    }

    PushPlanSectionDto {
        total,
        pushable,
        skipped_missing_identity: total.saturating_sub(pushable),
        skipped_examples,
    }
}

pub(crate) fn push_playlist_plan_section(
    state: &LibraryState,
    provider: ProviderKind,
) -> PushPlaylistPlanSectionDto {
    let track_index = build_track_index(state);
    let provider_key = provider.as_key();
    let mut examples = Vec::new();
    let mut linked = 0;

    for playlist in &state.playlists {
        let playlist_linked = playlist.provider_links.contains_key(provider_key);
        if playlist_linked {
            linked += 1;
        }
        let missing_entries = playlist
            .entries
            .iter()
            .filter(|entry| {
                !indexed_track_has_provider_link(&track_index, &entry.track_id, provider)
            })
            .count();

        if examples.len() < 10 && (!playlist_linked || missing_entries > 0) {
            examples.push(PushPlaylistPlanItemDto {
                playlist_id: playlist.id.clone(),
                name: playlist.name.clone(),
                entry_count: playlist.entries.len(),
                linked: playlist_linked,
                missing_entries,
            });
        }
    }

    PushPlaylistPlanSectionDto {
        total: state.playlists.len(),
        linked,
        unlinked: state.playlists.len().saturating_sub(linked),
        examples,
    }
}

pub(crate) fn random_state() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

pub(crate) fn app_notice_redirect(notice: Option<String>) -> String {
    match notice {
        Some(notice) => {
            let encoded =
                url::form_urlencoded::byte_serialize(notice.as_bytes()).collect::<String>();
            format!("/app/overview?notice={encoded}")
        }
        None => "/app/overview".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::Utc;

    use crate::domain::{
        LibraryState, LinkSource, PlaylistEntity, PlaylistEntry, ProviderConnection,
        ProviderConnectionConfig, ProviderKind, ProviderTrackLink, SavedTrackEntry,
        SpotifyConnectionConfig,
    };
    use crate::web::conflicts::identity_conflict_rows;
    use crate::web::test_support::test_track;

    use super::{provider_health_failed, provider_preflight_payload, provider_push_plan_payload};

    #[test]
    fn preflight_counts_pushable_and_missing_provider_identities() {
        let now = Utc::now();
        let mut linked_track = test_track("track-linked", "Sirius");
        linked_track.provider_links.insert(
            ProviderKind::Spotify.as_key().to_string(),
            ProviderTrackLink {
                provider_id: "spotify-track-1".to_string(),
                source: LinkSource::Export,
                confidence: Some(1.0),
                linked_at: now,
                last_seen_at: Some(now),
            },
        );

        let mut state = LibraryState::new();
        state.tracks.push(linked_track);
        state
            .tracks
            .push(test_track("track-missing", "No Spotify ID"));
        state.saved_tracks.push(SavedTrackEntry {
            id: "saved-linked".to_string(),
            track_id: "track-linked".to_string(),
            added_at: None,
            provider_state: BTreeMap::new(),
        });
        state.saved_tracks.push(SavedTrackEntry {
            id: "saved-missing".to_string(),
            track_id: "track-missing".to_string(),
            added_at: None,
            provider_state: BTreeMap::new(),
        });
        state.playlists.push(PlaylistEntity {
            id: "playlist-1".to_string(),
            name: "Favorites".to_string(),
            description: None,
            provider_links: BTreeMap::new(),
            provider_state: BTreeMap::new(),
            entries: vec![
                PlaylistEntry {
                    id: "entry-linked".to_string(),
                    track_id: "track-linked".to_string(),
                    added_at: None,
                    provider_state: BTreeMap::new(),
                },
                PlaylistEntry {
                    id: "entry-missing".to_string(),
                    track_id: "track-missing".to_string(),
                    added_at: None,
                    provider_state: BTreeMap::new(),
                },
            ],
        });
        let connection = ProviderConnection {
            provider: ProviderKind::Spotify,
            connected_at: now,
            updated_at: now,
            config: ProviderConnectionConfig::Spotify(SpotifyConnectionConfig {
                client_id: "client".to_string(),
                client_secret: "secret".to_string(),
                refresh_token: "refresh".to_string(),
            }),
        };

        let identity_conflicts = identity_conflict_rows(&state, None).len();
        let preflight = provider_preflight_payload(
            &state,
            ProviderKind::Spotify,
            Some(&connection),
            None,
            None,
            identity_conflicts,
        );

        assert!(preflight.can_push);
        assert!(!preflight.can_reset_push);
        assert_eq!(preflight.saved_tracks_total, 2);
        assert_eq!(preflight.saved_tracks_pushable, 1);
        assert_eq!(preflight.saved_tracks_missing_identity, 1);
        assert_eq!(preflight.playlist_entries_total, 2);
        assert_eq!(preflight.playlist_entries_pushable, 1);
        assert_eq!(preflight.playlist_entries_missing_identity, 1);
        assert_eq!(preflight.track_ids_missing, 1);
        assert!(preflight.blockers.is_empty());
        assert!(preflight
            .warnings
            .iter()
            .any(|warning| warning.contains("saved tracks")));
        assert!(preflight
            .reset_blockers
            .iter()
            .any(|blocker| blocker.contains("would be skipped after purging Spotify")));

        let failed_health =
            provider_health_failed(ProviderKind::Spotify, "Stored Spotify token is invalid.");
        let blocked = provider_preflight_payload(
            &state,
            ProviderKind::Spotify,
            Some(&connection),
            None,
            Some(&failed_health),
            identity_conflicts,
        );
        assert!(!blocked.can_pull);
        assert!(!blocked.can_push);
        assert!(blocked
            .blockers
            .iter()
            .any(|blocker| blocker.contains("connection check failed")));
    }

    #[test]
    fn push_plan_matches_preflight_and_lists_skipped_identity_examples() {
        let now = Utc::now();
        let mut linked_track = test_track("track-linked", "Sirius");
        linked_track.provider_links.insert(
            ProviderKind::Spotify.as_key().to_string(),
            ProviderTrackLink {
                provider_id: "spotify-track-1".to_string(),
                source: LinkSource::Export,
                confidence: Some(1.0),
                linked_at: now,
                last_seen_at: Some(now),
            },
        );

        let mut state = LibraryState::new();
        state.tracks.push(linked_track);
        state
            .tracks
            .push(test_track("track-missing", "No Spotify ID"));
        state.saved_tracks.push(SavedTrackEntry {
            id: "saved-linked".to_string(),
            track_id: "track-linked".to_string(),
            added_at: None,
            provider_state: BTreeMap::new(),
        });
        state.saved_tracks.push(SavedTrackEntry {
            id: "saved-missing".to_string(),
            track_id: "track-missing".to_string(),
            added_at: None,
            provider_state: BTreeMap::new(),
        });
        state.playlists.push(PlaylistEntity {
            id: "playlist-1".to_string(),
            name: "Favorites".to_string(),
            description: None,
            provider_links: BTreeMap::new(),
            provider_state: BTreeMap::new(),
            entries: vec![
                PlaylistEntry {
                    id: "entry-linked".to_string(),
                    track_id: "track-linked".to_string(),
                    added_at: None,
                    provider_state: BTreeMap::new(),
                },
                PlaylistEntry {
                    id: "entry-missing".to_string(),
                    track_id: "track-missing".to_string(),
                    added_at: None,
                    provider_state: BTreeMap::new(),
                },
            ],
        });
        let connection = ProviderConnection {
            provider: ProviderKind::Spotify,
            connected_at: now,
            updated_at: now,
            config: ProviderConnectionConfig::Spotify(SpotifyConnectionConfig {
                client_id: "client".to_string(),
                client_secret: "secret".to_string(),
                refresh_token: "refresh".to_string(),
            }),
        };

        let plan = provider_push_plan_payload(
            &state,
            ProviderKind::Spotify,
            Some(&connection),
            None,
            None,
        );

        assert!(plan.preflight.can_push);
        assert_eq!(plan.saved_tracks.total, 2);
        assert_eq!(plan.saved_tracks.pushable, 1);
        assert_eq!(plan.saved_tracks.skipped_missing_identity, 1);
        assert_eq!(
            plan.saved_tracks.skipped_examples[0].track_id,
            "track-missing"
        );
        assert_eq!(plan.playlist_entries.total, 2);
        assert_eq!(plan.playlist_entries.pushable, 1);
        assert_eq!(plan.playlist_entries.skipped_missing_identity, 1);
        assert_eq!(plan.playlists.total, 1);
        assert_eq!(plan.playlists.linked, 0);
        assert_eq!(plan.playlists.unlinked, 1);
        assert_eq!(plan.playlists.examples[0].missing_entries, 1);
    }
}
