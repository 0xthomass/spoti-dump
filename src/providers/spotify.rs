use std::collections::HashSet;
use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose, Engine as _};
use chrono::Utc;
use reqwest::header::{
    HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER,
};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use tiny_http::{Response, Server};
use tokio::sync::Mutex;
use tokio::time::sleep;
use url::Url;

use crate::domain::{
    LibraryState, LinkSource, ObservedArtwork, ObservedPlaylist, ObservedPlaylistTrack,
    ObservedSavedTrack, ObservedTrack, ProviderConnection, ProviderConnectionConfig, ProviderKind,
    ProviderLibrarySnapshot, PurgeReport, SpotifyConnectionConfig, SyncStatusRecord, SyncSummary,
    TrackMetadata,
};
use crate::error::{provider_failure, ProviderError, ProviderFailure};
use crate::matching::{best_candidate, cleaned_title, MatchCandidate};
use crate::provider::{ProgressHandler, ProviderCapability, ProviderProgress, StreamingProvider};

const REDIRECT_URI: &str = "http://127.0.0.1:8000/callback";
const SCOPE_READ: &str = "user-library-read playlist-read-private";
// The push path reads the current library (GET /v1/me/tracks) and playlists
// before modifying them, so a Write-capability token must carry the read scopes
// it exercises in addition to the modify scopes. Otherwise pushes 403.
const SCOPE_WRITE: &str = "user-library-read user-library-modify playlist-read-private playlist-modify-public playlist-modify-private";
const SCOPE_ALL: &str = "user-library-read user-library-modify playlist-read-private playlist-modify-public playlist-modify-private";
/// Seconds shaved off a token's advertised lifetime so we refresh before the
/// server-side expiry actually lands.
const TOKEN_EXPIRY_SAFETY_MARGIN_SECS: u64 = 60;
/// Consecutive per-item push failures tolerated before aborting the whole push.
const PUSH_CONSECUTIVE_FAILURE_LIMIT: usize = 5;
const SPOTIFY_READ_REQUEST_ATTEMPTS: usize = 6;
const SPOTIFY_WRITE_REQUEST_ATTEMPTS: usize = 12;
const SPOTIFY_READ_RATE_LIMIT_FALLBACK_SECS: u64 = 30;
const SPOTIFY_WRITE_RATE_LIMIT_FALLBACK_SECS: u64 = 75;
const SPOTIFY_READ_RATE_LIMIT_MAX_DELAY_SECS: u64 = 60;
const SPOTIFY_WRITE_RATE_LIMIT_MAX_DELAY_SECS: u64 = 180;
const SPOTIFY_RETRY_AFTER_BUFFER_SECS: u64 = 2;
const SPOTIFY_READ_DELAY: Duration = Duration::from_millis(250);
const SPOTIFY_WRITE_DELAY: Duration = Duration::from_millis(1_500);
const SPOTIFY_SEARCH_DELAY: Duration = Duration::from_secs(2);

#[derive(Clone, Copy)]
struct SpotifyRetryPolicy {
    attempts: usize,
    rate_limit_fallback_secs: u64,
    rate_limit_max_delay_secs: u64,
}

const SPOTIFY_READ_RETRY_POLICY: SpotifyRetryPolicy = SpotifyRetryPolicy {
    attempts: SPOTIFY_READ_REQUEST_ATTEMPTS,
    rate_limit_fallback_secs: SPOTIFY_READ_RATE_LIMIT_FALLBACK_SECS,
    rate_limit_max_delay_secs: SPOTIFY_READ_RATE_LIMIT_MAX_DELAY_SECS,
};

const SPOTIFY_WRITE_RETRY_POLICY: SpotifyRetryPolicy = SpotifyRetryPolicy {
    attempts: SPOTIFY_WRITE_REQUEST_ATTEMPTS,
    rate_limit_fallback_secs: SPOTIFY_WRITE_RATE_LIMIT_FALLBACK_SECS,
    rate_limit_max_delay_secs: SPOTIFY_WRITE_RATE_LIMIT_MAX_DELAY_SECS,
};

#[derive(Clone)]
pub struct SpotifyProvider {
    client: reqwest::Client,
    tokens: Arc<SpotifyTokenManager>,
}

#[derive(Deserialize)]
struct AccessTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    scope: Option<String>,
    expires_in: Option<u64>,
}

/// Credentials required to mint a fresh access token from Spotify.
///
/// Held in memory for the life of the provider. Never logged.
struct RefreshMaterials {
    client_id: String,
    client_secret: String,
    refresh_token: String,
}

/// The mutable half of the token manager, guarded by the manager's mutex.
struct SpotifyTokenState {
    access_token: String,
    /// Absolute instant (with the safety margin already applied) after which the
    /// token is considered stale. `None` when the lifetime is unknown (a bare
    /// access token that cannot be reasoned about proactively).
    expires_at: Option<Instant>,
}

/// Owns the Spotify access token and its lifecycle.
///
/// A single [`Mutex`] serializes refreshes so concurrent request paths never
/// stampede the token endpoint. [`get_valid_token`](Self::get_valid_token)
/// refreshes proactively when the token is stale;
/// [`refresh_after_unauthorized`](Self::refresh_after_unauthorized) forces a
/// refresh after a 401 so the caller can retry once. When no
/// [`RefreshMaterials`] are available (an env flow that never obtained a refresh
/// token) both paths fail with a clear, actionable error instead of silently
/// wedging.
struct SpotifyTokenManager {
    client: reqwest::Client,
    refresh: Option<RefreshMaterials>,
    state: Mutex<SpotifyTokenState>,
}

impl SpotifyTokenManager {
    fn new(
        client: reqwest::Client,
        access_token: String,
        expires_at: Option<Instant>,
        refresh: Option<RefreshMaterials>,
    ) -> Self {
        Self {
            client,
            refresh,
            state: Mutex::new(SpotifyTokenState {
                access_token,
                expires_at,
            }),
        }
    }

    /// Returns a token that is valid now, refreshing proactively if it has gone
    /// stale.
    async fn get_valid_token(&self) -> Result<String> {
        let mut state = self.state.lock().await;
        if token_is_stale(state.expires_at) {
            self.refresh_locked(&mut state).await?;
        }
        Ok(state.access_token.clone())
    }

    /// Forces a single refresh after a request came back `401 Unauthorized`, so
    /// the caller can retry the request once with a fresh token.
    async fn refresh_after_unauthorized(&self) -> Result<String> {
        let mut state = self.state.lock().await;
        self.refresh_locked(&mut state).await?;
        Ok(state.access_token.clone())
    }

    async fn refresh_locked(&self, state: &mut SpotifyTokenState) -> Result<()> {
        let Some(materials) = self.refresh.as_ref() else {
            return Err(anyhow::Error::new(ProviderError::auth_failed(
                "Spotify access token expired and no refresh token is available to renew it. \
                 Set SPOTIFY_REFRESH_TOKEN or reconnect Spotify so a refresh token can be stored.",
            )));
        };
        let response = refresh_access_token_with(
            &self.client,
            &materials.refresh_token,
            &materials.client_id,
            &materials.client_secret,
        )
        .await?;
        state.access_token = response.access_token;
        state.expires_at = expires_at_from(response.expires_in);
        Ok(())
    }
}

/// Everything obtained during initial credential acquisition, ready to seed a
/// [`SpotifyTokenManager`].
struct AcquiredToken {
    access_token: String,
    expires_at: Option<Instant>,
    refresh: Option<RefreshMaterials>,
}

/// Whether a token whose (margin-adjusted) expiry is `expires_at` should be
/// refreshed before use. Pure so the staleness decision is unit-testable.
fn token_is_stale(expires_at: Option<Instant>) -> bool {
    match expires_at {
        Some(expires_at) => Instant::now() >= expires_at,
        None => false,
    }
}

/// Converts Spotify's `expires_in` (seconds) into an absolute refresh deadline,
/// shaving off [`TOKEN_EXPIRY_SAFETY_MARGIN_SECS`] so we renew a little early.
fn expires_at_from(expires_in: Option<u64>) -> Option<Instant> {
    expires_in.map(|seconds| {
        Instant::now()
            + Duration::from_secs(seconds.saturating_sub(TOKEN_EXPIRY_SAFETY_MARGIN_SECS))
    })
}

#[derive(Debug, Deserialize)]
struct SpotifyArtist {
    name: String,
}

#[derive(Debug, Deserialize, Default)]
struct SpotifyAlbum {
    name: String,
    #[serde(default)]
    images: Vec<SpotifyImage>,
}

#[derive(Debug, Deserialize)]
struct SpotifyImage {
    url: String,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Debug, Deserialize, Default)]
struct SpotifyExternalIds {
    isrc: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SpotifyTrack {
    id: Option<String>,
    name: String,
    artists: Vec<SpotifyArtist>,
    album: SpotifyAlbum,
    duration_ms: Option<u32>,
    #[serde(default)]
    external_ids: SpotifyExternalIds,
}

#[derive(Debug, Deserialize)]
struct SpotifySavedTrack {
    added_at: Option<String>,
    track: Option<SpotifyTrack>,
}

#[derive(Clone, Debug, Deserialize)]
struct SpotifyPlaylistSummary {
    id: String,
    name: String,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SpotifyPlaylistItem {
    added_at: Option<String>,
    #[serde(alias = "track")]
    item: Option<SpotifyTrack>,
}

#[derive(Debug, Deserialize)]
struct SpotifySearchResponse {
    tracks: SpotifySearchItems,
}

#[derive(Debug, Deserialize)]
struct SpotifySearchItems {
    items: Vec<SpotifyTrack>,
}

impl SpotifyProvider {
    pub async fn new(capability: ProviderCapability) -> Result<Self> {
        dotenvy::dotenv().ok();
        let client = build_http_client()?;
        let acquired = acquire_env_token(&client, capability).await?;
        Ok(Self::from_acquired(client, acquired))
    }

