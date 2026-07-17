use std::collections::{HashSet, VecDeque};
use std::env;
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use rand::Rng;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde_json::{json, Value};
use tokio::time::sleep;
use ytmusicapi::{BrowserAuth, Privacy, YTMusicClient};

use crate::domain::{
    LibraryState, LinkSource, ObservedArtwork, ObservedPlaylist, ObservedPlaylistTrack,
    ObservedSavedTrack, ObservedTrack, PlaylistSyncTarget, ProviderKind, ProviderLibrarySnapshot,
    PurgeReport, SyncStatusRecord, SyncSummary, TrackMetadata, YoutubeMusicConnectionConfig,
};
use crate::error::{provider_failure, ProviderError, ProviderFailure};
use crate::matching::{best_candidate, cleaned_title, MatchCandidate};
use crate::provider::{ProgressHandler, ProviderProgress, StreamingProvider};

const YTM_DOMAIN: &str = "https://music.youtube.com";
const YTM_BASE_API: &str = "https://music.youtube.com/youtubei/v1/";
const YTM_PARAMS: &str = "?alt=json";
const YTM_PARAMS_KEY: &str = "&key=AIzaSyC9XL3ZjWddXya6X74dJoCTL-WEYFDNX30";
const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:88.0) Gecko/20100101 Firefox/88.0";
const SEARCH_SONGS_PARAMS: &str = "EgWKAQIIAWoMEA4QChADEAQQCRAF";
const SEARCH_VIDEOS_PARAMS: &str = "EgWKAQIQAWoMEA4QChADEAQQCRAF";
const LIBRARY_LANDING_BROWSE_ID: &str = "FEmusic_library_landing";
const LIBRARY_PLAYLISTS_BROWSE_ID: &str = "FEmusic_liked_playlists";
const MAX_LIBRARY_PLAYLIST_PAGES: usize = 1_000;
const UNBOUNDED_TRACK_LIMIT: u32 = u32::MAX;
/// Maximum attempts (including the first) for a retryable `ytmusicapi` call.
const MAX_YTM_RETRY_ATTEMPTS: u32 = 3;
/// Ceiling on any single retry backoff, including an honored `Retry-After`.
const YTM_RETRY_MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Consecutive per-item push failures tolerated before the run is aborted.
const PUSH_FAILURE_CIRCUIT_BREAKER: u32 = 5;
/// Maximum number of skipped-track labels listed in the aggregated export
/// warning before it collapses the rest into an "and N more" tail.
const MAX_DROPPED_TRACK_SAMPLE: usize = 10;
const UNSUPPORTED_LIBRARY_RESET_MESSAGE: &str = "YouTube Music does not support account-wide library reset in this app. Normal pull, push, and targeted canonical deletes are supported.";

#[derive(Clone)]
struct YoutubeMusicPlaylistSummary {
    playlist_id: String,
    title: String,
}

pub struct YoutubeMusicProvider {
    auth: BrowserAuth,
    client: YTMusicClient,
    http: reqwest::Client,
}

#[derive(Clone, Copy)]
enum SearchFilter {
    Songs,
    Videos,
}

impl SearchFilter {
    fn params(self) -> &'static str {
        match self {
            SearchFilter::Songs => SEARCH_SONGS_PARAMS,
            SearchFilter::Videos => SEARCH_VIDEOS_PARAMS,
        }
    }

    fn source_weight(self) -> f64 {
        match self {
            SearchFilter::Songs => 0.15,
            SearchFilter::Videos => 0.0,
        }
    }
}

impl YoutubeMusicProvider {
    pub fn new() -> Result<Self> {
        dotenvy::dotenv().ok();
        let headers_path = env::var("YOUTUBE_MUSIC_HEADERS_PATH")
            .ok()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("ytmusic_headers.json"));
        let auth = BrowserAuth::from_file(&headers_path).with_context(|| {
            format!(
                "Failed to load YouTube Music browser headers from {}",
                headers_path.display()
            )
        })?;
        let client = YTMusicClient::builder()
            .with_browser_auth(auth.clone())
            .build()?;
        let http = build_http_client()?;

