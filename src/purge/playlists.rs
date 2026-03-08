use anyhow::Result;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_LENGTH};
use serde_json::Value;

use crate::utils;

pub async fn purge_playlists(access_token: &str, force: bool) -> Result<Vec<String>> {
    println!("Purging playlists...");

    let playlists: Vec<Value> =
        utils::get_all_items(access_token, "https://api.spotify.com/v1/me/playlists").await?;
    let playlist_names: Vec<String> = playlists
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_string())
        .collect();

    if force {
        let client = reqwest::Client::new();
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", access_token))?,
        );
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("0"));
        let playlist_uris: Vec<String> = playlists
            .iter()
            .filter_map(|playlist| {
                playlist["id"]
                    .as_str()
                    .map(|playlist_id| format!("spotify:playlist:{playlist_id}"))
            })
            .collect();

        for chunk in playlist_uris.chunks(40) {
            let response = client
                .delete("https://api.spotify.com/v1/me/library")
                .headers(headers.clone())
                .query(&[("uris", chunk.join(","))])
                .body(String::new())
                .send()
                .await?;

            if !response.status().is_success() {
                return Err(utils::response_error("Failed to unfollow playlists", response).await);
            }
        }

        for name in &playlist_names {
            println!("Unfollowed playlist: {}", name);
        }
        println!("Playlists purged successfully.");
        Ok(Vec::new())
    } else {
        println!("Found {} playlists to unfollow.", playlists.len());
        for name in &playlist_names {
            println!("- {}", name);
        }
        println!("Playlists purge dry run complete.");
        Ok(playlist_names)
    }
}
