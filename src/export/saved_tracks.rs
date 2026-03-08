use anyhow::{Context, Result};
use csv::Writer;
use std::fs;
use std::path::Path;

use crate::types::Track;
use crate::utils;

#[derive(Debug, serde::Deserialize)]
struct SavedTrack {
    added_at: Option<String>,
    track: Option<Track>,
}

pub async fn export_saved_tracks(access_token: &String, force: bool) -> Result<()> {
    let tracks: Vec<SavedTrack> =
        utils::get_all_items(access_token, "https://api.spotify.com/v1/me/tracks").await?;

    if !force {
        println!(
            "Dry run: would have exported {} saved tracks.",
            tracks.len()
        );
        return Ok(());
    }

    let dump_dir = Path::new("dump");
    if !dump_dir.exists() {
        fs::create_dir(dump_dir).context("Failed to create dump directory")?;
    }

    let output_file = dump_dir.join("saved_tracks.csv");
    let mut writer = Writer::from_path(&output_file).with_context(|| {
        format!(
            "Failed to create CSV file: {}",
            output_file.to_str().unwrap()
        )
    })?;

    writer.write_record(&["Added At", "Track Name", "Artists", "Album", "Id"])?;

    let mut skipped_tracks_count = 0;

    for track in tracks {
        if let Some(track_data) = track.track {
            let Track {
                id,
                name,
                artists,
                album,
            } = track_data;

            if let Some(track_id) = id {
                let added_at = track
                    .added_at
                    .clone()
                    .unwrap_or_else(|| "Unknown".to_string());
                let track_name = if name.is_empty() {
                    "Unknown".to_string()
                } else {
                    name
                };
                let artist_names: Vec<String> = artists
                    .into_iter()
                    .filter_map(|artist| {
                        let trimmed = artist.name.trim().to_string();
                        if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed)
                        }
                    })
                    .collect();
                let artists_joined = if artist_names.is_empty() {
                    "Unknown".to_string()
                } else {
                    artist_names.join(", ")
                };
                let album_name = if album.name.is_empty() {
                    "Unknown".to_string()
                } else {
                    album.name
                };

                writer.write_record(&[
                    added_at.as_str(),
                    track_name.as_str(),
                    artists_joined.as_str(),
                    album_name.as_str(),
                    track_id.as_str(),
                ])?;
            } else {
                skipped_tracks_count += 1;
            }
        } else {
            skipped_tracks_count += 1;
        }
    }

    writer.flush()?;
    println!(
        "Saved tracks have been exported to {}",
        output_file.to_str().unwrap()
    );

    if skipped_tracks_count > 0 {
        println!("Skipped {} saved tracks.", skipped_tracks_count);
    }

    Ok(())
}
