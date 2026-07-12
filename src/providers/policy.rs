//! Provider-facing policy shared by the CLI, web server, and identity sync.
//!
//! This is the single home for decisions that depend on the *category* of a
//! provider failure: whether a failure should trigger a cooldown (and for how
//! long), and whether a failure means the stored connection is unhealthy. All
//! classification is typed — it matches on [`ProviderFailure`] recovered from
//! the `anyhow` chain, never on message text.

use chrono::{Duration, Utc};

use crate::domain::{ProviderCooldown, ProviderKind};
use crate::error::{provider_failure, ProviderFailure};

/// Default cooldown applied to a rate-limited Spotify request.
const SPOTIFY_DEFAULT_COOLDOWN_SECS: i64 = 15 * 60;
/// Default cooldown applied to a rate-limited YouTube Music request.
const YOUTUBE_MUSIC_DEFAULT_COOLDOWN_SECS: i64 = 30 * 60;
/// Hard ceiling on any cooldown, regardless of the provider's `Retry-After`.
const MAX_COOLDOWN_SECS: i64 = 24 * 60 * 60;

/// Derives a [`ProviderCooldown`] from a provider failure.
///
/// Only [`ProviderFailure::RateLimited`] produces a cooldown. Its duration is
/// `max(retry_after, provider_default)` clamped to 24 hours; the reason is the
/// provider error's human message.
pub fn cooldown_from_error(
    provider: ProviderKind,
    error: &anyhow::Error,
) -> Option<ProviderCooldown> {
    let provider_error = provider_failure(error)?;
    let ProviderFailure::RateLimited { retry_after } = provider_error.failure() else {
        return None;
    };

    let default_secs = match provider {
        ProviderKind::Spotify => SPOTIFY_DEFAULT_COOLDOWN_SECS,
        ProviderKind::YoutubeMusic => YOUTUBE_MUSIC_DEFAULT_COOLDOWN_SECS,
    };
    let retry_after_secs = retry_after
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(MAX_COOLDOWN_SECS))
        .unwrap_or(0);
    let blocked_secs = retry_after_secs.max(default_secs).min(MAX_COOLDOWN_SECS);

    let now = Utc::now();
    Some(ProviderCooldown {
        provider,
        blocked_until: now + Duration::seconds(blocked_secs),
        reason: provider_error.to_string(),
        updated_at: now,
    })
}

/// Reports whether a failure means the stored provider connection is unhealthy.
///
/// Auth failures, anti-bot blocks, transport-level failures, and 401/403
/// responses all indicate the connection needs attention (relink / check
/// connection). Rate limits and other statuses do not flip health.
pub fn is_connection_health_failure(error: &anyhow::Error) -> bool {
    provider_failure(error).is_some_and(|provider_error| {
        matches!(
            provider_error.failure(),
            ProviderFailure::AuthFailed
                | ProviderFailure::Blocked
                | ProviderFailure::Network
                | ProviderFailure::Http { status: 401 | 403 }
        )
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration as StdDuration;

    use crate::error::ProviderError;

    use super::*;

    fn anyhow_from(error: ProviderError) -> anyhow::Error {
        anyhow::Error::new(error).context("while talking to the provider")
    }

    #[test]
    fn rate_limit_without_retry_after_uses_provider_default() {
        let error = anyhow_from(ProviderError::rate_limited("slow down", None));

        let spotify = cooldown_from_error(ProviderKind::Spotify, &error).unwrap();
        let spotify_secs = (spotify.blocked_until - spotify.updated_at).num_seconds();
        assert_eq!(spotify_secs, SPOTIFY_DEFAULT_COOLDOWN_SECS);
        assert_eq!(spotify.provider, ProviderKind::Spotify);
        assert_eq!(spotify.reason, "slow down");

        let ytm = cooldown_from_error(ProviderKind::YoutubeMusic, &error).unwrap();
        let ytm_secs = (ytm.blocked_until - ytm.updated_at).num_seconds();
        assert_eq!(ytm_secs, YOUTUBE_MUSIC_DEFAULT_COOLDOWN_SECS);
    }

    #[test]
    fn rate_limit_uses_retry_after_when_longer_than_default() {
        let error = anyhow_from(ProviderError::rate_limited(
            "slow down",
            Some(StdDuration::from_secs(60 * 60)),
        ));

        let cooldown = cooldown_from_error(ProviderKind::Spotify, &error).unwrap();
        let secs = (cooldown.blocked_until - cooldown.updated_at).num_seconds();
        assert_eq!(secs, 60 * 60);
    }

    #[test]
    fn rate_limit_retry_after_below_default_is_floored_to_default() {
        let error = anyhow_from(ProviderError::rate_limited(
            "slow down",
            Some(StdDuration::from_secs(5)),
        ));

        let cooldown = cooldown_from_error(ProviderKind::Spotify, &error).unwrap();
        let secs = (cooldown.blocked_until - cooldown.updated_at).num_seconds();
        assert_eq!(secs, SPOTIFY_DEFAULT_COOLDOWN_SECS);
    }

    #[test]
    fn rate_limit_retry_after_is_clamped_to_24_hours() {
        let error = anyhow_from(ProviderError::rate_limited(
            "slow down",
            Some(StdDuration::from_secs(72 * 60 * 60)),
        ));

        let cooldown = cooldown_from_error(ProviderKind::YoutubeMusic, &error).unwrap();
        let secs = (cooldown.blocked_until - cooldown.updated_at).num_seconds();
        assert_eq!(secs, MAX_COOLDOWN_SECS);
    }

    #[test]
    fn non_rate_limited_failures_produce_no_cooldown() {
        for error in [
            ProviderError::auth_failed("nope"),
            ProviderError::invalid_argument("bad query"),
            ProviderError::blocked("captcha"),
            ProviderError::network("timeout"),
            ProviderError::http("teapot", 418),
        ] {
            let error = anyhow_from(error);
            assert!(cooldown_from_error(ProviderKind::Spotify, &error).is_none());
        }
    }

    #[test]
    fn plain_error_produces_no_cooldown() {
        let error = anyhow::anyhow!("database is on fire");
        assert!(cooldown_from_error(ProviderKind::Spotify, &error).is_none());
    }

    #[test]
    fn health_failure_classification_is_typed() {
        assert!(is_connection_health_failure(&anyhow_from(
            ProviderError::auth_failed("expired")
        )));
        assert!(is_connection_health_failure(&anyhow_from(
            ProviderError::blocked("automated queries")
        )));
        assert!(is_connection_health_failure(&anyhow_from(
            ProviderError::network("connection refused")
        )));
        assert!(is_connection_health_failure(&anyhow_from(
            ProviderError::http("forbidden", 403)
        )));
        assert!(is_connection_health_failure(&anyhow_from(
            ProviderError::http("unauthorized", 401)
        )));

        assert!(!is_connection_health_failure(&anyhow_from(
            ProviderError::rate_limited("slow down", None)
        )));
        assert!(!is_connection_health_failure(&anyhow_from(
            ProviderError::invalid_argument("bad query")
        )));
        assert!(!is_connection_health_failure(&anyhow_from(
            ProviderError::http("server error", 500)
        )));
        assert!(!is_connection_health_failure(&anyhow::anyhow!(
            "unrelated failure"
        )));
    }
}
