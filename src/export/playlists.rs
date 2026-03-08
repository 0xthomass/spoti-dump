use anyhow::{Context, Result};
use csv::Writer;
use rand::Rng;
use std::fs;
use std::path::Path;

use crate::types::Track;
use crate::utils;

#[derive(Debug, serde::Deserialize)]
struct Playlist {
    id: String,
    name: String,
}

#[derive(Debug, serde::Deserialize)]
struct PlaylistItem {
    added_at: Option<String>,
    #[serde(alias = "track")]
    item: Option<Track>,
}

pub async fn export_playlists(access_token: &String, force: bool) -> Result<()> {
    let playlists: Vec<Playlist> =
        utils::get_all_items(access_token, "https://api.spotify.com/v1/me/playlists").await?;
    let mut total_skipped_tracks = 0;

    let dump_dir = Path::new("dump");
    if force && !dump_dir.exists() {
        fs::create_dir(dump_dir).context("Failed to create dump directory")?;
    }

    for playlist in playlists {
        total_skipped_tracks +=
            export_playlist(access_token, &playlist.id, &playlist.name, dump_dir, force).await?;
    }

    if force {
        println!("All playlists have been exported.");
        if total_skipped_tracks > 0 {
            println!("Skipped {} tracks in playlists.", total_skipped_tracks);
        }
    }
    Ok(())
}

async fn export_playlist(
    access_token: &str,
    playlist_id: &str,
    playlist_name: &str,
    dump_dir: &Path,
    force: bool,
) -> Result<u32> {
    let url = format!("https://api.spotify.com/v1/playlists/{}/items", playlist_id);
    let tracks: Vec<PlaylistItem> = utils::get_all_items(access_token, &url).await?;

    if !force {
        println!(
            "Dry run: would have exported playlist '{}' with {} tracks.",
            playlist_name,
            tracks.len()
        );
        return Ok(0);
    }

    let sanitized_name = sanitize_filename(playlist_name);
    let output_file = dump_dir.join(format!("{}.csv", sanitized_name));
    let mut writer = Writer::from_path(&output_file)
        .with_context(|| format!("Failed to create CSV file: {:?}", output_file))?;

    writer.write_record(&["Added At", "Track Name", "Artists", "Album", "Id"])?;

    let mut skipped_tracks_count = 0;

    for item in tracks {
        if let Some(track_data) = item.item {
            let Track {
                id,
                name,
                artists,
                album,
            } = track_data;

            if let Some(track_id) = id {
                let added_at = item
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
        "Playlist '{}' has been exported to {}",
        playlist_name,
        output_file.to_str().unwrap()
    );

    Ok(skipped_tracks_count)
}

fn sanitize_filename(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .filter_map(|c| {
            if c.is_alphanumeric() {
                Some(c)
            } else if c == ' ' {
                Some('_')
            } else {
                None
            }
        })
        .collect();

    if sanitized.is_empty() {
        generate_random_name()
    } else {
        sanitized
    }
}

fn generate_random_name() -> String {
    let mut rng = rand::thread_rng();
    let random_name: String = (0..10)
        .map(|_| rng.sample(rand::distributions::Alphanumeric) as char)
        .collect();
    random_name
}
