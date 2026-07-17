use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

#[derive(Clone, Copy, Debug)]
pub(crate) enum ApiErrorKind {
    BadRequest,
    NotFound,
    RateLimited,
    Internal,
}

#[derive(Debug)]
pub(crate) struct ApiError {
    pub(crate) kind: ApiErrorKind,
    pub(crate) message: String,
    /// The originating `anyhow` error, preserved so cooldown/health decisions
    /// can be made from the typed [`crate::error::ProviderError`] in its chain
    /// rather than from the display message.
    pub(crate) source: Option<anyhow::Error>,
}

#[derive(Serialize)]
pub(crate) struct ErrorPayload {
    pub(crate) error: String,
}

impl ApiError {
    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self {
            kind: ApiErrorKind::BadRequest,
            message: message.into(),
            source: None,
        }
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: ApiErrorKind::NotFound,
            message: message.into(),
            source: None,
        }
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: ApiErrorKind::Internal,
            message: message.into(),
            source: None,
        }
    }

    pub(crate) fn rate_limited(message: impl Into<String>) -> Self {
        Self {
            kind: ApiErrorKind::RateLimited,
            message: message.into(),
            source: None,
        }
    }

    /// Attaches the originating `anyhow` error so downstream cooldown/health
    /// classification can inspect the typed provider failure it carries.
    pub(crate) fn with_source(mut self, source: anyhow::Error) -> Self {
        self.source = Some(source);
        self
    }

    pub(crate) fn status_code(&self) -> StatusCode {
        match self.kind {
            ApiErrorKind::BadRequest => StatusCode::BAD_REQUEST,
            ApiErrorKind::NotFound => StatusCode::NOT_FOUND,
            ApiErrorKind::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            ApiErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

pub(crate) fn sanitize_error_message(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "The provider returned an empty error.".to_string();
    }

    if looks_like_google_block_page(trimmed) {
        return "YouTube Music blocked this request with a Google anti-bot page (403). Relink YouTube Music with fresh browser headers and try again. If it keeps happening, wait a bit or retry from the same browser and network you used to capture the headers.".to_string();
    }

    if looks_like_html(trimmed) {
        let stripped = strip_html(trimmed);
        if stripped.is_empty() {
            return "The provider returned an HTML error page instead of an API response."
                .to_string();
        }
        return truncate_message(&stripped, 280);
    }

    truncate_message(trimmed, 400)
}

pub(crate) fn looks_like_google_block_page(raw: &str) -> bool {
    let lowercase = raw.to_ascii_lowercase();
    lowercase.contains("automated queries")
        || (lowercase.contains("server error 403")
            && lowercase.contains("<html")
            && lowercase.contains("google"))
}

pub(crate) fn looks_like_html(raw: &str) -> bool {
    let lowercase = raw.to_ascii_lowercase();
    lowercase.contains("<html")
        || lowercase.contains("<body")
        || lowercase.contains("<title")
        || lowercase.contains("<div")
}

pub(crate) fn strip_html(raw: &str) -> String {
    let mut plain = String::with_capacity(raw.len());
    let mut in_tag = false;

    for character in raw.chars() {
        match character {
            '<' => {
                in_tag = true;
                plain.push(' ');
            }
            '>' => in_tag = false,
            _ if !in_tag => plain.push(character),
            _ => {}
        }
    }

    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn truncate_message(raw: &str, max_chars: usize) -> String {
    let count = raw.chars().count();
    if count <= max_chars {
        return raw.to_string();
    }

    let shortened = raw
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    format!("{shortened}…")
}

impl From<anyhow::Error> for ApiError {
    fn from(value: anyhow::Error) -> Self {
        Self {
            kind: ApiErrorKind::Internal,
            message: sanitize_error_message(&value.to_string()),
            source: Some(value),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status_code(),
            Json(ErrorPayload {
                error: self.message,
            }),
        )
            .into_response()
    }
}