        Ok(Self { auth, client, http })
    }

    pub fn from_connection(config: &YoutubeMusicConnectionConfig) -> Result<Self> {
        let auth = BrowserAuth {
            cookie: config.cookie.clone(),
            x_goog_authuser: config.x_goog_authuser.clone(),
            origin: config
                .origin
                .clone()
                .unwrap_or_else(|| "https://music.youtube.com".to_string()),
        };
        let client = YTMusicClient::builder()
            .with_browser_auth(auth.clone())
            .build()?;
        let http = build_http_client()?;

        Ok(Self { auth, client, http })
    }

    pub async fn verify_connection(&self) -> Result<()> {
        let response = self
            .send_request(
                "browse",
                json!({
                    "browseId": LIBRARY_LANDING_BROWSE_ID,
                }),
            )
            .await?;
        if contains_key_recursive(&response, "signInEndpoint") {
            return Err(anyhow::Error::new(ProviderError::auth_failed(
                "YouTube Music browser headers are expired or incomplete. Capture fresh signed-in headers from a music.youtube.com browse request and relink the account.",
            )));
        }
        Ok(())
    }

    async fn resolve_track(
        &self,
        metadata: &TrackMetadata,
        existing_provider_id: Option<&str>,
    ) -> Result<Option<(String, f64)>> {
        if let Some(provider_id) = existing_provider_id {
            return Ok(Some((provider_id.to_string(), 1.0)));
        }

        let candidates = self.search_candidates(metadata).await?;
        Ok(best_candidate(metadata, &candidates).map(|candidate| (candidate.id, candidate.score)))
    }

    async fn search_candidates(&self, metadata: &TrackMetadata) -> Result<Vec<MatchCandidate>> {
        let primary_artist = metadata.artists.first().map(String::as_str).unwrap_or("");
        let title = cleaned_title(&metadata.title);
        let query = format!("{primary_artist} {title}").trim().to_string();

        let mut candidates = self.search(query.as_str(), SearchFilter::Songs).await?;
        if candidates.is_empty() {
            candidates = self.search(query.as_str(), SearchFilter::Videos).await?;
        }

        Ok(candidates)
    }

    async fn search(&self, query: &str, filter: SearchFilter) -> Result<Vec<MatchCandidate>> {
        let response = self
            .send_request(
                "search",
                json!({
                    "query": query,
                    "params": filter.params(),
                }),
            )
            .await?;

        let sections = response
            .get("contents")
            .and_then(|value| value.get("tabbedSearchResultsRenderer"))
            .and_then(|value| value.get("tabs"))
            .and_then(Value::as_array)
            .and_then(|tabs| tabs.first())
            .and_then(|tab| tab.get("tabRenderer"))
            .and_then(|tab| tab.get("content"))
            .and_then(|content| content.get("sectionListRenderer"))
            .and_then(|section_list| section_list.get("contents"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut candidates = Vec::new();
        let mut seen_ids = HashSet::new();

        for section in sections {
            let Some(items) = section
                .get("musicShelfRenderer")
                .and_then(|shelf| shelf.get("contents"))
                .and_then(Value::as_array)
            else {
                continue;
            };

            for item in items {
                let Some(renderer) = item.get("musicResponsiveListItemRenderer") else {
                    continue;
                };

                let Some(video_id) = navigate(
                    renderer,
                    &[
                        "overlay",
                        "musicItemThumbnailOverlayRenderer",
                        "content",
                        "musicPlayButtonRenderer",
                        "playNavigationEndpoint",
                        "watchEndpoint",
                        "videoId",
                    ],
                )
                .and_then(Value::as_str)
                .or_else(|| {
                    navigate(renderer, &["playlistItemData", "videoId"]).and_then(Value::as_str)
                }) else {
                    continue;
                };

                if !seen_ids.insert(video_id.to_string()) {
                    continue;
                }

                let title = flex_run_text(renderer, 0, 0).unwrap_or_default();
                let runs = flex_runs(renderer, 1).unwrap_or_default();
                let (artists, album, duration_seconds) = parse_song_runs(runs);

                candidates.push(MatchCandidate {
                    id: video_id.to_string(),
                    title,
                    artists,
                    album,
                    duration_seconds,
                    source_weight: filter.source_weight(),
                });
            }
        }

        Ok(candidates)
    }

    async fn send_request(&self, endpoint: &str, mut body: Value) -> Result<Value> {
        merge_context(&mut body);
        let url = format!(
            "{}{}{}{}",
            YTM_BASE_API, endpoint, YTM_PARAMS, YTM_PARAMS_KEY
        );
        let authorization = self.auth.get_authorization()?;
        let cookie = format!("{}; SOCS=CAI", self.auth.cookie);

        for attempt in 0..5 {
            let response = self
                .http
                .post(&url)
                .header("authorization", &authorization)
                .header("cookie", &cookie)
                .header("x-goog-authuser", &self.auth.x_goog_authuser)
                .header("x-origin", &self.auth.origin)
                .json(&body)
                .send()
                .await;

            match response {
                Ok(response) if response.status().is_success() => {
                    let response_json: Value = response.json().await?;
                    if let Some(error) = response_json.get("error") {
                        let code = error
                            .get("code")
                            .and_then(Value::as_u64)
                            .unwrap_or_default() as u16;
                        if attempt < 4 && is_retryable_status(code) {
                            sleep(Duration::from_secs(1 << attempt)).await;
                            continue;
                        }
                        return Err(ytmusic_status_error(
                            "YouTube Music API returned an error",
                            code,
                            None,
                            &error.to_string(),
                        ));
                    }

                    return Ok(response_json);
                }
                Ok(response) => {
                    let status = response.status().as_u16();
                    let retry_after = response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok())
                        .and_then(parse_ytmusic_retry_after);
                    let response_body = response
                        .text()
                        .await
                        .unwrap_or_else(|_| "Could not read response body".to_string());
                    if attempt < 4 && is_retryable_status(status) {
                        sleep(Duration::from_secs(1 << attempt)).await;
                        continue;
                    }
                    return Err(ytmusic_status_error(
                        "YouTube Music request failed",
                        status,
                        retry_after,
                        &response_body,
                    ));
                }
                Err(error) => {
                    if attempt < 4 {
                        sleep(Duration::from_secs(1 << attempt)).await;
                        continue;
                    }
                    return Err(ytmusic_transport_error(error));
                }
            }
        }

        anyhow::bail!("YouTube Music request failed after multiple retries")
    }

    async fn find_playlist_by_name(&self, name: &str) -> Result<Option<String>> {
        let playlists = self.list_library_playlists().await?;
        Ok(playlists
            .into_iter()
            .find(|playlist| playlist.title.eq_ignore_ascii_case(name))
            .map(|playlist| playlist.playlist_id))
    }

    async fn list_library_playlists(&self) -> Result<Vec<YoutubeMusicPlaylistSummary>> {
        let response = self
            .send_request(
                "browse",
                json!({
                    "browseId": LIBRARY_PLAYLISTS_BROWSE_ID,
                }),
            )
            .await?;
        let mut playlists = Vec::new();
        let mut seen_playlist_ids = HashSet::new();
        let mut queued_tokens = VecDeque::new();
        let mut seen_tokens = HashSet::new();
        collect_library_playlists(&response, &mut playlists, &mut seen_playlist_ids);
        collect_continuation_tokens(&response, &mut queued_tokens);

        while let Some(token) = queued_tokens.pop_front() {
            if !seen_tokens.insert(token.clone()) {
                continue;
            }
            if seen_tokens.len() > MAX_LIBRARY_PLAYLIST_PAGES {
                anyhow::bail!(
                    "YouTube Music returned more than {MAX_LIBRARY_PLAYLIST_PAGES} playlist pages; refusing to continue an unbounded import."
                );
            }
            let response = self
                .send_request(
                    "browse",
                    json!({
                        "continuation": token,
                    }),
                )
                .await?;
            collect_library_playlists(&response, &mut playlists, &mut seen_playlist_ids);
            collect_continuation_tokens(&response, &mut queued_tokens);
        }

        Ok(playlists)
    }

    /// Creates a fresh private playlist and populates it with `video_ids` in
    /// 100-item batches. Both the create and each add are retried on transient
    /// failures. If any batch fails, the partially built playlist is deleted
    /// (best effort) so a later retry does not accumulate orphans, and the
    /// classified error is returned to the caller.
    async fn create_and_populate_playlist(
        &self,
        name: &str,
        description: Option<&str>,
        video_ids: &[String],
    ) -> Result<String> {
        let created_id = retry_ytm_operation(move || async move {
            self.client
                .create_playlist(name, description, Privacy::Private)
                .await
                .map_err(ytmusic_client_error)
        })
        .await?
        .playlist_id;

        for chunk in video_ids.chunks(100) {
            let created_ref = created_id.as_str();
            if let Err(error) = retry_ytm_operation(move || async move {
                self.client
                    .add_playlist_items(created_ref, chunk, false)
                    .await
                    .map_err(ytmusic_client_error)
            })
            .await
            {
                let _ = self.client.delete_playlist(&created_id).await;
                return Err(error);
            }
        }

        Ok(created_id)
    }

    /// Performs the provider-side work of replacing a playlist: resolves the
    /// existing playlist to replace (by stored link or name), then creates and
    /// populates the replacement. Returns the new playlist ID plus the old
    /// playlist ID to delete afterwards, or a classified error on failure.
    async fn push_playlist_replacement(
        &self,
        playlist: &PlaylistSyncTarget,
        resolved_video_ids: &[String],
    ) -> Result<(String, Option<String>)> {
        let playlist_to_replace = if let Some(provider_id) = &playlist.existing_provider_id {
            Some(provider_id.clone())
        } else {
            self.find_playlist_by_name(&playlist.name).await?
        };

        let created_id = self
            .create_and_populate_playlist(
                &playlist.name,
                playlist.description.as_deref(),
                resolved_video_ids,
            )
            .await?;

        Ok((created_id, playlist_to_replace))
    }
}

fn emit_progress(progress: Option<&ProgressHandler>, update: ProviderProgress) {
    if let Some(callback) = progress {
        callback(update);
    }
}

fn youtube_music_cleanup_warning(
    playlist_name: &str,
    old_playlist_id: &str,
    new_playlist_id: &str,
    error: &str,
) -> String {
    format!(
        "Synced YouTube Music playlist '{playlist_name}' to replacement playlist {new_playlist_id}, but failed to delete old playlist {old_playlist_id}: {error}"
    )
}

