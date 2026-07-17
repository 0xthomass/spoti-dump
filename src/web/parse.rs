use crate::domain::ProviderKind;

use super::error::ApiError;

pub(crate) fn normalize_manual_provider_track_id(
    provider: ProviderKind,
    raw_value: &str,
) -> Result<String, ApiError> {
    let value = raw_value.trim();
    if value.is_empty() {
        return Err(ApiError::bad_request("Provider track ID is required."));
    }

    let provider_id = match provider {
        ProviderKind::Spotify => parse_spotify_track_id(value)?,
        ProviderKind::YoutubeMusic => parse_youtube_music_video_id(value)?,
    };

    Ok(provider_id)
}

pub(crate) fn parse_spotify_track_id(value: &str) -> Result<String, ApiError> {
    let provider_id = if let Some(rest) = value.strip_prefix("spotify:track:") {
        rest.split(':').next().unwrap_or(rest).to_string()
    } else if value.starts_with("http://") || value.starts_with("https://") {
        let url = url::Url::parse(value)
            .map_err(|_| ApiError::bad_request("Spotify URL is not valid."))?;
        let host = url.host_str().unwrap_or_default();
        if host != "open.spotify.com" {
            return Err(ApiError::bad_request(
                "Spotify URL must be from open.spotify.com.",
            ));
        }
        let segments = url
            .path_segments()
            .map(|segments| segments.collect::<Vec<_>>())
            .unwrap_or_default();
        let Some(track_index) = segments.iter().position(|segment| *segment == "track") else {
            return Err(ApiError::bad_request(
                "Spotify URL must point to a track, not an album, artist, or playlist.",
            ));
        };
        segments
            .get(track_index + 1)
            .copied()
            .unwrap_or_default()
            .to_string()
    } else {
        value.to_string()
    };

    if provider_id.len() != 22
        || !provider_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return Err(ApiError::bad_request(
            "Spotify track IDs must be 22 base62 characters or a Spotify track URL.",
        ));
    }

    Ok(provider_id)
}

pub(crate) fn parse_youtube_music_video_id(value: &str) -> Result<String, ApiError> {
    let provider_id = if value.starts_with("http://") || value.starts_with("https://") {
        let url = url::Url::parse(value)
            .map_err(|_| ApiError::bad_request("YouTube Music URL is not valid."))?;
        let host = url.host_str().unwrap_or_default();
        if host == "youtu.be" {
            url.path_segments()
                .and_then(|mut segments| segments.next())
                .unwrap_or_default()
                .to_string()
        } else if matches!(
            host,
            "music.youtube.com" | "www.youtube.com" | "youtube.com" | "m.youtube.com"
        ) {
            url.query_pairs()
                .find(|(key, _)| key == "v")
                .map(|(_, value)| value.into_owned())
                .unwrap_or_default()
        } else {
            return Err(ApiError::bad_request(
                "YouTube Music URL must be from music.youtube.com, youtube.com, or youtu.be.",
            ));
        }
    } else {
        value.to_string()
    };

    if !(3..=64).contains(&provider_id.len())
        || !provider_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(ApiError::bad_request(
            "YouTube Music video IDs must use only letters, numbers, '-' or '_'.",
        ));
    }

    Ok(provider_id)
}

#[cfg(test)]
mod tests {
    use crate::domain::ProviderKind;

    use super::normalize_manual_provider_track_id;

    #[test]
    fn normalizes_manual_spotify_track_ids_and_rejects_non_track_urls() {
        assert_eq!(
            normalize_manual_provider_track_id(
                ProviderKind::Spotify,
                "https://open.spotify.com/track/2ZrSFxVUAvbgVMqYTkfx3B?si=abc"
            )
            .unwrap(),
            "2ZrSFxVUAvbgVMqYTkfx3B"
        );
        assert_eq!(
            normalize_manual_provider_track_id(
                ProviderKind::Spotify,
                "spotify:track:2ZrSFxVUAvbgVMqYTkfx3B"
            )
            .unwrap(),
            "2ZrSFxVUAvbgVMqYTkfx3B"
        );
        assert!(normalize_manual_provider_track_id(
            ProviderKind::Spotify,
            "https://open.spotify.com/playlist/2ZrSFxVUAvbgVMqYTkfx3B"
        )
        .is_err());
    }

    #[test]
    fn normalizes_manual_youtube_music_video_ids() {
        assert_eq!(
            normalize_manual_provider_track_id(
                ProviderKind::YoutubeMusic,
                "https://music.youtube.com/watch?v=O3FrSTTpZ_U&list=RDAMVM"
            )
            .unwrap(),
            "O3FrSTTpZ_U"
        );
        assert_eq!(
            normalize_manual_provider_track_id(
                ProviderKind::YoutubeMusic,
                "https://youtu.be/O3FrSTTpZ_U"
            )
            .unwrap(),
            "O3FrSTTpZ_U"
        );
        assert!(normalize_manual_provider_track_id(
            ProviderKind::YoutubeMusic,
            "https://example.com/watch?v=O3FrSTTpZ_U"
        )
        .is_err());
    }
}
