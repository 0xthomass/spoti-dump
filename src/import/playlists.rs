use anyhow::Result;
use csv::Reader;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::json;
use std::fs;
use std::path::Path;

use crate::utils;

pub async fn import_playlists(access_token: &str, force: bool) -> Result<()> {
    let dump_dir = Path::new("dump");

    for entry in fs::read_dir(dump_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("csv") {
            let playlist_name = path.file_stem().unwrap().to_str().unwrap();
            if playlist_name != "saved_tracks" {
                import_playlist(access_token, &path, playlist_name, force).await?;
            }
        }
    }

    if force {
        println!("All playlists have been imported.");
    }
    Ok(())
}

async fn import_playlist(
    access_token: &str,
    csv_path: &Path,
    playlist_name: &str,
    force: bool,
) -> Result<()> {
    let mut reader = Reader::from_path(csv_path)?;
    let track_uris: Vec<String> = reader
        .records()
        .filter_map(|result| {
            result.ok().and_then(|record| {
                record
                    .get(4)
                    .map(|track_id| format!("spotify:track:{}", track_id))
            })
        })
        .collect();

    if !force {
        println!(
            "Dry run: would have imported playlist '{}' with {} tracks.",
            playlist_name,
            track_uris.len()
        );
        return Ok(());
    }

    let client = reqwest::Client::new();

    // Create playlist
    let playlist_id = create_playlist(&client, access_token, playlist_name).await?;

    // Add tracks to the playlist in chunks
    for chunk in track_uris.chunks(100) {
        add_tracks_to_playlist(&client, access_token, &playlist_id, chunk).await?;
    }

    println!("Playlist '{}' has been imported.", playlist_name);
    Ok(())
}

async fn create_playlist(
    client: &reqwest::Client,
    access_token: &str,
    playlist_name: &str,
) -> Result<String> {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", access_token))?,
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let response = client
        .post("https://api.spotify.com/v1/me/playlists")
        .headers(headers)
        .json(&json!({
            "name": playlist_name,
            "description": "Imported playlist",
            "public": false
        }))
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(utils::response_error("Failed to create playlist", response).await);
    }

    let playlist: serde_json::Value = response.json().await?;
    Ok(playlist["id"].as_str().unwrap().to_string())
}

async fn add_tracks_to_playlist(
    client: &reqwest::Client,
    access_token: &str,
    playlist_id: &str,
    track_uris: &[String],
) -> Result<()> {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", access_token))?,
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    let url = format!("https://api.spotify.com/v1/playlists/{}/items", playlist_id);

    let response = client
        .post(&url)
        .headers(headers)
        .json(&json!({ "uris": track_uris }))
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(utils::response_error("Failed to add tracks to playlist", response).await);
    }

    println!("Added {} tracks to playlist", track_uris.len());
    Ok(())
}
