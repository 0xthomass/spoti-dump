use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::sync::Semaphore;

use crate::domain::{LibraryState, ProviderKind};

use super::dto::SpotifyOembedResponse;
use super::error::ApiError;
use super::projections::preferred_artwork;
use super::{persist_library, AppContext};

/// How long a track that yielded no artwork is skipped before enrichment
/// retries it, to prevent refetch storms for artwork-less tracks.
const ARTWORK_NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
/// Upper bound on how many tracks a single background enrichment pass fetches,
/// so one browse request cannot schedule an unbounded run of external lookups.
const ARTWORK_ENRICHMENT_BATCH: usize = 50;
/// Concurrent external artwork fetches within one enrichment pass.
const ARTWORK_FETCH_CONCURRENCY: usize = 4;

/// Resolved artwork for one track: which provider it came from, the URL, and
/// its dimensions.
pub(crate) type ResolvedArtwork = (ProviderKind, String, Option<u32>, Option<u32>);

/// Schedules a debounced background pass that fills in missing artwork for the
/// given candidate track IDs. Browse handlers call this after building a page;
/// it never blocks the request. At most one pass runs at a time — if one is
/// already in flight the call is a no-op, and a later browse schedules the next.
pub(crate) fn schedule_artwork_enrichment(context: &Arc<AppContext>, requested: Vec<String>) {
    if requested.is_empty() {
        return;
    }
    // Debounce via the one-permit semaphore: acquire before spawning and hold
    // the permit for the whole pass.
    let Ok(permit) = context.artwork_semaphore.clone().try_acquire_owned() else {
        return;
    };
    let context = context.clone();
    tokio::spawn(async move {
        let _permit = permit;
        run_artwork_enrichment(context, requested).await;
    });
}

/// One background artwork pass: pick the tracks that still need artwork (honoring
/// the negative cache), fetch with bounded concurrency and no lock held, then
/// apply the results onto the current state under a single write lock. Does not
/// bump `library_version` — artwork is self-healing bookkeeping, not a user edit,
/// so it must not trip a concurrent operation's edit detection.
pub(crate) async fn run_artwork_enrichment(context: Arc<AppContext>, requested: Vec<String>) {
    let targets = {
        let state = context.library.read().await;
        collect_artwork_targets(&context, &state, &requested)
    };
    if targets.is_empty() {
        return;
    }

    // Fetch artwork concurrently (bounded), never holding the library lock.
    let permits = Arc::new(Semaphore::new(ARTWORK_FETCH_CONCURRENCY));
    let mut tasks = tokio::task::JoinSet::new();
    for (track_id, spotify_id, youtube_id) in targets {
        let client = context.http_client.clone();
        let permits = permits.clone();
        tasks.spawn(async move {
            let _permit = permits.acquire_owned().await.ok();
            let artwork = resolve_track_artwork(&client, spotify_id, youtube_id).await;
            (track_id, artwork)
        });
    }

    let mut resolved: Vec<(String, ResolvedArtwork)> = Vec::new();
    let mut misses: Vec<String> = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((track_id, Some(artwork))) => resolved.push((track_id, artwork)),
            Ok((track_id, None)) => misses.push(track_id),
            Err(error) => eprintln!("Artwork enrichment task failed: {error}"),
        }
    }

    if !resolved.is_empty() {
        let now = Utc::now();
        let mut state = context.library.write().await;
        let mut changed = false;
        for (track_id, (provider, url, width, height)) in &resolved {
            let still_missing = state
                .tracks
                .iter()
                .find(|track| track.id == *track_id)
                .map(|track| preferred_artwork(track).is_none())
                .unwrap_or(false);
            if still_missing {
                state.upsert_track_artwork(track_id, *provider, url.clone(), *width, *height, now);
                changed = true;
            }
        }
        if changed {
            if let Err(error) = persist_library(&state).await {
                eprintln!("Failed to persist enriched artwork: {}", error.message);
            }
        }
    }

    if !misses.is_empty() {
        let now = Instant::now();
        if let Ok(mut cache) = context.artwork_negative_cache.lock() {
            for track_id in misses {
                cache.insert(track_id, now);
            }
        }
    }
}