    pub async fn from_connection(
        config: &SpotifyConnectionConfig,
        capability: ProviderCapability,
    ) -> Result<Self> {
        let client = build_http_client()?;
        let response = refresh_access_token_with(
            &client,
            &config.refresh_token,
            &config.client_id,
            &config.client_secret,
        )
        .await?;
        if !token_has_required_scopes(response.scope.as_deref(), capability) {
            return Err(anyhow::Error::new(ProviderError::auth_failed(format!(
                "Stored Spotify connection is missing scopes required for {capability:?}"
            ))));
        }

        let acquired = AcquiredToken {
            access_token: response.access_token,
            expires_at: expires_at_from(response.expires_in),
            refresh: Some(RefreshMaterials {
                client_id: config.client_id.clone(),
                client_secret: config.client_secret.clone(),
                refresh_token: config.refresh_token.clone(),
            }),
        };
        Ok(Self::from_acquired(client, acquired))
    }

    fn from_acquired(client: reqwest::Client, acquired: AcquiredToken) -> Self {
        let tokens = SpotifyTokenManager::new(
            client.clone(),
            acquired.access_token,
            acquired.expires_at,
            acquired.refresh,
        );
        Self {
            client,
            tokens: Arc::new(tokens),
        }
    }

    pub async fn verify_connection(&self) -> Result<()> {
        let response = self
            .send_read(self.client.get("https://api.spotify.com/v1/me"))
            .await?;

        if !response.status().is_success() {
            return Err(response_error("Failed to verify Spotify connection", response).await);
        }

        Ok(())
    }

    /// Sends a bearer-authenticated read request through the token manager.
    ///
    /// The passed builder must carry every part of the request **except** the
    /// `Authorization` header — the token is injected here so it is always
    /// current and can be swapped on a reactive refresh.
    async fn send_read(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        self.send_authed(request, SPOTIFY_READ_RETRY_POLICY, SPOTIFY_READ_DELAY)
            .await
    }

    /// Sends a bearer-authenticated write request through the token manager.
    async fn send_write(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        self.send_authed(request, SPOTIFY_WRITE_RETRY_POLICY, SPOTIFY_WRITE_DELAY)
            .await
    }

    /// Attaches a valid bearer token and sends `request` under `policy`.
    ///
    /// On a `401 Unauthorized` the token manager is asked to refresh once and the
    /// request is replayed a single time with the fresh token. Any other status
    /// (including a `401` that survives the retry) is returned to the caller for
    /// classification.
    async fn send_authed(
        &self,
        request: reqwest::RequestBuilder,
        policy: SpotifyRetryPolicy,
        delay: Duration,
    ) -> Result<reqwest::Response> {
        // Preserve a clone before the request is consumed so the one reactive
        // retry after a forced refresh can rebuild it with a new token.
        let retry_spare = request.try_clone();

        let token = self.tokens.get_valid_token().await?;
        sleep(delay).await;
        let response = send_request_with_retry_policy(bearer(request, &token), policy).await?;

        if response.status() != StatusCode::UNAUTHORIZED {
            return Ok(response);
        }

        let Some(retry_request) = retry_spare else {
            return Ok(response);
        };

        let token = self.tokens.refresh_after_unauthorized().await?;
        sleep(delay).await;
        send_request_with_retry_policy(bearer(retry_request, &token), policy).await
    }

    pub fn authorization_url(
        capability: ProviderCapability,
        client_id: &str,
        redirect_uri: &Url,
        state: &str,
    ) -> Result<Url> {
        let scope = match capability {
            ProviderCapability::Read => SCOPE_READ,
            ProviderCapability::Write => SCOPE_WRITE,
            ProviderCapability::ReadWrite => SCOPE_ALL,
        };

        Url::parse_with_params(
            "https://accounts.spotify.com/authorize",
            &[
                ("client_id", client_id),
                ("response_type", "code"),
                ("redirect_uri", redirect_uri.as_str()),
                ("state", state),
                ("scope", scope),
            ],
        )
        .map_err(Into::into)
    }

    pub async fn exchange_authorization_code(
        client_id: &str,
        client_secret: &str,
        redirect_uri: &str,
        code: &str,
    ) -> Result<SpotifyConnectionConfig> {
        let client = build_http_client()?;
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );

        let params = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("client_secret", client_secret),
        ];

        let response = send_request_with_retry(
            client
                .post("https://accounts.spotify.com/api/token")
                .headers(headers)
                .form(&params),
        )
        .await
        .context("Failed to send Spotify authorization code request")?;

        if !response.status().is_success() {
            let status = response.status();
            let retry_after = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(parse_retry_after_seconds);
            let text = response
                .text()
                .await
                .unwrap_or_else(|_| "Could not read response text".to_string());
            return Err(spotify_status_error(
                "Spotify API request failed",
                status,
                retry_after,
                &text,
            ));
        }

        let response: AccessTokenResponse =
            response.json().await.context("Failed to parse response")?;
        let refresh_token = response
            .refresh_token
            .context("Spotify authorization response did not include a refresh token")?;

        Ok(SpotifyConnectionConfig {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            refresh_token,
        })
    }

    async fn get_all_items<T: DeserializeOwned>(&self, url: &str) -> Result<Vec<T>> {
        self.get_all_items_allow_forbidden(url)
            .await?
            .with_context(|| format!("Spotify unexpectedly denied access to {url}"))
    }

    async fn get_all_items_allow_forbidden<T: DeserializeOwned>(
        &self,
        url: &str,
    ) -> Result<Option<Vec<T>>> {
        let mut items = Vec::new();
        let mut next_url = Some(url.to_string());

        while let Some(url) = next_url {
            let response = self.send_read(self.client.get(&url)).await?;

            if response.status() == StatusCode::FORBIDDEN {
                return Ok(None);
            }

            if !response.status().is_success() {
                return Err(response_error(
                    &format!("Failed to get items from Spotify: {url}"),
                    response,
                )
                .await);
            }

            let mut data: Value = response.json().await?;
            let new_items: Vec<T> = serde_json::from_value(data["items"].take())?;
            items.extend(new_items);
            next_url = data["next"].as_str().map(ToOwned::to_owned);
        }

        Ok(Some(items))
    }

    async fn resolve_track(
        &self,
        metadata: &TrackMetadata,
        existing_provider_id: Option<&str>,
    ) -> Result<Option<(String, f64)>> {
        if let Some(provider_id) = existing_provider_id {
            return Ok(Some((provider_id.to_string(), 1.0)));
        }

        let match_profiles = spotify_match_profiles(metadata);
        let candidates = self
            .search_track_candidates(metadata, &match_profiles)
            .await?;
        Ok(best_candidate_for_profiles(&match_profiles, &candidates)
            .map(|candidate| (candidate.id, candidate.score)))
    }

    async fn search_track_candidates(
        &self,
        metadata: &TrackMetadata,
        match_profiles: &[TrackMetadata],
    ) -> Result<Vec<MatchCandidate>> {
        let queries = spotify_search_queries(metadata);

        let mut candidates = Vec::new();
        let mut seen_ids = HashSet::new();

        for query in queries {
            sleep(SPOTIFY_SEARCH_DELAY).await;
            let response = self
                .send_read(
                    self.client
                        .get("https://api.spotify.com/v1/search")
                        .query(&[("q", query.as_str()), ("type", "track"), ("limit", "10")]),
                )
                .await
                .context("Failed to search Spotify tracks")?;

            if !response.status().is_success() {
                return Err(response_error("Failed to search Spotify tracks", response).await);
            }

            let results: SpotifySearchResponse = response.json().await?;
            for candidate in results.tracks.items {
                let Some(id) = candidate.id else {
                    continue;
                };
                if !seen_ids.insert(id.clone()) {
                    continue;
                }

                candidates.push(MatchCandidate {
                    id,
                    title: candidate.name,
                    artists: candidate
                        .artists
                        .into_iter()
                        .map(|artist| artist.name)
                        .collect(),
                    album: if candidate.album.name.trim().is_empty() {
                        None
                    } else {
                        Some(candidate.album.name)
                    },
                    duration_seconds: candidate.duration_ms.map(|ms| ms / 1000),
                    source_weight: 0.05,
                });
            }

            if best_candidate_for_profiles(match_profiles, &candidates).is_some() {
                break;
            }
        }

        Ok(candidates)
    }

    async fn create_playlist(&self, name: &str, description: Option<&str>) -> Result<String> {
        let response = self
            .send_write(
                self.client
                    .post("https://api.spotify.com/v1/me/playlists")
                    .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                    .json(&json!({
                        "name": name,
                        "description": description.unwrap_or("Synced by spoti-dump"),
                        "public": false
                    })),
            )
            .await?;

        if !response.status().is_success() {
            return Err(response_error("Failed to create playlist", response).await);
        }

        let playlist: Value = response.json().await?;
        playlist["id"]
            .as_str()
            .map(ToOwned::to_owned)
            .context("Spotify create playlist response did not include an id")
    }

