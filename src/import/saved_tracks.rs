use anyhow::{Context, Result};
use csv::Reader;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use std::path::Path;

use crate::utils;

pub async fn import_saved_tracks(access_token: &str, force: bool) -> Result<()> {
    let dump_dir = Path::new("dump");
    let input_file = dump_dir.join("saved_tracks.csv");

    let mut reader = Reader::from_path(&input_file)
        .with_context(|| format!("Failed to open CSV file: {}", input_file.to_str().unwrap()))?;

    let track_uris: Vec<String> = reader
        .records()
        .filter_map(|result| {
            result.ok().and_then(|record| {
                record
                    .get(4)
                    .map(|track_id| format!("spotify:track:{track_id}"))
            })
        })
        .collect();

    if !force {
        println!(
            "Dry run: would have imported {} saved tracks.",
            track_uris.len()
        );
        return Ok(());
    }

    let client = reqwest::Client::new();

    for chunk in track_uris.chunks(40) {
        save_tracks(&client, access_token, chunk).await?;
    }

    println!("All saved tracks have been imported.");
    Ok(())
}

async fn save_tracks(
    client: &reqwest::Client,
    access_token: &str,
    track_uris: &[String],
) -> Result<()> {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", access_token))?,
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(CONTENT_LENGTH, HeaderValue::from_static("0"));

    let response = client
        .put("https://api.spotify.com/v1/me/library")
        .headers(headers)
        .query(&[("uris", track_uris.join(","))])
        .body(String::new())
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(utils::response_error("Failed to save tracks", response).await);
    }

    println!("Saved {} tracks", track_uris.len());
    Ok(())
}