/// Selects, from the requested track IDs, those that still lack artwork, have a
/// provider link to fetch from, and are not in the negative cache. Bounded to
/// [`ARTWORK_ENRICHMENT_BATCH`] so one browse request cannot schedule an
/// unbounded run of external lookups.
pub(crate) fn collect_artwork_targets(
    context: &Arc<AppContext>,
    state: &LibraryState,
    requested: &[String],
) -> Vec<(String, Option<String>, Option<String>)> {
    let cache = context.artwork_negative_cache.lock().ok();
    let now = Instant::now();
    let mut seen = std::collections::HashSet::new();
    let mut targets = Vec::new();
    for track_id in requested {
        if targets.len() >= ARTWORK_ENRICHMENT_BATCH {
            break;
        }
        if !seen.insert(track_id.as_str()) {
            continue;
        }
        let Some(track) = state.tracks.iter().find(|track| track.id == *track_id) else {
            continue;
        };
        if preferred_artwork(track).is_some() {
            continue;
        }
        if let Some(cache) = cache.as_ref() {
            if let Some(last) = cache.get(track_id) {
                if negative_cache_is_fresh(*last, now) {
                    continue;
                }
            }
        }
        let spotify_id = track
            .provider_links
            .get(ProviderKind::Spotify.as_key())
            .map(|link| link.provider_id.clone());
        let youtube_id = track
            .provider_links
            .get(ProviderKind::YoutubeMusic.as_key())
            .map(|link| link.provider_id.clone());
        if spotify_id.is_none() && youtube_id.is_none() {
            continue;
        }
        targets.push((track_id.clone(), spotify_id, youtube_id));
    }
    targets
}

/// Whether a negative-cache entry recorded at `last` is still fresh at `now`
/// (younger than [`ARTWORK_NEGATIVE_CACHE_TTL`]), meaning the artwork-less track
/// should be skipped this pass to avoid refetch storms.
pub(crate) fn negative_cache_is_fresh(last: Instant, now: Instant) -> bool {
    now.duration_since(last) < ARTWORK_NEGATIVE_CACHE_TTL
}

/// Resolves artwork for one track: prefers a Spotify oembed lookup (network),
/// falling back to the deterministic YouTube thumbnail. Returns `None` only when
/// there is no artwork to be had (no YouTube link and Spotify yielded nothing),
/// which the caller records in the negative cache.
pub(crate) async fn resolve_track_artwork(
    client: &reqwest::Client,
    spotify_id: Option<String>,
    youtube_id: Option<String>,
) -> Option<ResolvedArtwork> {
    if let Some(spotify_id) = spotify_id {
        match fetch_spotify_oembed_artwork(client, &spotify_id).await {
            Ok(Some((url, width, height))) => {
                return Some((ProviderKind::Spotify, url, width, height))
            }
            Ok(None) => {}
            Err(error) => eprintln!(
                "Artwork lookup failed for Spotify track {spotify_id}: {}",
                error.message
            ),
        }
    }
    if let Some(youtube_id) = youtube_id {
        return Some((
            ProviderKind::YoutubeMusic,
            youtube_thumbnail_url(&youtube_id),
            Some(480),
            Some(360),
        ));
    }
    None
}

pub(crate) async fn fetch_spotify_oembed_artwork(
    client: &reqwest::Client,
    provider_id: &str,
) -> Result<Option<(String, Option<u32>, Option<u32>)>, ApiError> {
    let response = client
        .get("https://open.spotify.com/oembed")
        .query(&[("url", format!("spotify:track:{provider_id}"))])
        .send()
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;

    if !response.status().is_success() {
        return Ok(None);
    }

    let payload: SpotifyOembedResponse = response
        .json()
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    Ok(payload
        .thumbnail_url
        .map(|url| (url, payload.thumbnail_width, payload.thumbnail_height)))
}

pub(crate) fn youtube_thumbnail_url(video_id: &str) -> String {
    format!("https://i.ytimg.com/vi/{video_id}/hqdefault.jpg")
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::negative_cache_is_fresh;

    #[test]
    fn artwork_negative_cache_gate_skips_only_within_ttl() {
        let last = Instant::now();
        // A fresh miss (within the 1h TTL) is skipped.
        assert!(negative_cache_is_fresh(
            last,
            last + Duration::from_secs(30)
        ));
        assert!(negative_cache_is_fresh(
            last,
            last + Duration::from_secs(60 * 59)
        ));
        // Once older than the TTL the track becomes eligible for a refetch.
        assert!(!negative_cache_is_fresh(
            last,
            last + Duration::from_secs(60 * 60 + 1)
        ));
        assert!(!negative_cache_is_fresh(
            last,
            last + Duration::from_secs(3 * 60 * 60)
        ));
    }
}