/// Runs a `ytmusicapi` operation with bounded retry.
///
/// The operation closure must classify its failure via [`ytmusic_client_error`]
/// so the returned `anyhow::Error` carries a typed [`ProviderError`]. Only typed
/// [`ProviderFailure::RateLimited`] (honoring a capped `Retry-After`) and
/// transient [`ProviderFailure::Network`] failures are retried, up to
/// [`MAX_YTM_RETRY_ATTEMPTS`] attempts with exponential backoff plus jitter.
/// Auth, invalid-argument, blocked, and unclassified failures return at once.
async fn retry_ytm_operation<T, F, Fut>(operation: F) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut attempt = 1u32;
    loop {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                let Some(base_delay) = ytm_retry_delay(&error, attempt) else {
                    return Err(error);
                };
                if attempt >= MAX_YTM_RETRY_ATTEMPTS {
                    return Err(error);
                }
                sleep(apply_jitter(base_delay)).await;
                attempt += 1;
            }
        }
    }
}

/// The base backoff a retryable YouTube Music failure calls for on `attempt`
/// (1-based), or `None` when the failure must not be retried.
///
/// Rate limits honor a provider-supplied `retry_after` (capped to
/// [`YTM_RETRY_MAX_BACKOFF`]); rate limits without one, and transient network
/// failures, fall back to exponential backoff. Auth, invalid-argument, blocked,
/// and unclassified failures are never retried.
fn ytm_retry_delay(error: &anyhow::Error, attempt: u32) -> Option<Duration> {
    match provider_failure(error).map(ProviderError::failure) {
        Some(ProviderFailure::RateLimited { retry_after }) => Some(
            retry_after
                .unwrap_or_else(|| ytm_backoff(attempt))
                .min(YTM_RETRY_MAX_BACKOFF),
        ),
        Some(ProviderFailure::Network) => Some(ytm_backoff(attempt)),
        _ => None,
    }
}

/// Exponential backoff (base 2 seconds) for `attempt` (1-based), capped at
/// [`YTM_RETRY_MAX_BACKOFF`]: 1s, 2s, 4s, ...
fn ytm_backoff(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(5);
    Duration::from_secs(1u64 << exponent).min(YTM_RETRY_MAX_BACKOFF)
}

/// Adds up to +50% random jitter to a backoff delay to desynchronize retries,
/// re-capped at [`YTM_RETRY_MAX_BACKOFF`].
fn apply_jitter(delay: Duration) -> Duration {
    let factor = 1.0 + rand::thread_rng().gen_range(0.0..=0.5);
    delay.mul_f64(factor).min(YTM_RETRY_MAX_BACKOFF)
}

/// Whether a typed failure must abort a batch operation (push or export)
/// immediately: rate limits and auth failures will not recover by continuing.
fn is_unrecoverable_failure(error: &anyhow::Error) -> bool {
    matches!(
        provider_failure(error).map(ProviderError::failure),
        Some(ProviderFailure::RateLimited { .. } | ProviderFailure::AuthFailed)
    )
}

/// Whether a push run must abort after an item failed. Unrecoverable failures
/// abort immediately; otherwise the run tolerates transient item failures until
/// `consecutive_failures` reaches [`PUSH_FAILURE_CIRCUIT_BREAKER`].
fn push_should_abort(error: &anyhow::Error, consecutive_failures: u32) -> bool {
    is_unrecoverable_failure(error) || consecutive_failures >= PUSH_FAILURE_CIRCUIT_BREAKER
}

/// Annotates the error that aborts a push. Circuit-breaker aborts gain context
/// naming the threshold; unrecoverable (rate-limit/auth) aborts are returned
/// as-is so their typed classification stays recoverable for downstream policy.
fn abort_push_error(error: anyhow::Error, consecutive_failures: u32) -> anyhow::Error {
    if is_unrecoverable_failure(&error) {
        error
    } else {
        error.context(format!(
            "Aborted YouTube Music push after {consecutive_failures} consecutive item failures"
        ))
    }
}

/// Accumulates tracks skipped during export because they carry no playable
/// `videoId`, keeping a bounded sample of human labels alongside the full count.
#[derive(Default)]
struct DroppedTrackTally {
    total: usize,
    sample: Vec<String>,
}

impl DroppedTrackTally {
    fn record(&mut self, track: &ytmusicapi::PlaylistTrack) {
        self.total += 1;
        if self.sample.len() < MAX_DROPPED_TRACK_SAMPLE {
            self.sample.push(describe_dropped_track(track));
        }
    }

    /// One aggregated warning for all drops, or `None` when nothing was dropped.
    fn into_warning(self) -> Option<String> {
        if self.total == 0 {
            None
        } else {
            Some(dropped_tracks_warning(self.total, &self.sample))
        }
    }
}

/// A human label ("Artist - Title") for a dropped track, for the export warning.
fn describe_dropped_track(track: &ytmusicapi::PlaylistTrack) -> String {
    let title = track
        .title
        .clone()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| "Unknown title".to_string());
    let artists = track
        .artists
        .iter()
        .map(|artist| artist.name.trim())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    if artists.is_empty() {
        title
    } else {
        format!("{} - {}", artists.join(", "), title)
    }
}

/// Builds one aggregated warning for tracks skipped during export because they
/// had no playable `videoId` (podcasts, region-locked items, or personal
/// uploads). `sample` holds up to [`MAX_DROPPED_TRACK_SAMPLE`] labels already
/// collected; `total` is the full count across every drop site, with the
/// remainder collapsed into an "and N more" tail.
fn dropped_tracks_warning(total: usize, sample: &[String]) -> String {
    let noun = if total == 1 { "track" } else { "tracks" };
    let mut listed = sample.join(", ");
    let remainder = total.saturating_sub(sample.len());
    if remainder > 0 {
        if !listed.is_empty() {
            listed.push_str(", ");
        }
        listed.push_str(&format!("and {remainder} more"));
    }
    format!(
        "Skipped {total} YouTube Music {noun} with no playable videoId (podcast, region-locked, or personal upload); not exported: {listed}"
    )
}