    async fn replace_playlist_tracks(
        &self,
        playlist_id: &str,
        track_uris: &[String],
    ) -> Result<()> {
        if self
            .playlist_already_contains(playlist_id, track_uris)
            .await?
        {
            return Ok(());
        }

        let url = playlist_items_url(playlist_id);

        let first_chunk = track_uris.iter().take(100).cloned().collect::<Vec<_>>();
        let response = self
            .send_write(
                self.client
                    .put(&url)
                    .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                    .json(&json!({ "uris": first_chunk })),
            )
            .await?;

        if !response.status().is_success() {
            return Err(response_error("Failed to replace Spotify playlist items", response).await);
        }

        for chunk in track_uris.iter().skip(100).collect::<Vec<_>>().chunks(100) {
            let response = self
                .send_write(
                    self.client
                        .post(&url)
                        .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                        .json(&json!({ "uris": chunk })),
                )
                .await?;

            if !response.status().is_success() {
                return Err(
                    response_error("Failed to append Spotify playlist items", response).await,
                );
            }
        }

        Ok(())
    }

    async fn playlist_already_contains(
        &self,
        playlist_id: &str,
        expected_track_uris: &[String],
    ) -> Result<bool> {
        let Some(items): Option<Vec<SpotifyPlaylistItem>> = self
            .get_all_items_allow_forbidden(&playlist_items_url(playlist_id))
            .await?
        else {
            return Ok(false);
        };
        let actual_track_uris = items
            .into_iter()
            .filter_map(|item| item.item.and_then(|track| track.id))
            .map(|track_id| Self::spotify_uri(&track_id))
            .collect::<Vec<_>>();

        Ok(actual_track_uris == expected_track_uris)
    }

    async fn current_saved_track_ids(&self) -> Result<HashSet<String>> {
        let saved_tracks: Vec<SpotifySavedTrack> = self
            .get_all_items("https://api.spotify.com/v1/me/tracks")
            .await?;
        Ok(saved_tracks
            .into_iter()
            .filter_map(|saved_track| saved_track.track.and_then(|track| track.id))
            .collect())
    }

    /// Saves `track_ids` to the library in chunks, returning the set of provider
    /// ids whose chunk failed to save.
    ///
    /// Per-chunk failures are recorded and skipped so one bad chunk does not sink
    /// the whole push; a typed rate-limit/auth failure or a tripped circuit
    /// breaker still aborts with `Err`. Track ids already present in the library
    /// count as successes (they are simply not re-sent).
    async fn save_tracks(
        &self,
        track_ids: &[String],
        breaker: &mut PushCircuitBreaker,
    ) -> Result<HashSet<String>> {
        let mut failed = HashSet::new();
        if track_ids.is_empty() {
            return Ok(failed);
        }

        let existing_ids = self.current_saved_track_ids().await?;
        let track_ids = track_ids
            .iter()
            .filter(|track_id| !existing_ids.contains(track_id.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if track_ids.is_empty() {
            return Ok(failed);
        }

        for chunk in track_ids.chunks(40) {
            let uris = chunk
                .iter()
                .map(|track_id| Self::spotify_uri(track_id))
                .collect::<Vec<_>>()
                .join(",");
            let result = self
                .send_write(
                    self.client
                        .put(spotify_library_url())
                        .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                        .header(CONTENT_LENGTH, HeaderValue::from_static("0"))
                        .query(&[("uris", uris)])
                        .body(String::new()),
                )
                .await;

            match push_chunk_outcome(result, "Failed to save tracks").await {
                Ok(()) => breaker.record_success(),
                Err(error) => {
                    if is_fatal_push_error(&error) {
                        return Err(error);
                    }
                    for track_id in chunk {
                        failed.insert(track_id.clone());
                    }
                    if breaker.record_failure() {
                        return Err(push_circuit_breaker_error());
                    }
                }
            }
        }

        Ok(failed)
    }

    async fn remove_library_items(&self, uris: &[String]) -> Result<()> {
        if uris.is_empty() {
            return Ok(());
        }

        for chunk in uris.chunks(40) {
            let response = self
                .send_write(
                    self.client
                        .delete(spotify_library_url())
                        .header(CONTENT_LENGTH, HeaderValue::from_static("0"))
                        .query(&[("uris", chunk.join(","))])
                        .body(String::new()),
                )
                .await?;

            if !response.status().is_success() {
                return Err(
                    response_error("Failed to remove Spotify library items", response).await,
                );
            }
        }

        Ok(())
    }

    async fn remove_saved_tracks(&self, track_ids: &[String]) -> Result<()> {
        let uris = track_ids
            .iter()
            .map(|track_id| Self::spotify_uri(track_id))
            .collect::<Vec<_>>();
        self.remove_library_items(&uris).await
    }

    async fn unfollow_playlist(&self, playlist_id: &str) -> Result<()> {
        self.remove_library_items(&[format!("spotify:playlist:{playlist_id}")])
            .await
    }

    fn spotify_uri(id: &str) -> String {
        format!("spotify:track:{id}")
    }
}

fn emit_progress(progress: Option<&ProgressHandler>, update: ProviderProgress) {
    if let Some(callback) = progress {
        callback(update);
    }
}

#[async_trait]
impl StreamingProvider for SpotifyProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Spotify
    }

    async fn verify_connection(&self) -> Result<()> {
        SpotifyProvider::verify_connection(self).await
    }

    async fn export_library_with_progress(
        &self,
        progress: Option<ProgressHandler>,
    ) -> Result<ProviderLibrarySnapshot> {
        let captured_at = Utc::now();
        emit_progress(
            progress.as_ref(),
            ProviderProgress {
                stage: "Fetching saved tracks".to_string(),
                detail: Some("Spotify".to_string()),
                ..Default::default()
            },
        );
        let saved_tracks: Vec<SpotifySavedTrack> = self
            .get_all_items("https://api.spotify.com/v1/me/tracks")
            .await?;
        emit_progress(
            progress.as_ref(),
            ProviderProgress {
                stage: "Fetching playlists".to_string(),
                detail: Some("Spotify".to_string()),
                saved_tracks_total: Some(saved_tracks.len()),
                ..Default::default()
            },
        );
        let playlists: Vec<SpotifyPlaylistSummary> = self
            .get_all_items("https://api.spotify.com/v1/me/playlists")
            .await?;

        let mut snapshot = ProviderLibrarySnapshot {
            provider: ProviderKind::Spotify,
            captured_at,
            saved_tracks: Vec::new(),
            playlists: Vec::new(),
            warnings: Vec::new(),
        };

        let saved_total = saved_tracks.len();
        for (index, saved_track) in saved_tracks.into_iter().enumerate() {
            if let Some(track) = saved_track.track.and_then(spotify_track_to_observed) {
                snapshot.saved_tracks.push(ObservedSavedTrack {
                    added_at: saved_track.added_at,
                    track,
                });
            }
            emit_progress(
                progress.as_ref(),
                ProviderProgress {
                    stage: "Pulling saved tracks".to_string(),
                    saved_tracks_done: index + 1,
                    saved_tracks_total: Some(saved_total),
                    playlists_total: Some(playlists.len()),
                    ..Default::default()
                },
            );
        }

        let playlists_total = playlists.len();
        let mut playlist_entries_done = 0usize;
        for (playlist_index, playlist) in playlists.into_iter().enumerate() {
            emit_progress(
                progress.as_ref(),
                ProviderProgress {
                    stage: "Fetching playlist".to_string(),
                    detail: Some(playlist.name.clone()),
                    saved_tracks_done: saved_total,
                    saved_tracks_total: Some(saved_total),
                    playlists_done: playlist_index,
                    playlists_total: Some(playlists_total),
                    ..Default::default()
                },
            );
            let url = playlist_items_url(&playlist.id);
            let Some(items): Option<Vec<SpotifyPlaylistItem>> =
                self.get_all_items_allow_forbidden(&url).await?
            else {
                snapshot.warnings.push(format!(
                    "Skipped Spotify playlist '{}' because Spotify only exposes items for playlists owned by the current user or playlists where the user is a collaborator.",
                    playlist.name
                ));
                continue;
            };
            let item_count = items.len();

            let mut observed_playlist = ObservedPlaylist {
                provider_id: Some(playlist.id),
                name: playlist.name,
                description: playlist.description,
                tracks: Vec::new(),
            };

            for item in items {
                if let Some(track) = item.item.and_then(spotify_track_to_observed) {
                    observed_playlist.tracks.push(ObservedPlaylistTrack {
                        added_at: item.added_at,
                        track,
                    });
                }
                playlist_entries_done += 1;
            }

            snapshot.playlists.push(observed_playlist);
            emit_progress(
                progress.as_ref(),
                ProviderProgress {
                    stage: "Pulling playlists".to_string(),
                    detail: Some(format!("{} tracks", item_count)),
                    saved_tracks_done: saved_total,
                    saved_tracks_total: Some(saved_total),
                    playlists_done: playlist_index + 1,
                    playlists_total: Some(playlists_total),
                    playlist_entries_done,
                    ..Default::default()
                },
            );
        }

        Ok(snapshot)
    }

    async fn resolve_track_identity(
        &self,
        metadata: &TrackMetadata,
    ) -> Result<Option<(String, f64)>> {
        self.resolve_track(metadata, None).await
    }

