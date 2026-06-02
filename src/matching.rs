use strsim::jaro_winkler;

use crate::model::TrackMetadata;

const MIN_ACCEPTABLE_SCORE: f64 = 0.78;
const MIN_DECISIVE_MARGIN: f64 = 0.03;
const HIGH_CONFIDENCE_SCORE: f64 = 0.93;

#[derive(Clone, Debug)]
pub struct MatchCandidate {
    pub id: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub duration_seconds: Option<u32>,
    pub source_weight: f64,
}

#[derive(Clone, Debug)]
pub struct RankedCandidate {
    pub id: String,
    pub score: f64,
}

pub fn best_candidate(
    track: &TrackMetadata,
    candidates: &[MatchCandidate],
) -> Option<RankedCandidate> {
    let mut scored_candidates = candidates
        .iter()
        .map(|candidate| (candidate, score_candidate(track, candidate)))
        .collect::<Vec<_>>();

    scored_candidates.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let (best_candidate, best_score) = *scored_candidates.first()?;
    let second_best_score = scored_candidates
        .get(1)
        .map(|(_, score)| *score)
        .unwrap_or(0.0);

    if best_score < MIN_ACCEPTABLE_SCORE {
        return None;
    }

    if best_score < HIGH_CONFIDENCE_SCORE && best_score - second_best_score < MIN_DECISIVE_MARGIN {
        return None;
    }

    Some(RankedCandidate {
        id: best_candidate.id.clone(),
        score: best_score,
    })
}

pub fn metadata_similarity(left: &TrackMetadata, right: &TrackMetadata) -> f64 {
    let title_score = similarity(&left.title, &right.title);
    let artist_score = score_artists(&left.artists, &right.artists);
    let album_score = match (&left.album, &right.album) {
        (Some(left), Some(right)) => similarity(left, right),
        _ => 0.5,
    };
    let duration_score = match (left.duration_seconds, right.duration_seconds) {
        (Some(left), Some(right)) => {
            let difference = left.abs_diff(right) as f64;
            (1.0 - (difference / 30.0)).clamp(0.0, 1.0)
        }
        _ => 0.5,
    };

    title_score * 0.45 + artist_score * 0.30 + album_score * 0.15 + duration_score * 0.10
}

pub fn score_candidate(track: &TrackMetadata, candidate: &MatchCandidate) -> f64 {
    let candidate_metadata = TrackMetadata {
        title: candidate.title.clone(),
        artists: candidate.artists.clone(),
        album: candidate.album.clone(),
        duration_seconds: candidate.duration_seconds,
        isrc: None,
    };

    metadata_similarity(track, &candidate_metadata) + candidate.source_weight
}

pub fn cleaned_title(title: &str) -> String {
    let lowered = title.to_ascii_lowercase();
    for marker in [" (feat.", " (ft.", " [feat.", " [ft."] {
        if let Some(index) = lowered.find(marker) {
            return title[..index].trim().to_string();
        }
    }

    title.trim().to_string()
}

fn score_artists(left: &[String], right: &[String]) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.5;
    }

    let joined_left = left.join(" ");
    let joined_right = right.join(" ");
    let primary_score = similarity(&left[0], &right[0]);
    let combined_score = similarity(&joined_left, &joined_right);
    primary_score.max(combined_score)
}

fn similarity(left: &str, right: &str) -> f64 {
    let left = normalize_for_match(left);
    let right = normalize_for_match(right);

    if left.is_empty() || right.is_empty() {
        return 0.0;
    }

    jaro_winkler(&left, &right)
}

fn normalize_for_match(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character.is_ascii_whitespace() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_track() -> TrackMetadata {
        TrackMetadata {
            title: "Sweet Dreams (Are Made of This) - 2005 Remaster".to_string(),
            artists: vec!["Eurythmics".to_string()],
            album: Some("Sweet Dreams".to_string()),
            duration_seconds: Some(216),
            isrc: None,
        }
    }

    #[test]
    fn strips_featured_suffixes_from_search_titles() {
        assert_eq!(cleaned_title("Song Name (feat. Someone)"), "Song Name");
        assert_eq!(cleaned_title("Song Name"), "Song Name");
    }

    #[test]
    fn prefers_the_closest_candidate() {
        let track = sample_track();
        let candidates = vec![
            MatchCandidate {
                id: "bad".to_string(),
                title: "Completely Different".to_string(),
                artists: vec!["Other".to_string()],
                album: Some("Elsewhere".to_string()),
                duration_seconds: Some(100),
                source_weight: 0.0,
            },
            MatchCandidate {
                id: "good".to_string(),
                title: "Sweet Dreams (Are Made of This)".to_string(),
                artists: vec!["Eurythmics".to_string()],
                album: Some("Sweet Dreams".to_string()),
                duration_seconds: Some(215),
                source_weight: 0.1,
            },
        ];

        assert_eq!(
            best_candidate(&track, &candidates).map(|candidate| candidate.id),
            Some("good".to_string())
        );
    }

    #[test]
    fn rejects_ambiguous_low_confidence_matches() {
        let track = TrackMetadata {
            title: "Night".to_string(),
            artists: vec!["Artist".to_string()],
            album: None,
            duration_seconds: None,
            isrc: None,
        };
        let candidates = vec![
            MatchCandidate {
                id: "first".to_string(),
                title: "Night".to_string(),
                artists: vec!["Artist One".to_string()],
                album: None,
                duration_seconds: None,
                source_weight: 0.0,
            },
            MatchCandidate {
                id: "second".to_string(),
                title: "Night".to_string(),
                artists: vec!["Artist Two".to_string()],
                album: None,
                duration_seconds: None,
                source_weight: 0.0,
            },
        ];

        assert!(best_candidate(&track, &candidates).is_none());
    }
}