#[async_trait]
impl StreamingProvider for YoutubeMusicProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::YoutubeMusic
    }

    async fn verify_connection(&self) -> Result<()> {
        YoutubeMusicProvider::verify_connection(self).await
    }

    async fn export_library_with_progress(
        &self,
        progress: Option<ProgressHandler>,
    ) -> Result<ProviderLibrarySnapshot> {
        self.verify_connection().await?;
        emit_progress(
            progress.as_ref(),
            ProviderProgress {
                stage: "Fetching saved tracks".to_string(),
                detail: Some("YouTube Music".to_string()),
                ..Default::default()
            },
        );
        let captured_at = Utc::now();
        let liked = retry_ytm_operation(|| async {
            self.client
                .get_liked_songs(Some(UNBOUNDED_TRACK_LIMIT))
                .await
                .map_err(ytmusic_client_error)
        })
        .await?;
        emit_progress(
            progress.as_ref(),
            ProviderProgress {
                stage: "Fetching playlists".to_string(),
                saved_tracks_total: Some(liked.tracks.len()),
                ..Default::default()
            },
        );
        let playlists = self.list_library_playlists().await?;

        let mut snapshot = ProviderLibrarySnapshot {
            provider: ProviderKind::YoutubeMusic,
            captured_at,
            saved_tracks: Vec::new(),
            playlists: Vec::new(),
            warnings: Vec::new(),
        };
        // Tracks with no playable `videoId` (podcasts, region-locked items, or
        // personal uploads) cannot be represented; tally them across every drop
        // site and surface one aggregated warning at the end of the export.
        let mut dropped = DroppedTrackTally::default();

        let saved_total = liked.tracks.len();
        for (index, track) in liked.tracks.into_iter().enumerate() {
            match playlist_track_to_observed(&track) {
                Some(observed_track) => snapshot.saved_tracks.push(ObservedSavedTrack {
                    added_at: None,
                    track: observed_track,
                }),
                None => dropped.record(&track),
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

        let playlists_total = playlists
            .iter()
            .filter(|playlist| playlist.playlist_id != "LM")
            .count();
        let mut playlist_entries_done = 0usize;
        let mut playlists_done = 0usize;
        for playlist in playlists {
            if playlist.playlist_id == "LM" {
                continue;
            }

            emit_progress(
                progress.as_ref(),
                ProviderProgress {
                    stage: "Fetching playlist".to_string(),
                    detail: Some(playlist.title.clone()),
                    saved_tracks_done: saved_total,
                    saved_tracks_total: Some(saved_total),
                    playlists_done,
                    playlists_total: Some(playlists_total),
                    ..Default::default()
                },
            );
            let playlist_id = playlist.playlist_id.as_str();
            let full_playlist = match retry_ytm_operation(|| async {
                self.client
                    .get_playlist(playlist_id, Some(UNBOUNDED_TRACK_LIMIT))
                    .await
                    .map_err(ytmusic_client_error)
            })
            .await
            {
                Ok(full_playlist) => full_playlist,
                // A single failing playlist fetch must not sink the whole
                // export: record a warning and move on. Only auth/rate-limit
                // failures (which will not recover mid-run) abort.
                Err(error) if is_unrecoverable_failure(&error) => return Err(error),
                Err(error) => {
                    snapshot.warnings.push(format!(
                        "Skipped YouTube Music playlist '{}' ({}): {error}",
                        playlist.title, playlist.playlist_id
                    ));
                    playlists_done += 1;
                    continue;
                }
            };
            let item_count = full_playlist.tracks.len();
            let mut observed_playlist = ObservedPlaylist {
                provider_id: Some(full_playlist.id),
                name: full_playlist.title,
                description: full_playlist.description,
                tracks: Vec::new(),
            };

            for track in full_playlist.tracks {
                match playlist_track_to_observed(&track) {
                    Some(observed_track) => observed_playlist.tracks.push(ObservedPlaylistTrack {
                        added_at: None,
                        track: observed_track,
                    }),
                    None => dropped.record(&track),
                }
                playlist_entries_done += 1;
            }

            snapshot.playlists.push(observed_playlist);
            playlists_done += 1;
            emit_progress(
                progress.as_ref(),
                ProviderProgress {
                    stage: "Pulling playlists".to_string(),
                    detail: Some(format!("{} tracks", item_count)),
                    saved_tracks_done: saved_total,
                    saved_tracks_total: Some(saved_total),
                    playlists_done,
                    playlists_total: Some(playlists_total),
                    playlist_entries_done,
                    ..Default::default()
                },
            );
        }

        if let Some(warning) = dropped.into_warning() {
            snapshot.warnings.push(warning);
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
        self.verify_connection().await?;
        let now = Utc::now();
        let saved_targets = state.saved_track_targets(ProviderKind::YoutubeMusic)?;
        let playlist_targets = state.playlist_targets(ProviderKind::YoutubeMusic)?;
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

        // Consecutive per-item mutation failures across the whole push run;
        // reset on any success and used to trip the circuit breaker.
        let mut consecutive_failures = 0u32;
        let mut saved_done = 0usize;
        for target in saved_targets {
            match target.existing_provider_id.clone() {
                Some(provider_id) => {
                    let confidence = 1.0;
                    // In apply mode, attempt the provider mutation FIRST and
                    // only commit the canonical link + synced status once it
                    // succeeds, so failed items never leave state claiming a
                    // success that did not happen. Dry runs have no mutation
                    // and commit the resolution directly.
                    let committed = if force {
                        let provider_id_ref = provider_id.as_str();
                        match retry_ytm_operation(move || async move {
                            self.client
                                .like_song(provider_id_ref)
                                .await
                                .map_err(ytmusic_client_error)
                        })
                        .await
                        {
                            Ok(_) => {
                                consecutive_failures = 0;
                                true
                            }
                            Err(error) => {
                                let message = format!(
                                    "Failed to like '{}' on YouTube Music: {error}",
                                    target.metadata.display_label()
                                );
                                state.set_track_status(
                                    &target.track_id,
                                    ProviderKind::YoutubeMusic,
                                    SyncStatusRecord::error(message.clone(), now),
                                );
                                state.set_saved_track_status(
                                    &target.saved_track_id,
                                    ProviderKind::YoutubeMusic,
                                    SyncStatusRecord::error(message, now),
                                );
                                consecutive_failures += 1;
                                if push_should_abort(&error, consecutive_failures) {
                                    return Err(abort_push_error(error, consecutive_failures));
                                }
                                false
                            }
                        }
                    } else {
                        true
                    };

                    if committed {
                        state.upsert_track_link(
                            &target.track_id,
                            ProviderKind::YoutubeMusic,
                            provider_id.clone(),
                            LinkSource::Match,
                            Some(confidence),
                            now,
                        );
                        state.set_track_status(
                            &target.track_id,
                            ProviderKind::YoutubeMusic,
                            SyncStatusRecord::synced(
                                Some(provider_id.clone()),
                                Some(confidence),
                                Some("Resolved on YouTube Music".to_string()),
                                now,
                            ),
                        );
                        summary.saved_tracks_synced += 1;
                        state.set_saved_track_status(
                            &target.saved_track_id,
                            ProviderKind::YoutubeMusic,
                            SyncStatusRecord::synced(
                                Some(provider_id),
                                Some(confidence),
                                Some(if force {
                                    "Synced to YouTube Music likes".to_string()
                                } else {
                                    "Resolved for YouTube Music sync dry run".to_string()
                                }),
                                now,
                            ),
                        );
                    }
                }
                None => {
                    summary.saved_tracks_unmatched += 1;
                    let reason = format!(
                        "No YouTube Music identity for {}. Run library identity sync before pushing.",
                        target.metadata.display_label()
                    );
                    state.set_track_status(
                        &target.track_id,
                        ProviderKind::YoutubeMusic,
                        SyncStatusRecord::unmatched(reason.clone(), now),
                    );
                    state.set_saved_track_status(
                        &target.saved_track_id,
                        ProviderKind::YoutubeMusic,
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

        let mut playlist_entries_done = 0usize;
        for (playlist_index, playlist) in playlist_targets.into_iter().enumerate() {
            let mut resolved_video_ids = Vec::new();
            let mut matched_entries = Vec::new();

            for entry in &playlist.entries {
                match entry.existing_provider_id.clone() {
                    // Only classify here; the canonical link + synced status are
                    // committed after the playlist push succeeds (or immediately
                    // for a dry run), never before the provider mutation.
                    Some(provider_id) => {
                        let confidence = 1.0;
                        resolved_video_ids.push(provider_id.clone());
                        matched_entries.push((
                            entry.entry_id.clone(),
                            entry.track_id.clone(),
                            provider_id,
                            confidence,
                        ));
                    }
                    None => {
                        summary.playlist_entries_unmatched += 1;
                        let reason = format!(
                            "No YouTube Music identity for {}. Run library identity sync before pushing.",
                            entry.metadata.display_label()
                        );
                        state.set_track_status(
                            &entry.track_id,
                            ProviderKind::YoutubeMusic,
                            SyncStatusRecord::unmatched(reason.clone(), now),
                        );
                        state.set_playlist_entry_status(
                            &playlist.playlist_id,
                            &entry.entry_id,
                            ProviderKind::YoutubeMusic,
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

            // `Some(message)` means the resolved entries should be committed as
            // synced with `message`; `None` means the playlist push failed and
            // its entries were already recorded as errors, so nothing is
            // committed. The provider mutation always precedes any commit.
            let synced_entry_message: Option<&str> = if force {
                match self
                    .push_playlist_replacement(&playlist, &resolved_video_ids)
                    .await
                {
                    Ok((created_id, playlist_to_replace)) => {
                        consecutive_failures = 0;
                        state.upsert_playlist_link(
                            &playlist.playlist_id,
                            ProviderKind::YoutubeMusic,
                            created_id.clone(),
                            LinkSource::Create,
                            Some(1.0),
                            now,
                        );
                        if let Some(existing_playlist_id) = playlist_to_replace {
                            if existing_playlist_id != created_id {
                                if let Err(error) =
                                    self.client.delete_playlist(&existing_playlist_id).await
                                {
                                    summary.warnings.push(youtube_music_cleanup_warning(
                                        &playlist.name,
                                        &existing_playlist_id,
                                        &created_id,
                                        &error.to_string(),
                                    ));
                                }
                            }
                        }
                        state.set_playlist_status(
                            &playlist.playlist_id,
                            ProviderKind::YoutubeMusic,
                            SyncStatusRecord::synced(
                                Some(created_id),
                                Some(1.0),
                                Some("Synced to YouTube Music".to_string()),
                                now,
                            ),
                        );
                        Some("Synced to YouTube Music")
                    }
                    Err(error) => {
                        // Record the failure on the playlist and every resolved
                        // entry, then continue with the next playlist unless the
                        // failure is fatal or the circuit breaker trips.
                        let message = format!(
                            "Failed to sync YouTube Music playlist '{}': {error}",
                            playlist.name
                        );
                        state.set_playlist_status(
                            &playlist.playlist_id,
                            ProviderKind::YoutubeMusic,
                            SyncStatusRecord::error(message.clone(), now),
                        );
                        for (entry_id, _track_id, _provider_id, _confidence) in &matched_entries {
                            state.set_playlist_entry_status(
                                &playlist.playlist_id,
                                entry_id,
                                ProviderKind::YoutubeMusic,
                                SyncStatusRecord::error(message.clone(), now),
                            );
                        }
                        consecutive_failures += 1;
                        if push_should_abort(&error, consecutive_failures) {
                            return Err(abort_push_error(error, consecutive_failures));
                        }
                        None
                    }
                }
            } else {
                state.set_playlist_status(
                    &playlist.playlist_id,
                    ProviderKind::YoutubeMusic,
                    SyncStatusRecord::synced(
                        playlist.existing_provider_id.clone(),
                        Some(1.0),
                        Some("Resolved for YouTube Music sync dry run".to_string()),
                        now,
                    ),
                );
                Some("Resolved for YouTube Music sync dry run")
            };

            if let Some(entry_message) = synced_entry_message {
                for (entry_id, track_id, provider_id, confidence) in matched_entries {
                    state.upsert_track_link(
                        &track_id,
                        ProviderKind::YoutubeMusic,
                        provider_id.clone(),
                        LinkSource::Match,
                        Some(confidence),
                        now,
                    );
                    state.set_track_status(
                        &track_id,
                        ProviderKind::YoutubeMusic,
                        SyncStatusRecord::synced(
                            Some(provider_id.clone()),
                            Some(confidence),
                            Some("Resolved on YouTube Music".to_string()),
                            now,
                        ),
                    );
                    summary.playlist_entries_synced += 1;
                    state.set_playlist_entry_status(
                        &playlist.playlist_id,
                        &entry_id,
                        ProviderKind::YoutubeMusic,
                        SyncStatusRecord::synced(
                            Some(provider_id),
                            Some(confidence),
                            Some(entry_message.to_string()),
                            now,
                        ),
                    );
                }
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
        let _ = force;
        anyhow::bail!(UNSUPPORTED_LIBRARY_RESET_MESSAGE)
    }

    async fn remove_saved_track(&self, provider_track_id: &str) -> Result<()> {
        self.verify_connection().await?;
        retry_ytm_operation(|| async {
            self.client
                .unlike_song(provider_track_id)
                .await
                .map_err(ytmusic_client_error)
        })
        .await?;
        Ok(())
    }

    async fn delete_playlist(&self, provider_playlist_id: &str) -> Result<()> {
        self.verify_connection().await?;
        retry_ytm_operation(|| async {
            self.client
                .delete_playlist(provider_playlist_id)
                .await
                .map_err(ytmusic_client_error)
        })
        .await?;
        Ok(())
    }
}

fn build_http_client() -> Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    headers.insert("user-agent", HeaderValue::from_static(USER_AGENT));
    headers.insert("accept", HeaderValue::from_static("*/*"));
    headers.insert("accept-encoding", HeaderValue::from_static("gzip, deflate"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert("origin", HeaderValue::from_static(YTM_DOMAIN));

    Ok(reqwest::Client::builder()
        .default_headers(headers)
        .gzip(true)
        .timeout(Duration::from_secs(30))
        .build()?)
}

fn merge_context(body: &mut Value) {
    let client_version = format!("1.{}.01.00", Utc::now().format("%Y%m%d"));
    let context = json!({
        "context": {
            "client": {
                "clientName": "WEB_REMIX",
                "clientVersion": client_version,
                "hl": "en"
            },
            "user": {}
        }
    });

    if let (Value::Object(body_map), Value::Object(context_map)) = (body, context) {
        for (key, value) in context_map {
            body_map.insert(key, value);
        }
    }
}

fn navigate<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    Some(current)
}

fn contains_key_recursive(value: &Value, expected_key: &str) -> bool {
    match value {
        Value::Object(entries) => {
            entries.contains_key(expected_key)
                || entries
                    .values()
                    .any(|value| contains_key_recursive(value, expected_key))
        }
        Value::Array(values) => values
            .iter()
            .any(|value| contains_key_recursive(value, expected_key)),
        _ => false,
    }
}

fn collect_library_playlists(
    value: &Value,
    playlists: &mut Vec<YoutubeMusicPlaylistSummary>,
    seen_playlist_ids: &mut HashSet<String>,
) {
    match value {
        Value::Object(entries) => {
            if let Some(renderer) = entries.get("musicTwoRowItemRenderer") {
                let title = renderer
                    .get("title")
                    .and_then(|value| value.get("runs"))
                    .and_then(Value::as_array)
                    .and_then(|runs| runs.first())
                    .and_then(|run| run.get("text"))
                    .and_then(Value::as_str);
                let playlist_id = renderer
                    .get("navigationEndpoint")
                    .and_then(|value| value.get("watchEndpoint"))
                    .and_then(|value| value.get("playlistId"))
                    .and_then(Value::as_str)
                    .or_else(|| {
                        renderer
                            .get("navigationEndpoint")
                            .and_then(|value| value.get("browseEndpoint"))
                            .and_then(|value| value.get("browseId"))
                            .and_then(Value::as_str)
                    })
                    .map(|playlist_id| playlist_id.trim_start_matches("VL"));

                if let (Some(title), Some(playlist_id)) = (title, playlist_id) {
                    if !playlist_id.is_empty() && seen_playlist_ids.insert(playlist_id.to_string())
                    {
                        playlists.push(YoutubeMusicPlaylistSummary {
                            playlist_id: playlist_id.to_string(),
                            title: title.to_string(),
                        });
                    }
                }
            }

            for child in entries.values() {
                collect_library_playlists(child, playlists, seen_playlist_ids);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_library_playlists(child, playlists, seen_playlist_ids);
            }
        }
        _ => {}
    }
}

fn collect_continuation_tokens(value: &Value, tokens: &mut VecDeque<String>) {
    match value {
        Value::Object(entries) => {
            if let Some(token) = entries
                .get("continuationItemRenderer")
                .and_then(|value| value.get("continuationEndpoint"))
                .and_then(|value| value.get("continuationCommand"))
                .and_then(|value| value.get("token"))
                .and_then(Value::as_str)
            {
                tokens.push_back(token.to_string());
            }

            for child in entries.values() {
                collect_continuation_tokens(child, tokens);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_continuation_tokens(child, tokens);
            }
        }
        _ => {}
    }
}

fn flex_runs(value: &Value, index: usize) -> Option<&[Value]> {
    value
        .get("flexColumns")?
        .get(index)?
        .get("musicResponsiveListItemFlexColumnRenderer")?
        .get("text")?
        .get("runs")?
        .as_array()
        .map(Vec::as_slice)
}

fn flex_run_text(value: &Value, column_index: usize, run_index: usize) -> Option<String> {
    flex_runs(value, column_index)?
        .get(run_index)?
        .get("text")?
        .as_str()
        .map(ToOwned::to_owned)
}

fn parse_song_runs(runs: &[Value]) -> (Vec<String>, Option<String>, Option<u32>) {
    let mut artists = Vec::new();
    let mut album = None;
    let mut duration = None;

    let start_index = runs
        .first()
        .and_then(|first| first.get("text"))
        .and_then(Value::as_str)
        .filter(|text| text.eq_ignore_ascii_case("song") || text.eq_ignore_ascii_case("video"))
        .map(|_| 2)
        .unwrap_or(0);

    for run in runs.iter().skip(start_index).step_by(2) {
        let Some(text) = run.get("text").and_then(Value::as_str) else {
            continue;
        };

        if looks_like_duration(text) {
            duration = parse_duration(text);
            continue;
        }

        if let Some(browse_id) = run
            .get("navigationEndpoint")
            .and_then(|value| value.get("browseEndpoint"))
            .and_then(|value| value.get("browseId"))
            .and_then(Value::as_str)
        {
            if browse_id.starts_with("MPRE") || browse_id.contains("release_detail") {
                album = Some(text.to_string());
            } else {
                artists.push(text.to_string());
            }
            continue;
        }

        if text.len() == 4 && text.chars().all(|character| character.is_ascii_digit()) {
            continue;
        }

        artists.push(text.to_string());
    }

    (artists, album, duration)
}

fn looks_like_duration(value: &str) -> bool {
    let parts = value.split(':').collect::<Vec<_>>();
    matches!(parts.len(), 2 | 3)
        && parts.iter().all(|part| {
            !part.is_empty() && part.chars().all(|character| character.is_ascii_digit())
        })
}

fn parse_duration(value: &str) -> Option<u32> {
    let parts = value.split(':').collect::<Vec<_>>();
    let mut total = 0u32;
    for (index, part) in parts.iter().rev().enumerate() {
        let value: u32 = part.parse().ok()?;
        let multiplier = match index {
            0 => 1,
            1 => 60,
            2 => 3600,
            _ => return None,
        };
        total += value * multiplier;
    }
    Some(total)
}

fn playlist_track_to_observed(track: &ytmusicapi::PlaylistTrack) -> Option<ObservedTrack> {
    let video_id = track.video_id.clone()?;
    let artwork = track
        .thumbnails
        .iter()
        .max_by_key(|thumbnail| thumbnail.width.unwrap_or(0) * thumbnail.height.unwrap_or(0))
        .map(|thumbnail| ObservedArtwork {
            url: thumbnail.url.clone(),
            width: thumbnail.width,
            height: thumbnail.height,
        });
    Some(ObservedTrack {
        metadata: TrackMetadata {
            title: track.title.clone().unwrap_or_else(|| "Unknown".to_string()),
            artists: track
                .artists
                .iter()
                .map(|artist| artist.name.clone())
                .collect(),
            album: track.album.as_ref().map(|album| album.name.clone()),
            duration_seconds: track.duration_seconds,
            isrc: None,
        },
        provider_id: Some(video_id),
        artwork,
    })
}

fn is_retryable_status(status: u16) -> bool {
    matches!(status, 409 | 429 | 500 | 502 | 503 | 504)
}

/// Builds the typed [`ProviderError`] for a non-success YouTube Music response
/// observed on the crate-internal `reqwest` path, where the numeric status code
/// (and optional `Retry-After`) is in hand.
fn ytmusic_status_error(
    context: &str,
    status: u16,
    retry_after: Option<Duration>,
    body: &str,
) -> anyhow::Error {
    let message = format!("{context} ({status}): {body}");
    let provider_error = if looks_like_bot_block(body) {
        ProviderError::blocked(message)
    } else {
        match status {
            429 => ProviderError::rate_limited(message, retry_after),
            401 | 403 => ProviderError::auth_failed(message),
            400 => ProviderError::invalid_argument(message),
            other => ProviderError::http(message, other),
        }
    };
    anyhow::Error::new(provider_error)
}

/// Classifies a transport-level `reqwest` error from the YouTube Music
/// `reqwest` path, mapping connect/timeout failures to
/// [`ProviderFailure::Network`](crate::error::ProviderFailure).
fn ytmusic_transport_error(error: reqwest::Error) -> anyhow::Error {
    if error.is_connect() || error.is_timeout() {
        anyhow::Error::new(ProviderError::network(format!(
            "YouTube Music request transport failure: {error}"
        )))
    } else {
        error.into()
    }
}

/// Wraps an error returned by the `ytmusicapi` crate, attaching a typed
/// [`ProviderError`] when the string form is recognizable.
///
/// The `ytmusicapi` crate surfaces failures as `Display` strings rather than
/// structured statuses, so this is the sole surviving message-sniffing
/// classifier — and it lives at the boundary where those errors are received.
fn ytmusic_client_error(error: ytmusicapi::Error) -> anyhow::Error {
    match classify_ytmusic_error(&error.to_string()) {
        Some(provider_error) => anyhow::Error::new(provider_error),
        None => error.into(),
    }
}

/// Classifies a `ytmusicapi` error string into a typed [`ProviderError`].
///
/// Returns `None` when the string carries no recognizable signal, in which case
/// the caller keeps the original error unclassified.
fn classify_ytmusic_error(message: &str) -> Option<ProviderError> {
    let lowered = message.to_ascii_lowercase();

    if looks_like_bot_block(&lowered) {
        return Some(ProviderError::blocked(message));
    }

    if lowered.contains("429")
        || lowered.contains("too many requests")
        || lowered.contains("quota")
        || lowered.contains("resource_exhausted")
        || lowered.contains("rate limit")
        || lowered.contains("rate-limit")
    {
        return Some(ProviderError::rate_limited(message, None));
    }

    if let Some(status) = parse_ytmusic_server_status(&lowered) {
        return Some(match status {
            401 | 403 => ProviderError::auth_failed(message),
            400 => ProviderError::invalid_argument(message),
            other => ProviderError::http(message, other),
        });
    }

    if lowered.contains("authentication required")
        || lowered.contains("invalid auth")
        || lowered.contains("unauthorized")
        || lowered.contains("forbidden")
        || lowered.contains("sign in")
        || lowered.contains("signin")
    {
        return Some(ProviderError::auth_failed(message));
    }

    if lowered.contains("invalid input") || lowered.contains("invalid_argument") {
        return Some(ProviderError::invalid_argument(message));
    }

    None
}

/// Extracts the HTTP status from a `ytmusicapi` `Error::Server` display string
/// of the form `Server error {status}: {message}`.
fn parse_ytmusic_server_status(lowered_message: &str) -> Option<u16> {
    let marker = "server error ";
    let index = lowered_message.find(marker)?;
    let suffix = &lowered_message[index + marker.len()..];
    let digits = suffix
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect::<String>();
    digits.parse::<u16>().ok()
}

/// Detects Google's anti-bot / "automated queries" interstitial in a response
/// body or error string.
fn looks_like_bot_block(raw: &str) -> bool {
    let lowered = raw.to_ascii_lowercase();
    lowered.contains("automated queries") || lowered.contains("unusual traffic")
}

/// Parses a `Retry-After` header value (delta-seconds) into a [`Duration`].
fn parse_ytmusic_retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    value
        .parse::<u64>()
        .ok()
        .or_else(|| {
            value
                .parse::<f64>()
                .ok()
                .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
                .map(|seconds| seconds.ceil() as u64)
        })
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use std::collections::{HashSet, VecDeque};
    use std::time::Duration;

    use serde_json::json;

    use super::{
        abort_push_error, apply_jitter, collect_continuation_tokens, collect_library_playlists,
        contains_key_recursive, dropped_tracks_warning, is_unrecoverable_failure,
        push_should_abort, youtube_music_cleanup_warning, ytm_backoff, ytm_retry_delay,
        DroppedTrackTally, MAX_DROPPED_TRACK_SAMPLE, PUSH_FAILURE_CIRCUIT_BREAKER,
        UNSUPPORTED_LIBRARY_RESET_MESSAGE, YTM_RETRY_MAX_BACKOFF,
    };
    use crate::error::{provider_failure, ProviderError, ProviderFailure};

    fn anyhow_error(provider_error: ProviderError) -> anyhow::Error {
        anyhow::Error::new(provider_error)
    }

    fn dropped_track(title: Option<&str>, artists: &[&str]) -> ytmusicapi::PlaylistTrack {
        serde_json::from_value(json!({
            "video_id": null,
            "title": title,
            "artists": artists
                .iter()
                .map(|name| json!({ "name": name, "id": null }))
                .collect::<Vec<_>>(),
            "album": null,
            "duration": null,
            "duration_seconds": null,
            "thumbnails": [],
            "is_available": false,
            "is_explicit": false,
            "set_video_id": null,
            "video_type": null,
        }))
        .expect("valid PlaylistTrack fixture")
    }

    #[test]
    fn detects_youtube_music_sign_in_prompt_hidden_in_success_response() {
        let response = json!({
            "contents": {
                "singleColumnBrowseResultsRenderer": {
                    "tabs": [{
                        "tabRenderer": {
                            "content": {
                                "messageRenderer": {
                                    "button": {
                                        "buttonRenderer": {
                                            "navigationEndpoint": {
                                                "signInEndpoint": { "hack": true }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }]
                }
            }
        });

        assert!(contains_key_recursive(&response, "signInEndpoint"));
        assert!(!contains_key_recursive(
            &response,
            "musicPlaylistShelfRenderer"
        ));
    }

    #[test]
    fn parses_paginated_youtube_music_library_playlists_without_duplicates() {
        let response = json!({
            "contents": [{
                "musicTwoRowItemRenderer": {
                    "title": { "runs": [{ "text": "Road trip" }] },
                    "navigationEndpoint": {
                        "browseEndpoint": { "browseId": "VLplaylist-1" }
                    }
                }
            }, {
                "musicTwoRowItemRenderer": {
                    "title": { "runs": [{ "text": "Duplicate" }] },
                    "navigationEndpoint": {
                        "watchEndpoint": { "playlistId": "playlist-1" }
                    }
                }
            }, {
                "continuationItemRenderer": {
                    "continuationEndpoint": {
                        "continuationCommand": { "token": "next-page" }
                    }
                }
            }]
        });
        let mut playlists = Vec::new();
        let mut seen_playlist_ids = HashSet::new();
        let mut tokens = VecDeque::new();

        collect_library_playlists(&response, &mut playlists, &mut seen_playlist_ids);
        collect_continuation_tokens(&response, &mut tokens);

        assert_eq!(playlists.len(), 1);
        assert_eq!(playlists[0].playlist_id, "playlist-1");
        assert_eq!(playlists[0].title, "Road trip");
        assert_eq!(tokens.pop_front().as_deref(), Some("next-page"));
    }

    #[test]
    fn cleanup_warning_preserves_replacement_playlist_context() {
        let warning = youtube_music_cleanup_warning(
            "Road trip",
            "old-playlist",
            "new-playlist",
            "permission denied",
        );

        assert!(warning.contains("Road trip"));
        assert!(warning.contains("old-playlist"));
        assert!(warning.contains("new-playlist"));
        assert!(warning.contains("permission denied"));
    }

    #[test]
    fn account_wide_reset_is_explicitly_unsupported() {
        assert!(UNSUPPORTED_LIBRARY_RESET_MESSAGE.contains("does not support"));
        assert!(UNSUPPORTED_LIBRARY_RESET_MESSAGE.contains("Normal pull, push"));
        assert!(UNSUPPORTED_LIBRARY_RESET_MESSAGE.contains("targeted canonical deletes"));
    }

    #[test]
    fn dropped_tracks_warning_lists_every_sample_when_under_cap() {
        let warning = dropped_tracks_warning(
            2,
            &["Joe Rogan - Episode".to_string(), "Serial - S1".to_string()],
        );
        assert!(warning.contains("Skipped 2 YouTube Music tracks"));
        assert!(warning.contains("Joe Rogan - Episode"));
        assert!(warning.contains("Serial - S1"));
        assert!(!warning.contains("and "));
    }

    #[test]
    fn dropped_tracks_warning_collapses_remainder_into_and_n_more() {
        let sample = (0..MAX_DROPPED_TRACK_SAMPLE)
            .map(|index| format!("Title {index}"))
            .collect::<Vec<_>>();
        let warning = dropped_tracks_warning(25, &sample);
        assert!(warning.contains("Skipped 25 YouTube Music tracks"));
        assert!(warning.contains("Title 0"));
        assert!(warning.contains(&format!("and {} more", 25 - MAX_DROPPED_TRACK_SAMPLE)));
    }

    #[test]
    fn dropped_tracks_warning_is_singular_for_one_track() {
        let warning = dropped_tracks_warning(1, &["Solo - Track".to_string()]);
        assert!(warning.contains("Skipped 1 YouTube Music track "));
    }

    #[test]
    fn dropped_tally_counts_all_but_samples_only_up_to_cap() {
        let mut tally = DroppedTrackTally::default();
        assert!(DroppedTrackTally::default().into_warning().is_none());

        for index in 0..(MAX_DROPPED_TRACK_SAMPLE + 5) {
            tally.record(&dropped_track(Some(&format!("Title {index}")), &["Artist"]));
        }
        assert_eq!(tally.total, MAX_DROPPED_TRACK_SAMPLE + 5);
        assert_eq!(tally.sample.len(), MAX_DROPPED_TRACK_SAMPLE);

        let warning = tally.into_warning().expect("warning for recorded drops");
        assert!(warning.contains(&format!("Skipped {} ", MAX_DROPPED_TRACK_SAMPLE + 5)));
        assert!(warning.contains("Artist - Title 0"));
        assert!(warning.contains("and 5 more"));
    }

    #[test]
    fn describe_dropped_track_falls_back_when_metadata_missing() {
        let mut tally = DroppedTrackTally::default();
        tally.record(&dropped_track(None, &[]));
        assert_eq!(tally.sample[0], "Unknown title");
    }

    #[test]
    fn unrecoverable_failures_are_rate_limit_and_auth_only() {
        assert!(is_unrecoverable_failure(&anyhow_error(
            ProviderError::rate_limited("slow", None)
        )));
        assert!(is_unrecoverable_failure(&anyhow_error(
            ProviderError::auth_failed("expired")
        )));

        assert!(!is_unrecoverable_failure(&anyhow_error(
            ProviderError::network("timeout")
        )));
        assert!(!is_unrecoverable_failure(&anyhow_error(
            ProviderError::blocked("automated queries")
        )));
        assert!(!is_unrecoverable_failure(&anyhow_error(
            ProviderError::invalid_argument("bad")
        )));
        assert!(!is_unrecoverable_failure(&anyhow_error(
            ProviderError::http("teapot", 418)
        )));
        assert!(!is_unrecoverable_failure(&anyhow::anyhow!("plain")));
    }

    #[test]
    fn circuit_breaker_aborts_immediately_on_unrecoverable_failures() {
        // Rate-limit and auth abort at the very first failure, ignoring count.
        assert!(push_should_abort(
            &anyhow_error(ProviderError::rate_limited("slow", None)),
            1
        ));
        assert!(push_should_abort(
            &anyhow_error(ProviderError::auth_failed("expired")),
            1
        ));
    }

    #[test]
    fn circuit_breaker_tolerates_transient_failures_until_threshold() {
        let transient = anyhow_error(ProviderError::network("blip"));
        for count in 1..PUSH_FAILURE_CIRCUIT_BREAKER {
            assert!(
                !push_should_abort(&transient, count),
                "should tolerate transient failure {count}"
            );
        }
        assert!(push_should_abort(&transient, PUSH_FAILURE_CIRCUIT_BREAKER));

        // An unclassified error also trips the breaker at the threshold.
        let plain = anyhow::anyhow!("mystery");
        assert!(!push_should_abort(&plain, PUSH_FAILURE_CIRCUIT_BREAKER - 1));
        assert!(push_should_abort(&plain, PUSH_FAILURE_CIRCUIT_BREAKER));
    }

    #[test]
    fn abort_push_error_annotates_only_circuit_breaker_aborts() {
        // Circuit-breaker abort gains context but keeps the typed failure
        // recoverable for downstream policy.
        let annotated = abort_push_error(anyhow_error(ProviderError::network("blip")), 5);
        assert!(annotated
            .to_string()
            .contains("5 consecutive item failures"));
        assert!(matches!(
            provider_failure(&annotated).map(ProviderError::failure),
            Some(ProviderFailure::Network)
        ));

        // Unrecoverable abort is returned verbatim (no circuit-breaker context).
        let verbatim = abort_push_error(anyhow_error(ProviderError::rate_limited("slow", None)), 1);
        assert!(!verbatim.to_string().contains("consecutive item failures"));
        assert!(matches!(
            provider_failure(&verbatim).map(ProviderError::failure),
            Some(ProviderFailure::RateLimited { .. })
        ));
    }

    #[test]
    fn retry_delay_only_covers_rate_limit_and_network() {
        // Rate limit honors a capped Retry-After.
        assert_eq!(
            ytm_retry_delay(
                &anyhow_error(ProviderError::rate_limited(
                    "slow",
                    Some(Duration::from_secs(7))
                )),
                1,
            ),
            Some(Duration::from_secs(7))
        );
        // Retry-After above the ceiling is clamped.
        assert_eq!(
            ytm_retry_delay(
                &anyhow_error(ProviderError::rate_limited(
                    "slow",
                    Some(Duration::from_secs(10_000))
                )),
                1,
            ),
            Some(YTM_RETRY_MAX_BACKOFF)
        );
        // Rate limit without Retry-After, and network, fall back to backoff.
        assert_eq!(
            ytm_retry_delay(&anyhow_error(ProviderError::rate_limited("slow", None)), 2),
            Some(ytm_backoff(2))
        );
        assert_eq!(
            ytm_retry_delay(&anyhow_error(ProviderError::network("blip")), 3),
            Some(ytm_backoff(3))
        );

        // Everything else is non-retryable.
        for non_retryable in [
            ProviderError::auth_failed("expired"),
            ProviderError::invalid_argument("bad"),
            ProviderError::blocked("automated queries"),
            ProviderError::http("teapot", 418),
        ] {
            assert_eq!(ytm_retry_delay(&anyhow_error(non_retryable), 1), None);
        }
        assert_eq!(ytm_retry_delay(&anyhow::anyhow!("plain"), 1), None);
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        assert_eq!(ytm_backoff(1), Duration::from_secs(1));
        assert_eq!(ytm_backoff(2), Duration::from_secs(2));
        assert_eq!(ytm_backoff(3), Duration::from_secs(4));
        assert_eq!(ytm_backoff(4), Duration::from_secs(8));
        assert!(ytm_backoff(100) <= YTM_RETRY_MAX_BACKOFF);
    }

    #[test]
    fn jitter_stays_within_bounds() {
        let base = Duration::from_secs(4);
        for _ in 0..256 {
            let jittered = apply_jitter(base);
            assert!(jittered >= base, "jitter must not shorten the delay");
            assert!(jittered <= base.mul_f64(1.5));
            assert!(jittered <= YTM_RETRY_MAX_BACKOFF);
        }
    }
}