    async fn sync_library_with_progress(
        &self,
        state: &mut LibraryState,
        force: bool,
        progress: Option<ProgressHandler>,
    ) -> Result<SyncSummary> {
        let now = Utc::now();
        let saved_targets = state.saved_track_targets(ProviderKind::Spotify)?;
        let playlist_targets = state.playlist_targets(ProviderKind::Spotify)?;
        let mut summary = SyncSummary {
            saved_tracks_requested: saved_targets.len(),
            playlists_processed: playlist_targets.len(),
            playlist_entries_requested: playlist_targets
                .iter()
                .map(|playlist| playlist.entries.len())
                .sum(),
            ..Default::default()
        };
        let saved_total = summary.saved_tracks_requested;
        let playlist_total = summary.playlists_processed;
        let playlist_entries_total = summary.playlist_entries_requested;
        emit_progress(
            progress.as_ref(),
            ProviderProgress {
                stage: if force {
                    "Pushing saved tracks".to_string()
                } else {
                    "Resolving saved tracks".to_string()
                },
                saved_tracks_total: Some(saved_total),
                playlists_total: Some(playlist_total),
                playlist_entries_total: Some(playlist_entries_total),
                ..Default::default()
            },
        );

        let mut resolved_saved_tracks = Vec::new();
        let mut saved_done = 0usize;
        for target in saved_targets {
            let resolution = target
                .existing_provider_id
                .as_ref()
                .map(|provider_id| (provider_id.clone(), 1.0));
            match resolution {
                Some((provider_id, confidence)) => {
                    state.upsert_track_link(
                        &target.track_id,
                        ProviderKind::Spotify,
                        provider_id.clone(),
                        LinkSource::Match,
                        Some(confidence),
                        now,
                    );
                    state.set_track_status(
                        &target.track_id,
                        ProviderKind::Spotify,
                        SyncStatusRecord::synced(
                            Some(provider_id.clone()),
                            Some(confidence),
                            Some("Resolved on Spotify".to_string()),
                            now,
                        ),
                    );
                    resolved_saved_tracks.push((target.saved_track_id, provider_id, confidence));
                }
                None => {
                    summary.saved_tracks_unmatched += 1;
                    let reason = format!(
                        "No Spotify identity for {}. Run library identity sync before pushing.",
                        target.metadata.display_label()
                    );
                    state.set_track_status(
                        &target.track_id,
                        ProviderKind::Spotify,
                        SyncStatusRecord::unmatched(reason.clone(), now),
                    );
                    state.set_saved_track_status(
                        &target.saved_track_id,
                        ProviderKind::Spotify,
                        SyncStatusRecord::unmatched(reason, now),
                    );
                }
            }
            saved_done += 1;
            emit_progress(
                progress.as_ref(),
                ProviderProgress {
                    stage: if force {
                        "Pushing saved tracks".to_string()
                    } else {
                        "Resolving saved tracks".to_string()
                    },
                    saved_tracks_done: saved_done,
                    saved_tracks_total: Some(saved_total),
                    playlists_total: Some(playlist_total),
                    playlist_entries_total: Some(playlist_entries_total),
                    ..Default::default()
                },
            );
        }

        // One circuit breaker spans the whole push (saved tracks and playlists):
        // 5 consecutive per-item failures anywhere aborts the operation.
        let mut push_breaker = PushCircuitBreaker::new(PUSH_CONSECUTIVE_FAILURE_LIMIT);

        let failed_saved_ids = if force {
            let track_ids = resolved_saved_tracks
                .iter()
                .map(|(_, provider_id, _)| provider_id.clone())
                .collect::<Vec<_>>();
            self.save_tracks(&track_ids, &mut push_breaker).await?
        } else {
            HashSet::new()
        };

        for (saved_track_id, provider_id, confidence) in resolved_saved_tracks {
            if force && failed_saved_ids.contains(&provider_id) {
                let message =
                    format!("Failed to save track {provider_id} to Spotify; recorded for retry.");
                summary.warnings.push(message.clone());
                state.set_saved_track_status(
                    &saved_track_id,
                    ProviderKind::Spotify,
                    SyncStatusRecord::error_with_provider_item_id(
                        message,
                        provider_id,
                        Some(confidence),
                        now,
                    ),
                );
                continue;
            }
            summary.saved_tracks_synced += 1;
            state.set_saved_track_status(
                &saved_track_id,
                ProviderKind::Spotify,
                SyncStatusRecord::synced(
                    Some(provider_id),
                    Some(confidence),
                    Some(if force {
                        "Synced to Spotify".to_string()
                    } else {
                        "Resolved for Spotify sync dry run".to_string()
                    }),
                    now,
                ),
            );
        }

        let needs_playlist_name_lookup = playlist_targets
            .iter()
            .any(|playlist| playlist.existing_provider_id.is_none());
        let playlists_by_name = if force && needs_playlist_name_lookup {
            let playlists: Vec<SpotifyPlaylistSummary> = self
                .get_all_items("https://api.spotify.com/v1/me/playlists")
                .await?;
            Some(
                playlists
                    .into_iter()
                    .map(|playlist| (normalized_playlist_name_key(&playlist.name), playlist))
                    .collect::<std::collections::HashMap<_, _>>(),
            )
        } else {
            None
        };

        let mut playlist_entries_done = 0usize;
        for (playlist_index, playlist) in playlist_targets.into_iter().enumerate() {
            let mut resolved_track_uris = Vec::new();
            let mut matched_entries = Vec::new();

            for entry in &playlist.entries {
                let resolution = entry
                    .existing_provider_id
                    .as_ref()
                    .map(|provider_id| (provider_id.clone(), 1.0));
                match resolution {
                    Some((provider_id, confidence)) => {
                        state.upsert_track_link(
                            &entry.track_id,
                            ProviderKind::Spotify,
                            provider_id.clone(),
                            LinkSource::Match,
                            Some(confidence),
                            now,
                        );
                        state.set_track_status(
                            &entry.track_id,
                            ProviderKind::Spotify,
                            SyncStatusRecord::synced(
                                Some(provider_id.clone()),
                                Some(confidence),
                                Some("Resolved on Spotify".to_string()),
                                now,
                            ),
                        );
                        resolved_track_uris.push(Self::spotify_uri(&provider_id));
                        matched_entries.push((entry.entry_id.clone(), provider_id, confidence));
                    }
                    None => {
                        summary.playlist_entries_unmatched += 1;
                        let reason = format!(
                            "No Spotify identity for {}. Run library identity sync before pushing.",
                            entry.metadata.display_label()
                        );
                        state.set_track_status(
                            &entry.track_id,
                            ProviderKind::Spotify,
                            SyncStatusRecord::unmatched(reason.clone(), now),
                        );
                        state.set_playlist_entry_status(
                            &playlist.playlist_id,
                            &entry.entry_id,
                            ProviderKind::Spotify,
                            SyncStatusRecord::unmatched(
                                format!("{reason} in playlist {}", playlist.name),
                                now,
                            ),
                        );
                    }
                }
                playlist_entries_done += 1;
                emit_progress(
                    progress.as_ref(),
                    ProviderProgress {
                        stage: if force {
                            "Pushing playlists".to_string()
                        } else {
                            "Resolving playlists".to_string()
                        },
                        detail: Some(playlist.name.clone()),
                        saved_tracks_done: saved_done,
                        saved_tracks_total: Some(saved_total),
                        playlists_done: playlist_index,
                        playlists_total: Some(playlist_total),
                        playlist_entries_done,
                        playlist_entries_total: Some(playlist_entries_total),
                    },
                );
            }

            // Set when a playlist's push fails non-fatally: its entries are then
            // marked errored (not synced) and the loop moves on to the next
            // playlist rather than aborting the whole run.
            let mut playlist_push_failed: Option<String> = None;

            if force {
                if playlist.entries.is_empty() && is_spotify_system_playlist_name(&playlist.name) {
                    state.set_playlist_status(
                        &playlist.playlist_id,
                        ProviderKind::Spotify,
                        SyncStatusRecord::skipped(
                            format!(
                                "Skipped Spotify-managed empty playlist '{}'.",
                                playlist.name
                            ),
                            now,
                        ),
                    );
                    emit_progress(
                        progress.as_ref(),
                        ProviderProgress {
                            stage: "Pushing playlists".to_string(),
                            detail: Some(playlist.name.clone()),
                            saved_tracks_done: saved_done,
                            saved_tracks_total: Some(saved_total),
                            playlists_done: playlist_index + 1,
                            playlists_total: Some(playlist_total),
                            playlist_entries_done,
                            playlist_entries_total: Some(playlist_entries_total),
                        },
                    );
                    continue;
                }

                // Guard against clobbering a non-empty destination with an empty
                // list when a transient matching failure resolved nothing.
                if should_skip_empty_playlist_replace(
                    resolved_track_uris.len(),
                    playlist.entries.len(),
                ) {
                    let message = format!(
                        "Skipped Spotify playlist '{}': none of its {} tracks resolved to Spotify \
                         identities, so the destination was left untouched to avoid clearing it. \
                         Run library identity sync and retry.",
                        playlist.name,
                        playlist.entries.len()
                    );
                    summary.warnings.push(message.clone());
                    state.set_playlist_status(
                        &playlist.playlist_id,
                        ProviderKind::Spotify,
                        SyncStatusRecord::unmatched(message, now),
                    );
                    emit_progress(
                        progress.as_ref(),
                        ProviderProgress {
                            stage: "Pushing playlists".to_string(),
                            detail: Some(playlist.name.clone()),
                            saved_tracks_done: saved_done,
                            saved_tracks_total: Some(saved_total),
                            playlists_done: playlist_index + 1,
                            playlists_total: Some(playlist_total),
                            playlist_entries_done,
                            playlist_entries_total: Some(playlist_entries_total),
                        },
                    );
                    continue;
                }

                let playlist_provider_id = if let Some(provider_id) = &playlist.existing_provider_id
                {
                    provider_id.clone()
                } else if let Some(existing) = playlists_by_name.as_ref().and_then(|playlists| {
                    playlists.get(&normalized_playlist_name_key(&playlist.name))
                }) {
                    state.upsert_playlist_link(
                        &playlist.playlist_id,
                        ProviderKind::Spotify,
                        existing.id.clone(),
                        LinkSource::Match,
                        Some(1.0),
                        now,
                    );
                    existing.id.clone()
                } else {
                    let created = self
                        .create_playlist(&playlist.name, playlist.description.as_deref())
                        .await?;
                    state.upsert_playlist_link(
                        &playlist.playlist_id,
                        ProviderKind::Spotify,
                        created.clone(),
                        LinkSource::Create,
                        Some(1.0),
                        now,
                    );
                    created
                };

                match self
                    .replace_playlist_tracks(&playlist_provider_id, &resolved_track_uris)
                    .await
                {
                    Ok(()) => {
                        push_breaker.record_success();
                        state.set_playlist_status(
                            &playlist.playlist_id,
                            ProviderKind::Spotify,
                            SyncStatusRecord::synced(
                                Some(playlist_provider_id),
                                Some(1.0),
                                Some("Synced to Spotify".to_string()),
                                now,
                            ),
                        );
                    }
                    Err(error) if is_fatal_push_error(&error) => {
                        state.set_playlist_status(
                            &playlist.playlist_id,
                            ProviderKind::Spotify,
                            SyncStatusRecord::error(
                                format!(
                                    "Failed to sync Spotify playlist '{}': {error}",
                                    playlist.name
                                ),
                                now,
                            ),
                        );
                        return Err(error);
                    }
                    Err(error) => {
                        let message = format!(
                            "Failed to sync Spotify playlist '{}': {error}",
                            playlist.name
                        );
                        summary.warnings.push(message.clone());
                        state.set_playlist_status(
                            &playlist.playlist_id,
                            ProviderKind::Spotify,
                            SyncStatusRecord::error(message.clone(), now),
                        );
                        playlist_push_failed = Some(message);
                        if push_breaker.record_failure() {
                            return Err(push_circuit_breaker_error());
                        }
                    }
                }
            }

            for (entry_id, provider_id, confidence) in matched_entries {
                if let Some(reason) = playlist_push_failed.as_ref() {
                    state.set_playlist_entry_status(
                        &playlist.playlist_id,
                        &entry_id,
                        ProviderKind::Spotify,
                        SyncStatusRecord::error_with_provider_item_id(
                            reason.clone(),
                            provider_id,
                            Some(confidence),
                            now,
                        ),
                    );
                    continue;
                }
                summary.playlist_entries_synced += 1;
                state.set_playlist_entry_status(
                    &playlist.playlist_id,
                    &entry_id,
                    ProviderKind::Spotify,
                    SyncStatusRecord::synced(
                        Some(provider_id),
                        Some(confidence),
                        Some(if force {
                            "Synced to Spotify".to_string()
                        } else {
                            "Resolved for Spotify sync dry run".to_string()
                        }),
                        now,
                    ),
                );
            }

            if !force {
                state.set_playlist_status(
                    &playlist.playlist_id,
                    ProviderKind::Spotify,
                    SyncStatusRecord::synced(
                        playlist.existing_provider_id.clone(),
                        Some(1.0),
                        Some("Resolved for Spotify sync dry run".to_string()),
                        now,
                    ),
                );
            }
            emit_progress(
                progress.as_ref(),
                ProviderProgress {
                    stage: if force {
                        "Pushing playlists".to_string()
                    } else {
                        "Resolving playlists".to_string()
                    },
                    detail: Some(playlist.name.clone()),
                    saved_tracks_done: saved_done,
                    saved_tracks_total: Some(saved_total),
                    playlists_done: playlist_index + 1,
                    playlists_total: Some(playlist_total),
                    playlist_entries_done,
                    playlist_entries_total: Some(playlist_entries_total),
                },
            );
        }

        Ok(summary)
    }

