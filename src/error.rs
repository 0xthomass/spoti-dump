//! Typed provider error foundation.
//!
//! Providers classify failures **at the boundary** where HTTP status codes,
//! response headers, and error strings are still in hand, then attach a
//! [`ProviderError`] into the `anyhow` error chain that callers see. Downstream
//! policy (`crate::providers::policy`) and the CLI/web/identity call sites match
//! on the structured [`ProviderFailure`] instead of sniffing message text.

use std::fmt;
use std::time::Duration;

/// The structured category of a provider failure.
///
/// This is the machine-readable classification. Human-facing detail lives in
/// [`ProviderError::message`].
#[derive(Debug, Clone)]
pub enum ProviderFailure {
    /// The provider rate-limited the request. `retry_after` carries the
    /// provider-supplied backoff hint (e.g. a parsed `Retry-After` header) when
    /// one was present.
    RateLimited { retry_after: Option<Duration> },
    /// Authentication/authorization failed: expired or missing credentials,
    /// insufficient scopes, or a 401/403 that is not an anti-bot block page.
    AuthFailed,
    /// The provider rejected the request as malformed (e.g. a 400 or an
    /// "invalid input"/"invalid argument" style rejection of a generated query).
    InvalidArgument,
    /// The request was intercepted by a bot-block / anti-abuse HTML page (e.g.
    /// Google's "automated queries" interstitial).
    Blocked,
    /// A transport-level failure (connect/timeout) before a usable HTTP response
    /// was received.
    Network,
    /// Any other unsuccessful HTTP response, carrying the raw status code.
    Http { status: u16 },
}

/// A provider failure with a human-readable message.
///
/// Implements [`std::error::Error`] so it can be carried inside an
/// [`anyhow::Error`] chain and later recovered via [`provider_failure`].
#[derive(Debug, Clone)]
pub struct ProviderError {
    failure: ProviderFailure,
    message: String,
}

impl ProviderError {
    /// Builds a [`ProviderFailure::RateLimited`] error.
    pub fn rate_limited(message: impl Into<String>, retry_after: Option<Duration>) -> Self {
        Self {
            failure: ProviderFailure::RateLimited { retry_after },
            message: message.into(),
        }
    }

    /// Builds a [`ProviderFailure::AuthFailed`] error.
    pub fn auth_failed(message: impl Into<String>) -> Self {
        Self {
            failure: ProviderFailure::AuthFailed,
            message: message.into(),
        }
    }

    /// Builds a [`ProviderFailure::InvalidArgument`] error.
    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self {
            failure: ProviderFailure::InvalidArgument,
            message: message.into(),
        }
    }

    /// Builds a [`ProviderFailure::Blocked`] error.
    pub fn blocked(message: impl Into<String>) -> Self {
        Self {
            failure: ProviderFailure::Blocked,
            message: message.into(),
        }
    }

    /// Builds a [`ProviderFailure::Network`] error.
    pub fn network(message: impl Into<String>) -> Self {
        Self {
            failure: ProviderFailure::Network,
            message: message.into(),
        }
    }

    /// Builds a [`ProviderFailure::Http`] error carrying the raw status code.
    pub fn http(message: impl Into<String>, status: u16) -> Self {
        Self {
            failure: ProviderFailure::Http { status },
            message: message.into(),
        }
    }

    /// The structured failure classification.
    pub fn failure(&self) -> &ProviderFailure {
        &self.failure
    }

    /// The human-readable message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProviderError {}

/// Walks an [`anyhow::Error`] chain and returns the first [`ProviderError`] found.
///
/// This is the single recovery point for the typed classification a provider
/// attached at its boundary.
pub fn provider_failure(error: &anyhow::Error) -> Option<&ProviderError> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ProviderError>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_failure_recovers_error_from_anyhow_chain() {
        let error = anyhow::Error::new(ProviderError::rate_limited(
            "boom",
            Some(Duration::from_secs(42)),
        ))
        .context("while pulling library");

        let recovered = provider_failure(&error).expect("provider error in chain");
        assert!(matches!(
            recovered.failure(),
            ProviderFailure::RateLimited {
                retry_after: Some(duration)
            } if *duration == Duration::from_secs(42)
        ));
        assert_eq!(recovered.message(), "boom");
    }

    #[test]
    fn provider_failure_is_none_for_plain_errors() {
        let error = anyhow::anyhow!("just a string error");
        assert!(provider_failure(&error).is_none());
    }

    #[test]
    fn display_is_the_human_message() {
        let error = ProviderError::http("service exploded", 503);
        assert_eq!(error.to_string(), "service exploded");
    }
}
