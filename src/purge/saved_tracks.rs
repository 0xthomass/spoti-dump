use anyhow::Result;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_LENGTH};
use serde_json::Value;

use crate::utils;

pub async fn purge_saved_tracks(access_token: &str, force: bool) -> Result<Vec<String>> {
    println!("Purging saved tracks...");

    let tracks: Vec<Value> =
        utils::get_all_items(access_token, "https://api.spotify.com/v1/me/tracks").await?;
    let track_uris: Vec<String> = tracks
        .into_iter()
        .filter_map(|track| {
            track["track"]["id"]
                .as_str()
                .map(|track_id| format!("spotify:track:{track_id}"))
        })
        .collect();

    if force {
        let client = reqwest::Client::new();
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", access_token))?,
        );
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("0"));

        for chunk in track_uris.chunks(40) {
            let response = client
                .delete("https://api.spotify.com/v1/me/library")
                .headers(headers.clone())
                .query(&[("uris", chunk.join(","))])
                .body(String::new())
                .send()
                .await?;

            if !response.status().is_success() {
                return Err(utils::response_error("Failed to purge saved tracks", response).await);
            }
            println!("Purged a chunk of saved tracks.");
        }
        println!("Saved tracks purged successfully.");
        Ok(Vec::new())
    } else {
        println!("Found {} saved tracks to purge.", track_uris.len());
        println!("Saved tracks purge dry run complete.");
        Ok(track_uris)
    }
}