    async fn purge_library(&self, force: bool) -> Result<PurgeReport> {
        let tracks: Vec<Value> = self
            .get_all_items("https://api.spotify.com/v1/me/tracks")
            .await?;
        let playlists: Vec<SpotifyPlaylistSummary> = self
            .get_all_items("https://api.spotify.com/v1/me/playlists")
            .await?;

        let track_uris: Vec<String> = tracks
            .into_iter()
            .filter_map(|track| track["track"]["id"].as_str().map(Self::spotify_uri))
            .collect();

        let playlist_ids: Vec<String> = playlists
            .iter()
            .map(|playlist| playlist.id.clone())
            .collect();
        let report = PurgeReport {
            saved_tracks: track_uris.len(),
            playlists: playlist_ids.len(),
        };

        if !force {
            println!(
                "Dry run: would remove {} saved tracks and unfollow {} playlists from Spotify.",
                report.saved_tracks, report.playlists
            );
            return Ok(report);
        }

        self.remove_library_items(&track_uris).await?;

        let playlist_uris = playlist_ids
            .iter()
            .map(|playlist_id| format!("spotify:playlist:{playlist_id}"))
            .collect::<Vec<_>>();
        self.remove_library_items(&playlist_uris).await?;

        Ok(report)
    }

    async fn remove_saved_track(&self, provider_track_id: &str) -> Result<()> {
        self.remove_saved_tracks(&[provider_track_id.to_string()])
            .await
    }

    async fn delete_playlist(&self, provider_playlist_id: &str) -> Result<()> {
        self.unfollow_playlist(provider_playlist_id).await
    }
}

fn spotify_track_to_observed(track: SpotifyTrack) -> Option<ObservedTrack> {
    let provider_id = track.id?;
    let artwork = track
        .album
        .images
        .iter()
        .max_by_key(|image| image.width.unwrap_or(0) * image.height.unwrap_or(0))
        .map(|image| ObservedArtwork {
            url: image.url.clone(),
            width: image.width,
            height: image.height,
        });
    Some(ObservedTrack {
        metadata: TrackMetadata {
            title: track.name,
            artists: track
                .artists
                .into_iter()
                .map(|artist| artist.name)
                .collect(),
            album: if track.album.name.trim().is_empty() {
                None
            } else {
                Some(track.album.name)
            },
            duration_seconds: track.duration_ms.map(|ms| ms / 1000),
            isrc: track.external_ids.isrc,
        },
        provider_id: Some(provider_id),
        artwork,
    })
}

fn spotify_search_queries(metadata: &TrackMetadata) -> Vec<String> {
    let mut queries = Vec::new();
    let primary_artist = metadata.artists.first().map(String::as_str).unwrap_or("");

    for title in title_search_variants(&metadata.title) {
        push_search_query(
            &mut queries,
            format!("track:{title} artist:{primary_artist}"),
        );
        push_search_query(&mut queries, format!("{primary_artist} {title}"));
    }

    if let Some((song, artist)) = song_artist_from_video_title(&metadata.title) {
        push_search_query(&mut queries, format!("track:{song} artist:{artist}"));
        push_search_query(&mut queries, format!("{artist} {song}"));
    }

    queries
}

fn push_search_query(queries: &mut Vec<String>, query: String) {
    let query = query.split_whitespace().collect::<Vec<_>>().join(" ");
    if !query.is_empty() && !queries.iter().any(|existing| existing == &query) {
        queries.push(query);
    }
}

fn spotify_match_profiles(metadata: &TrackMetadata) -> Vec<TrackMetadata> {
    let mut profiles = Vec::new();
    profiles.push(metadata.clone());

    for title in title_search_variants(&metadata.title) {
        if title != metadata.title {
            let mut profile = metadata.clone();
            profile.title = title;
            profiles.push(profile);
        }
    }

    if let Some((song, artist)) = song_artist_from_video_title(&metadata.title) {
        let mut profile = metadata.clone();
        profile.title = song;
        profile.artists = vec![artist];
        profile.album = None;
        profiles.push(profile);
    }

    profiles
}

fn title_search_variants(title: &str) -> Vec<String> {
    let mut variants = Vec::new();
    push_title_variant(&mut variants, cleaned_title(title));

    for segment in title.split('|') {
        push_title_variant(&mut variants, cleaned_title(segment));
        if let Some((song, _)) = song_artist_from_video_title(segment) {
            push_title_variant(&mut variants, song);
        }
    }

    variants
}

fn push_title_variant(variants: &mut Vec<String>, title: String) {
    let title = title.trim();
    if !title.is_empty() && !variants.iter().any(|existing| existing == title) {
        variants.push(title.to_string());
    }
}

fn song_artist_from_video_title(title: &str) -> Option<(String, String)> {
    let lowered = title.to_ascii_lowercase();
    let marker = " by ";
    let by_index = lowered.find(marker)?;
    let prefix = title[..by_index].trim();
    let suffix = title[by_index + marker.len()..].trim();
    let song = prefix
        .rsplit(['|', '\u{2013}', '\u{2014}'])
        .next()
        .unwrap_or(prefix)
        .trim();
    let artist = suffix
        .split(['|', '\u{2013}', '\u{2014}'])
        .next()
        .unwrap_or(suffix)
        .trim();

    if song.is_empty() || artist.is_empty() {
        return None;
    }

    Some((cleaned_title(song), artist.to_string()))
}

