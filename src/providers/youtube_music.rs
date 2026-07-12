use std::collections::{HashSet, VecDeque};
use std::env;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde_json::{json, Value};
use tokio::time::sleep;
use ytmusicapi::{BrowserAuth, Privacy, YTMusicClient};

use crate::domain::{
    LibraryState, LinkSource, ObservedArtwork, ObservedPlaylist, ObservedPlaylistTrack,
    ObservedSavedTrack, ObservedTrack, ProviderKind, ProviderLibrarySnapshot, PurgeReport,
    SyncStatusRecord, SyncSummary, TrackMetadata, YoutubeMusicConnectionConfig,
};
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
            anyhow::bail!(
                "YouTube Music browser headers are expired or incomplete. Capture fresh signed-in headers from a music.youtube.com browse request and relink the account."
            );
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
                        anyhow::bail!("YouTube Music API returned an error: {error}");
                    }

                    return Ok(response_json);
                }
                Ok(response) => {
                    let status = response.status().as_u16();
                    let response_body = response
                        .text()
                        .await
                        .unwrap_or_else(|_| "Could not read response body".to_string());
                    if attempt < 4 && is_retryable_status(status) {
                        sleep(Duration::from_secs(1 << attempt)).await;
                        continue;
                    }
                    anyhow::bail!("YouTube Music request failed ({status}): {response_body}");
                }
                Err(error) => {
                    if attempt < 4 {
                        sleep(Duration::from_secs(1 << attempt)).await;
                        continue;
                    }
                    return Err(error.into());
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
        let liked = self
            .client
            .get_liked_songs(Some(UNBOUNDED_TRACK_LIMIT))
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

        let saved_total = liked.tracks.len();
        for (index, track) in liked.tracks.into_iter().enumerate() {
            if let Some(observed_track) = playlist_track_to_observed(&track) {
                snapshot.saved_tracks.push(ObservedSavedTrack {
                    added_at: None,
                    track: observed_track,
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
            let full_playlist = self
                .client
                .get_playlist(&playlist.playlist_id, Some(UNBOUNDED_TRACK_LIMIT))
                .await?;
            let item_count = full_playlist.tracks.len();
            let mut observed_playlist = ObservedPlaylist {
                provider_id: Some(full_playlist.id),
                name: full_playlist.title,
                description: full_playlist.description,
                tracks: Vec::new(),
            };

            for track in full_playlist.tracks {
                if let Some(observed_track) = playlist_track_to_observed(&track) {
                    observed_playlist.tracks.push(ObservedPlaylistTrack {
                        added_at: None,
                        track: observed_track,
                    });
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
                    if force {
                        if let Err(error) = self.client.like_song(&provider_id).await {
                            state.set_saved_track_status(
                                &target.saved_track_id,
                                ProviderKind::YoutubeMusic,
                                SyncStatusRecord::error(
                                    format!(
                                        "Failed to like '{}' on YouTube Music: {error}",
                                        target.metadata.display_label()
                                    ),
                                    now,
                                ),
                            );
                            return Err(error.into());
                        }
                    }
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
                let resolution = entry
                    .existing_provider_id
                    .as_ref()
                    .map(|provider_id| (provider_id.clone(), 1.0));
                match resolution {
                    Some((provider_id, confidence)) => {
                        state.upsert_track_link(
                            &entry.track_id,
                            ProviderKind::YoutubeMusic,
                            provider_id.clone(),
                            LinkSource::Match,
                            Some(confidence),
                            now,
                        );
                        state.set_track_status(
                            &entry.track_id,
                            ProviderKind::YoutubeMusic,
                            SyncStatusRecord::synced(
                                Some(provider_id.clone()),
                                Some(confidence),
                                Some("Resolved on YouTube Music".to_string()),
                                now,
                            ),
                        );
                        resolved_video_ids.push(provider_id.clone());
                        matched_entries.push((entry.entry_id.clone(), provider_id, confidence));
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

            if force {
                let playlist_to_replace = if let Some(provider_id) = &playlist.existing_provider_id
                {
                    Some(provider_id.clone())
                } else {
                    self.find_playlist_by_name(&playlist.name).await?
                };

                let created = self
                    .client
                    .create_playlist(
                        &playlist.name,
                        playlist.description.as_deref(),
                        Privacy::Private,
                    )
                    .await?;

                for chunk in resolved_video_ids.chunks(100) {
                    if let Err(error) = self
                        .client
                        .add_playlist_items(&created.playlist_id, chunk, false)
                        .await
                    {
                        let _ = self.client.delete_playlist(&created.playlist_id).await;
                        state.set_playlist_status(
                            &playlist.playlist_id,
                            ProviderKind::YoutubeMusic,
                            SyncStatusRecord::error(
                                format!(
                                    "Failed to sync YouTube Music playlist '{}': {error}",
                                    playlist.name
                                ),
                                now,
                            ),
                        );
                        return Err(error.into());
                    }
                }

                state.upsert_playlist_link(
                    &playlist.playlist_id,
                    ProviderKind::YoutubeMusic,
                    created.playlist_id.clone(),
                    LinkSource::Create,
                    Some(1.0),
                    now,
                );
                if let Some(existing_playlist_id) = playlist_to_replace {
                    if existing_playlist_id != created.playlist_id {
                        if let Err(error) = self.client.delete_playlist(&existing_playlist_id).await
                        {
                            summary.warnings.push(youtube_music_cleanup_warning(
                                &playlist.name,
                                &existing_playlist_id,
                                &created.playlist_id,
                                &error.to_string(),
                            ));
                        }
                    }
                }

                state.set_playlist_status(
                    &playlist.playlist_id,
                    ProviderKind::YoutubeMusic,
                    SyncStatusRecord::synced(
                        Some(created.playlist_id),
                        Some(1.0),
                        Some("Synced to YouTube Music".to_string()),
                        now,
                    ),
                );
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
            }

            for (entry_id, provider_id, confidence) in matched_entries {
                summary.playlist_entries_synced += 1;
                state.set_playlist_entry_status(
                    &playlist.playlist_id,
                    &entry_id,
                    ProviderKind::YoutubeMusic,
                    SyncStatusRecord::synced(
                        Some(provider_id),
                        Some(confidence),
                        Some(if force {
                            "Synced to YouTube Music".to_string()
                        } else {
                            "Resolved for YouTube Music sync dry run".to_string()
                        }),
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
        let _ = force;
        anyhow::bail!(UNSUPPORTED_LIBRARY_RESET_MESSAGE)
    }

    async fn remove_saved_track(&self, provider_track_id: &str) -> Result<()> {
        self.verify_connection().await?;
        self.client.unlike_song(provider_track_id).await?;
        Ok(())
    }

    async fn delete_playlist(&self, provider_playlist_id: &str) -> Result<()> {
        self.verify_connection().await?;
        self.client.delete_playlist(provider_playlist_id).await?;
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

#[cfg(test)]
mod tests {
    use std::collections::{HashSet, VecDeque};

    use serde_json::json;

    use super::{
        collect_continuation_tokens, collect_library_playlists, contains_key_recursive,
        youtube_music_cleanup_warning, UNSUPPORTED_LIBRARY_RESET_MESSAGE,
    };

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
}