fn best_candidate_for_profiles(
    profiles: &[TrackMetadata],
    candidates: &[MatchCandidate],
) -> Option<crate::matching::RankedCandidate> {
    profiles
        .iter()
        .filter_map(|profile| best_candidate(profile, candidates))
        .max_by(|left, right| {
            left.score
                .partial_cmp(&right.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

async fn acquire_env_token(
    client: &reqwest::Client,
    capability: ProviderCapability,
) -> Result<AcquiredToken> {
    let redirect_uri =
        Url::parse(REDIRECT_URI).expect("Hard-coded redirect URI should always be valid");

    let client_id = env::var("SPOTIFY_CLIENT_ID").context("SPOTIFY_CLIENT_ID not set")?;
    let client_secret =
        env::var("SPOTIFY_CLIENT_SECRET").context("SPOTIFY_CLIENT_SECRET not set")?;

    if let Ok(refresh_token) = env::var("SPOTIFY_REFRESH_TOKEN") {
        if !refresh_token.is_empty() {
            let response =
                refresh_access_token_with(client, &refresh_token, &client_id, &client_secret)
                    .await?;
            if token_has_required_scopes(response.scope.as_deref(), capability) {
                return Ok(AcquiredToken {
                    access_token: response.access_token,
                    expires_at: expires_at_from(response.expires_in),
                    refresh: Some(RefreshMaterials {
                        client_id,
                        client_secret,
                        refresh_token,
                    }),
                });
            }

            eprintln!(
                "Stored Spotify refresh token is missing scopes required for this command. Re-authorizing."
            );
        }
    }

    acquire_token_via_authorization_code(
        client,
        capability,
        &client_id,
        &client_secret,
        &redirect_uri,
    )
    .await
}

async fn acquire_token_via_authorization_code(
    client: &reqwest::Client,
    capability: ProviderCapability,
    client_id: &str,
    client_secret: &str,
    redirect_uri: &Url,
) -> Result<AcquiredToken> {
    let (code, _) = get_authorization_code(capability, client_id, redirect_uri).await?;

    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    );

    let params = [
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", REDIRECT_URI),
        ("client_id", client_id),
        ("client_secret", client_secret),
    ];

    let response = send_request_with_retry(
        client
            .post("https://accounts.spotify.com/api/token")
            .headers(headers)
            .form(&params),
    )
    .await
    .context("Failed to send Spotify authorization code request")?;

    if !response.status().is_success() {
        let status = response.status();
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_retry_after_seconds);
        let text = response
            .text()
            .await
            .unwrap_or_else(|_| "Could not read response text".to_string());
        return Err(spotify_status_error(
            "Spotify API request failed",
            status,
            retry_after,
            &text,
        ));
    }

    let response: AccessTokenResponse =
        response.json().await.context("Failed to parse response")?;

    let expires_at = expires_at_from(response.expires_in);
    let refresh = match response.refresh_token {
        Some(refresh_token) => {
            // Persist the connection so later runs reuse it instead of forcing a
            // fresh browser authorization every time.
            match persist_spotify_connection(client_id, client_secret, &refresh_token) {
                Ok(()) => println!(
                    "Stored the Spotify connection; later runs will reuse it without re-authorizing."
                ),
                Err(error) => eprintln!(
                    "Warning: authorized with Spotify but could not persist the connection for \
                     reuse ({error}). You may be prompted to authorize again next run."
                ),
            }
            Some(RefreshMaterials {
                client_id: client_id.to_string(),
                client_secret: client_secret.to_string(),
                refresh_token,
            })
        }
        None => {
            eprintln!(
                "Spotify did not return a refresh token, so this session cannot renew its access \
                 token automatically once it expires. Reconnect Spotify to store a refresh token."
            );
            None
        }
    };

    Ok(AcquiredToken {
        access_token: response.access_token,
        expires_at,
        refresh,
    })
}

/// Persists a Spotify [`ProviderConnection`] so subsequent runs reuse it. The
/// `.env` file remains the source of `client_id`/`client_secret`; only the
/// refresh token (obtained via OAuth) is added here. Secrets are never logged.
fn persist_spotify_connection(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<()> {
    let now = Utc::now();
    let connection = ProviderConnection {
        provider: ProviderKind::Spotify,
        connected_at: now,
        updated_at: now,
        config: ProviderConnectionConfig::Spotify(SpotifyConnectionConfig {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            refresh_token: refresh_token.to_string(),
        }),
    };
    crate::storage::save_provider_connection(&connection)
}

async fn refresh_access_token_with(
    client: &reqwest::Client,
    refresh_token: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<AccessTokenResponse> {
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/x-www-form-urlencoded"),
    );

    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
        ("client_secret", client_secret),
    ];

    let response = send_request_with_retry(
        client
            .post("https://accounts.spotify.com/api/token")
            .headers(headers)
            .form(&params),
    )
    .await
    .context("Failed to refresh Spotify access token")?;

    if !response.status().is_success() {
        let status = response.status();
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_retry_after_seconds);
        let text = response
            .text()
            .await
            .unwrap_or_else(|_| "Could not read response text".to_string());
        return Err(spotify_status_error(
            "Spotify API request failed",
            status,
            retry_after,
            &text,
        ));
    }

    response
        .json()
        .await
        .context("Failed to parse refresh response")
}

fn token_has_required_scopes(scope: Option<&str>, capability: ProviderCapability) -> bool {
    let Some(scope) = scope else {
        return false;
    };

    let granted_scopes: HashSet<&str> = scope.split_whitespace().collect();
    required_scopes(capability)
        .iter()
        .all(|required_scope| granted_scopes.contains(required_scope))
}

fn required_scopes(capability: ProviderCapability) -> &'static [&'static str] {
    match capability {
        ProviderCapability::Read => &["user-library-read", "playlist-read-private"],
        ProviderCapability::Write => &[
            // The push path reads before it writes, so Write requires the read
            // scopes it uses in addition to the modify scopes.
            "user-library-read",
            "user-library-modify",
            "playlist-read-private",
            "playlist-modify-public",
            "playlist-modify-private",
        ],
        ProviderCapability::ReadWrite => &[
            "user-library-read",
            "user-library-modify",
            "playlist-read-private",
            "playlist-modify-public",
            "playlist-modify-private",
        ],
    }
}

async fn get_authorization_code(
    capability: ProviderCapability,
    client_id: &str,
    redirect_uri: &Url,
) -> Result<(String, String)> {
    let state = {
        let random_bytes: Vec<u8> = (0..16).map(|_| rand::random::<u8>()).collect();
        general_purpose::URL_SAFE_NO_PAD.encode(&random_bytes)
    };

    let scope = match capability {
        ProviderCapability::Read => SCOPE_READ,
        ProviderCapability::Write => SCOPE_WRITE,
        ProviderCapability::ReadWrite => SCOPE_ALL,
    };

    let auth_url = Url::parse_with_params(
        "https://accounts.spotify.com/authorize",
        &[
            ("client_id", client_id),
            ("response_type", "code"),
            ("redirect_uri", redirect_uri.as_str()),
            ("state", &state),
            ("scope", scope),
        ],
    )?;

    let host = redirect_uri
        .host_str()
        .context("Redirect URI must include a host")?
        .to_string();
    let port = redirect_uri
        .port_or_known_default()
        .context("Redirect URI must include a port")?;
    let server_addr = format!("{host}:{port}");

    match open::that(auth_url.as_str()) {
        Ok(()) => println!("Opened your browser for Spotify authorization."),
        Err(error) => {
            eprintln!(
                "Failed to launch the browser automatically ({error}). Please open this URL manually:"
            );
            println!("{auth_url}");
        }
    }

    println!("Waiting for Spotify authorization... (will time out in 2 minutes)");

    // `tiny_http`'s bind/accept/recv are blocking, so run them on the blocking
    // pool instead of stalling the async runtime. `Ok(Some(raw_url))` carries the
    // callback path+query, `Ok(None)` means the wait timed out.
    let raw_callback_url = tokio::task::spawn_blocking(move || -> Result<Option<String>> {
        let server = Server::http(&server_addr)
            .map_err(|error| anyhow::anyhow!("Failed to start local callback server: {error}"))?;
        match server.recv_timeout(Duration::from_secs(120)) {
            Ok(Some(request)) => {
                let raw_url = request.url().to_string();
                let response = Response::from_string(
                    "Spotify authorization received. You can close this tab and return to the CLI.",
                );
                let _ = request.respond(response);
                Ok(Some(raw_url))
            }
            Ok(None) => Ok(None),
            Err(error) => Err(anyhow::anyhow!("Local callback server error: {error}")),
        }
    })
    .await
    .context("Spotify authorization callback task failed")??;

    let Some(raw_callback_url) = raw_callback_url else {
        anyhow::bail!("Timed out waiting for Spotify authorization callback");
    };

    let callback_url = format!("http://{host}:{port}{raw_callback_url}");
    let url = Url::parse(&callback_url)?;

    // Spotify redirects with `?error=access_denied` (or similar) when the user
    // declines the consent screen; surface that clearly instead of a generic
    // "no code" failure.
    if let Some(error) = url
        .query_pairs()
        .find(|(key, _)| key == "error")
        .map(|(_, value)| value.into_owned())
    {
        anyhow::bail!(
            "Spotify authorization was not granted (error: {error}). If you declined the consent \
             screen, re-run the command and approve access."
        );
    }

    let code = url
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.into_owned())
        .context("No code found in callback URL")?;
    let returned_state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .context("No state found in callback URL")?;

    if returned_state != state {
        anyhow::bail!("OAuth state mismatch");
    }

    Ok((code, returned_state))
}

/// Attaches the bearer token to a request builder just before it is sent, so a
/// reactive refresh can swap in a fresh token on the retry.
fn bearer(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request.header(AUTHORIZATION, format!("Bearer {token}"))
}

/// Trips a push after too many *consecutive* per-item failures. A single
/// success resets the count, so isolated hiccups are tolerated while a sustained
/// outage aborts fast instead of hammering every remaining item.
struct PushCircuitBreaker {
    consecutive_failures: usize,
    limit: usize,
}

impl PushCircuitBreaker {
    fn new(limit: usize) -> Self {
        Self {
            consecutive_failures: 0,
            limit,
        }
    }

    fn record_success(&mut self) {
        self.consecutive_failures = 0;
    }

    /// Records a failure and returns `true` if the breaker has now tripped.
    fn record_failure(&mut self) -> bool {
        self.consecutive_failures += 1;
        self.consecutive_failures >= self.limit
    }
}

fn push_circuit_breaker_error() -> anyhow::Error {
    anyhow::anyhow!(
        "Aborting Spotify push after {PUSH_CONSECUTIVE_FAILURE_LIMIT} consecutive item failures; \
         the provider appears unhealthy. Resolve the underlying issue and re-run the sync."
    )
}

/// Collapses a raw send result into success or a classified error at the
/// boundary, so calling loops match on the typed failure rather than the raw
/// HTTP status.
async fn push_chunk_outcome(result: Result<reqwest::Response>, context: &str) -> Result<()> {
    match result {
        Ok(response) if response.status().is_success() => Ok(()),
        Ok(response) => Err(response_error(context, response).await),
        Err(error) => Err(error),
    }
}

/// Whether a push error must abort the whole operation instead of being recorded
/// against a single item. Typed rate-limit and auth failures are fatal because
/// pushing further items cannot succeed until they are resolved.
fn is_fatal_push_error(error: &anyhow::Error) -> bool {
    matches!(
        provider_failure(error).map(ProviderError::failure),
        Some(ProviderFailure::RateLimited { .. } | ProviderFailure::AuthFailed)
    )
}

/// Whether a playlist replace should be skipped to avoid clobbering a non-empty
/// destination with an empty track list when every entry failed to resolve.
/// Pure so the data-loss guard is unit-testable in isolation.
fn should_skip_empty_playlist_replace(resolved_uris: usize, canonical_entries: usize) -> bool {
    resolved_uris == 0 && canonical_entries > 0
}

async fn response_error(context: &str, response: reqwest::Response) -> anyhow::Error {
    let status = response.status();
    let retry_after = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after_seconds);
    let body = response
        .text()
        .await
        .unwrap_or_else(|_| "Could not read response body".to_string());
    spotify_status_error(context, status, retry_after, &body)
}

/// Builds the typed [`ProviderError`] for a non-success Spotify HTTP response.
///
/// This is the Spotify classification boundary: the `Retry-After` header is
/// carried structurally as a [`Duration`] on the rate-limit variant, so nothing
/// downstream re-parses message text.
fn spotify_status_error(
    context: &str,
    status: StatusCode,
    retry_after: Option<u64>,
    body: &str,
) -> anyhow::Error {
    let message = format!("{context} ({status}): {body}");
    let provider_error = match status {
        StatusCode::TOO_MANY_REQUESTS => {
            ProviderError::rate_limited(message, retry_after.map(Duration::from_secs))
        }
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ProviderError::auth_failed(message),
        StatusCode::BAD_REQUEST => ProviderError::invalid_argument(message),
        other => ProviderError::http(message, other.as_u16()),
    };
    anyhow::Error::new(provider_error)
}

/// Classifies a transport-level `reqwest` error, mapping connect/timeout
/// failures to [`ProviderFailure::Network`](crate::error::ProviderFailure).
fn spotify_transport_error(error: reqwest::Error) -> anyhow::Error {
    if error.is_connect() || error.is_timeout() {
        anyhow::Error::new(ProviderError::network(format!(
            "Spotify request transport failure: {error}"
        )))
    } else {
        error.into()
    }
}

fn build_http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?)
}

/// Read-tier wrapper used by the token endpoints (which carry client
/// credentials, not a bearer token, so they bypass the token manager).
async fn send_request_with_retry(request: reqwest::RequestBuilder) -> Result<reqwest::Response> {
    sleep(SPOTIFY_READ_DELAY).await;
    send_request_with_retry_policy(request, SPOTIFY_READ_RETRY_POLICY).await
}

async fn send_request_with_retry_policy(
    request: reqwest::RequestBuilder,
    policy: SpotifyRetryPolicy,
) -> Result<reqwest::Response> {
    for attempt in 0..policy.attempts {
        let request = request
            .try_clone()
            .context("Spotify request body could not be cloned for retry")?;
        match request.send().await {
            Ok(response)
                if should_retry_spotify_status(response.status())
                    && attempt + 1 < policy.attempts =>
            {
                let status = response.status();
                if should_defer_spotify_retry(status, response.headers(), policy) {
                    return Ok(response);
                }
                let delay = spotify_retry_delay(status, response.headers(), attempt, policy);
                eprintln!(
                    "Spotify returned {status}; retrying in {}s (attempt {} of {}).",
                    delay.as_secs(),
                    attempt + 1,
                    policy.attempts
                );
                sleep(delay).await;
            }
            Ok(response) => return Ok(response),
            Err(error) if attempt + 1 < policy.attempts => {
                let delay = spotify_retry_delay(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &HeaderMap::new(),
                    attempt,
                    policy,
                );
                eprintln!(
                    "Spotify request failed transiently; retrying in {}s (attempt {} of {}).",
                    delay.as_secs(),
                    attempt + 1,
                    policy.attempts
                );
                sleep(delay).await;
                if error.is_builder() {
                    return Err(spotify_transport_error(error));
                }
            }
            Err(error) => return Err(spotify_transport_error(error)),
        }
    }

    anyhow::bail!("Spotify request failed after {} attempts", policy.attempts)
}

fn should_retry_spotify_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn should_defer_spotify_retry(
    status: StatusCode,
    headers: &HeaderMap,
    policy: SpotifyRetryPolicy,
) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS
        && headers
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_retry_after_seconds)
            .map(|seconds| seconds > policy.rate_limit_max_delay_secs)
            .unwrap_or(false)
}

fn spotify_retry_delay(
    status: StatusCode,
    headers: &HeaderMap,
    attempt: usize,
    policy: SpotifyRetryPolicy,
) -> Duration {
    let retry_after = headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after_seconds);
    let fallback = if status == StatusCode::TOO_MANY_REQUESTS {
        policy.rate_limit_fallback_secs
    } else {
        1 << attempt.min(4)
    };
    let delay = retry_after.unwrap_or(fallback);
    let delay = if status == StatusCode::TOO_MANY_REQUESTS && retry_after.is_some() {
        delay.saturating_add(SPOTIFY_RETRY_AFTER_BUFFER_SECS)
    } else {
        delay
    };
    let max_delay = if status == StatusCode::TOO_MANY_REQUESTS {
        policy.rate_limit_max_delay_secs
    } else {
        60
    };
    Duration::from_secs(delay.min(max_delay))
}

fn parse_retry_after_seconds(value: &str) -> Option<u64> {
    let value = value.trim();
    value.parse::<u64>().ok().or_else(|| {
        value
            .parse::<f64>()
            .ok()
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
            .map(|seconds| seconds.ceil() as u64)
    })
}

fn playlist_items_url(playlist_id: &str) -> String {
    format!("https://api.spotify.com/v1/playlists/{playlist_id}/items")
}

fn normalized_playlist_name_key(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

fn spotify_library_url() -> &'static str {
    "https://api.spotify.com/v1/me/library"
}

fn is_spotify_system_playlist_name(name: &str) -> bool {
    matches!(name, "Episodes for Later" | "New Episodes")
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
    use reqwest::StatusCode;

    use super::{
        best_candidate_for_profiles, expires_at_from, is_fatal_push_error,
        is_spotify_system_playlist_name, parse_retry_after_seconds, playlist_items_url,
        required_scopes, should_defer_spotify_retry, should_retry_spotify_status,
        should_skip_empty_playlist_replace, song_artist_from_video_title, spotify_library_url,
        spotify_match_profiles, spotify_retry_delay, spotify_search_queries,
        token_has_required_scopes, token_is_stale, PushCircuitBreaker, SpotifyPlaylistItem,
        PUSH_CONSECUTIVE_FAILURE_LIMIT, SCOPE_ALL, SCOPE_WRITE, SPOTIFY_READ_RETRY_POLICY,
        SPOTIFY_WRITE_RETRY_POLICY, TOKEN_EXPIRY_SAFETY_MARGIN_SECS,
    };
    use crate::domain::TrackMetadata;
    use crate::error::ProviderError;
    use crate::matching::MatchCandidate;
    use crate::provider::ProviderCapability;

    #[test]
    fn parses_current_spotify_playlist_item_shape() {
        let item: SpotifyPlaylistItem = serde_json::from_str(
            r#"{
                "added_at": "2026-01-01T00:00:00Z",
                "item": {
                    "id": "spotify-track-1",
                    "name": "Sirius",
                    "artists": [{"name": "The Alan Parsons Project"}],
                    "album": {"name": "Eye In The Sky", "images": []},
                    "duration_ms": 401000,
                    "external_ids": {"isrc": "GBF077930020"}
                }
            }"#,
        )
        .unwrap();

        assert_eq!(item.item.unwrap().id.as_deref(), Some("spotify-track-1"));
    }

    #[test]
    fn uses_current_spotify_library_and_playlist_items_endpoints() {
        assert_eq!(
            playlist_items_url("playlist-1"),
            "https://api.spotify.com/v1/playlists/playlist-1/items"
        );
        assert_eq!(
            spotify_library_url(),
            "https://api.spotify.com/v1/me/library"
        );
    }

    #[test]
    fn identifies_spotify_managed_podcast_playlists() {
        assert!(is_spotify_system_playlist_name("Episodes for Later"));
        assert!(is_spotify_system_playlist_name("New Episodes"));
        assert!(!is_spotify_system_playlist_name("Road Trip"));
    }

    #[test]
    fn retries_rate_limits_and_transient_server_failures() {
        assert!(should_retry_spotify_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(should_retry_spotify_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(!should_retry_spotify_status(StatusCode::BAD_REQUEST));

        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("12"));
        assert_eq!(
            spotify_retry_delay(
                StatusCode::TOO_MANY_REQUESTS,
                &headers,
                0,
                SPOTIFY_WRITE_RETRY_POLICY
            ),
            Duration::from_secs(14)
        );
        assert_eq!(
            spotify_retry_delay(
                StatusCode::TOO_MANY_REQUESTS,
                &HeaderMap::new(),
                0,
                SPOTIFY_WRITE_RETRY_POLICY
            ),
            Duration::from_secs(75)
        );
        assert_eq!(
            spotify_retry_delay(
                StatusCode::TOO_MANY_REQUESTS,
                &HeaderMap::new(),
                0,
                SPOTIFY_READ_RETRY_POLICY
            ),
            Duration::from_secs(30)
        );
        assert_eq!(
            spotify_retry_delay(
                StatusCode::SERVICE_UNAVAILABLE,
                &HeaderMap::new(),
                2,
                SPOTIFY_WRITE_RETRY_POLICY
            ),
            Duration::from_secs(4)
        );
    }

    #[test]
    fn defers_long_spotify_retry_after_to_operation_cooldown() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, HeaderValue::from_static("58846"));

        assert!(should_defer_spotify_retry(
            StatusCode::TOO_MANY_REQUESTS,
            &headers,
            SPOTIFY_READ_RETRY_POLICY
        ));
        assert!(should_defer_spotify_retry(
            StatusCode::TOO_MANY_REQUESTS,
            &headers,
            SPOTIFY_WRITE_RETRY_POLICY
        ));

        headers.insert(RETRY_AFTER, HeaderValue::from_static("60"));
        assert!(!should_defer_spotify_retry(
            StatusCode::TOO_MANY_REQUESTS,
            &headers,
            SPOTIFY_READ_RETRY_POLICY
        ));
    }

    #[test]
    fn extracts_song_artist_from_youtube_style_video_title() {
        let title =
            "Cyberpunk: Edgerunners -- Ending Theme | Let You Down by Dawid Podsiadlo | Netflix";
        assert_eq!(
            song_artist_from_video_title(title),
            Some(("Let You Down".to_string(), "Dawid Podsiadlo".to_string()))
        );

        let metadata = TrackMetadata {
            title: title.to_string(),
            artists: vec!["Cyberpunk 2077".to_string()],
            album: None,
            duration_seconds: Some(284),
            isrc: None,
        };
        let queries = spotify_search_queries(&metadata);
        assert!(queries
            .iter()
            .any(|query| query == "track:Let You Down artist:Dawid Podsiadlo"));
    }

    #[test]
    fn alternate_match_profiles_resolve_video_metadata_candidates() {
        let metadata = TrackMetadata {
            title:
                "Cyberpunk: Edgerunners -- Ending Theme | Let You Down by Dawid Podsiadlo | Netflix"
                    .to_string(),
            artists: vec!["Cyberpunk 2077".to_string()],
            album: None,
            duration_seconds: Some(284),
            isrc: None,
        };
        let profiles = spotify_match_profiles(&metadata);
        let candidates = vec![MatchCandidate {
            id: "spotify-let-you-down".to_string(),
            title: "Let You Down".to_string(),
            artists: vec!["Dawid Podsiadlo".to_string()],
            album: Some("Let You Down".to_string()),
            duration_seconds: Some(284),
            source_weight: 0.05,
        }];

        assert_eq!(
            best_candidate_for_profiles(&profiles, &candidates).map(|candidate| candidate.id),
            Some("spotify-let-you-down".to_string())
        );
    }

    #[test]
    fn parses_decimal_retry_after_header() {
        assert_eq!(parse_retry_after_seconds("1.2"), Some(2));
        assert_eq!(parse_retry_after_seconds(" 12 "), Some(12));
        assert_eq!(parse_retry_after_seconds("not-a-duration"), None);
    }

    #[test]
    fn write_scope_covers_every_scope_the_push_path_uses() {
        // The scopes the push path actually exercises: it reads the current
        // library and playlists (user-library-read, playlist-read-private) and
        // modifies both (user-library-modify, playlist-modify-*).
        let push_required = [
            "user-library-read",
            "user-library-modify",
            "playlist-read-private",
            "playlist-modify-public",
            "playlist-modify-private",
        ];
        let write_scopes: std::collections::HashSet<&str> =
            SCOPE_WRITE.split_whitespace().collect();

        // push-required scopes ⊆ Write scope set
        for scope in push_required {
            assert!(
                write_scopes.contains(scope),
                "SCOPE_WRITE is missing push scope {scope}"
            );
        }

        // The validation table for Write must also require what the push uses,
        // and a token granted SCOPE_WRITE must satisfy it.
        let table: std::collections::HashSet<&str> = required_scopes(ProviderCapability::Write)
            .iter()
            .copied()
            .collect();
        assert_eq!(table, write_scopes);
        assert!(token_has_required_scopes(
            Some(SCOPE_WRITE),
            ProviderCapability::Write
        ));

        // SCOPE_ALL stays consistent (a superset that also satisfies Write).
        let all_scopes: std::collections::HashSet<&str> = SCOPE_ALL.split_whitespace().collect();
        assert!(write_scopes.is_subset(&all_scopes));
        assert!(token_has_required_scopes(
            Some(SCOPE_ALL),
            ProviderCapability::Write
        ));
    }

    #[test]
    fn token_staleness_respects_expiry_and_safety_margin() {
        // Unknown lifetime is never treated as stale (can't reason about it).
        assert!(!token_is_stale(None));
        // An expiry already in the past is stale.
        assert!(token_is_stale(Some(
            Instant::now() - Duration::from_secs(1)
        )));
        // A comfortably future expiry is fresh.
        assert!(!token_is_stale(Some(
            Instant::now() + Duration::from_secs(120)
        )));

        // A long-lived token is fresh once the safety margin is applied.
        assert!(!token_is_stale(expires_at_from(Some(3600))));
        // A token whose whole lifetime is within the safety margin is stale now.
        assert!(token_is_stale(expires_at_from(Some(
            TOKEN_EXPIRY_SAFETY_MARGIN_SECS
        ))));
        // No advertised lifetime => no computed expiry.
        assert!(expires_at_from(None).is_none());
    }

    #[test]
    fn skips_playlist_replace_only_when_all_entries_failed_to_resolve() {
        // All entries failed to resolve on a non-empty playlist => skip.
        assert!(should_skip_empty_playlist_replace(0, 5));
        // Some resolved => proceed (partial push is allowed).
        assert!(!should_skip_empty_playlist_replace(3, 5));
        // Genuinely empty canonical playlist => proceed (clearing is intended).
        assert!(!should_skip_empty_playlist_replace(0, 0));
        // Defensive: resolved without canonical entries => proceed.
        assert!(!should_skip_empty_playlist_replace(2, 0));
    }

    #[test]
    fn circuit_breaker_trips_after_consecutive_failures_and_resets_on_success() {
        let mut breaker = PushCircuitBreaker::new(PUSH_CONSECUTIVE_FAILURE_LIMIT);

        // The first (limit - 1) failures do not trip.
        for _ in 0..PUSH_CONSECUTIVE_FAILURE_LIMIT - 1 {
            assert!(!breaker.record_failure());
        }
        // A success resets the streak, so we can fail again without tripping.
        breaker.record_success();
        for _ in 0..PUSH_CONSECUTIVE_FAILURE_LIMIT - 1 {
            assert!(!breaker.record_failure());
        }
        // The limit-th consecutive failure trips the breaker.
        assert!(breaker.record_failure());
    }

    #[test]
    fn only_rate_limit_and_auth_failures_abort_the_push() {
        let rate_limited = anyhow::Error::new(ProviderError::rate_limited(
            "429",
            Some(Duration::from_secs(30)),
        ));
        let auth = anyhow::Error::new(ProviderError::auth_failed("401"));
        let http = anyhow::Error::new(ProviderError::http("500", 500));
        let invalid = anyhow::Error::new(ProviderError::invalid_argument("400"));
        let network = anyhow::Error::new(ProviderError::network("connect reset"));
        let plain = anyhow::anyhow!("some non-provider error");

        assert!(is_fatal_push_error(&rate_limited));
        assert!(is_fatal_push_error(&auth));
        assert!(!is_fatal_push_error(&http));
        assert!(!is_fatal_push_error(&invalid));
        assert!(!is_fatal_push_error(&network));
        assert!(!is_fatal_push_error(&plain));
    }
}
