//! Normalized CSV export. One file per table, byte-compatible with the previous
//! implementation (including `track_identity_conflicts.csv`).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use csv::Writer;

use crate::domain::{LibraryState, SyncStatusRecord};

use super::{default_csv_export_path_in, encode_datetime};

pub fn export_csv(root: &Path, state: &LibraryState, output_dir: Option<&Path>) -> Result<PathBuf> {
    let output_dir = output_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_csv_export_path_in(root));

    if output_dir.exists() {
        for entry in fs::read_dir(&output_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("csv") {
                fs::remove_file(&path)
                    .with_context(|| format!("Failed to remove stale {}", path.display()))?;
            }
        }
    } else {
        fs::create_dir_all(&output_dir)
            .with_context(|| format!("Failed to create {}", output_dir.display()))?;
    }

    write_metadata_csv(&output_dir, state)?;
    write_tracks_csv(&output_dir, state)?;
    write_track_artists_csv(&output_dir, state)?;
    write_track_provider_links_csv(&output_dir, state)?;
    write_track_provider_artwork_csv(&output_dir, state)?;
    write_track_provider_status_csv(&output_dir, state)?;
    write_track_identity_conflicts_csv(&output_dir, state)?;
    write_saved_tracks_csv(&output_dir, state)?;
    write_saved_track_provider_status_csv(&output_dir, state)?;
    write_playlists_csv(&output_dir, state)?;
    write_playlist_provider_links_csv(&output_dir, state)?;
    write_playlist_provider_status_csv(&output_dir, state)?;
    write_playlist_entries_csv(&output_dir, state)?;
    write_playlist_entry_provider_status_csv(&output_dir, state)?;

    Ok(output_dir)
}

fn write_metadata_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("library_metadata.csv"))?;
    writer.write_record(["key", "value"])?;
    writer.write_record(["schema_version", &state.format_version.to_string()])?;
    writer.write_record(["created_at", &encode_datetime(&state.created_at)])?;
    writer.write_record(["updated_at", &encode_datetime(&state.updated_at)])?;
    writer.flush()?;
    Ok(())
}

fn write_tracks_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("tracks.csv"))?;
    writer.write_record(["id", "title", "album", "duration_seconds", "isrc"])?;
    for track in &state.tracks {
        writer.write_record(vec![
            track.id.clone(),
            track.metadata.title.clone(),
            track.metadata.album.clone().unwrap_or_default(),
            optional_number(track.metadata.duration_seconds),
            track.metadata.isrc.clone().unwrap_or_default(),
        ])?;
    }
    writer.flush()?;
    Ok(())
}

fn write_track_artists_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("track_artists.csv"))?;
    writer.write_record(["track_id", "position", "name"])?;
    for track in &state.tracks {
        for (position, artist) in track.metadata.artists.iter().enumerate() {
            writer.write_record([track.id.as_str(), &position.to_string(), artist.as_str()])?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn write_track_provider_links_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("track_provider_links.csv"))?;
    writer.write_record([
        "track_id",
        "provider",
        "provider_id",
        "source",
        "confidence",
        "linked_at",
        "last_seen_at",
    ])?;
    for track in &state.tracks {
        for (provider, link) in &track.provider_links {
            writer.write_record(vec![
                track.id.clone(),
                provider.clone(),
                link.provider_id.clone(),
                link.source.as_str().to_string(),
                optional_float(link.confidence),
                encode_datetime(&link.linked_at),
                optional_datetime(link.last_seen_at.as_ref()),
            ])?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn write_track_provider_artwork_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("track_provider_artwork.csv"))?;
    writer.write_record([
        "track_id",
        "provider",
        "url",
        "width",
        "height",
        "last_seen_at",
    ])?;
    for track in &state.tracks {
        for (provider, artwork) in &track.provider_artwork {
            writer.write_record(vec![
                track.id.clone(),
                provider.clone(),
                artwork.url.clone(),
                optional_number(artwork.width),
                optional_number(artwork.height),
                optional_datetime(artwork.last_seen_at.as_ref()),
            ])?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn write_track_provider_status_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("track_provider_status.csv"))?;
    writer.write_record(status_header("track_id"))?;
    for track in &state.tracks {
        write_status_rows(&mut writer, "track_id", &track.id, &track.provider_state)?;
    }
    writer.flush()?;
    Ok(())
}

fn write_track_identity_conflicts_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("track_identity_conflicts.csv"))?;
    writer.write_record([
        "track_id",
        "provider",
        "candidate_provider_id",
        "confidence",
        "detected_at",
        "status",
        "rejected_at",
    ])?;
    for track in &state.tracks {
        for conflict in &track.identity_conflicts {
            writer.write_record(vec![
                track.id.clone(),
                conflict.provider.as_key().to_string(),
                conflict.candidate_provider_id.clone(),
                optional_float(conflict.confidence),
                encode_datetime(&conflict.detected_at),
                conflict.status.as_str().to_string(),
                optional_datetime(conflict.rejected_at.as_ref()),
            ])?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn write_saved_tracks_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("saved_tracks.csv"))?;
    writer.write_record(["id", "track_id", "added_at"])?;
    for saved_track in &state.saved_tracks {
        writer.write_record([
            saved_track.id.as_str(),
            saved_track.track_id.as_str(),
            optional_str(saved_track.added_at.as_deref()),
        ])?;
    }
    writer.flush()?;
    Ok(())
}

fn write_saved_track_provider_status_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("saved_track_provider_status.csv"))?;
    writer.write_record(status_header("saved_track_id"))?;
    for saved_track in &state.saved_tracks {
        write_status_rows(
            &mut writer,
            "saved_track_id",
            &saved_track.id,
            &saved_track.provider_state,
        )?;
    }
    writer.flush()?;
    Ok(())
}

fn write_playlists_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("playlists.csv"))?;
    writer.write_record(["id", "name", "description"])?;
    for playlist in &state.playlists {
        writer.write_record([
            playlist.id.as_str(),
            playlist.name.as_str(),
            optional_str(playlist.description.as_deref()),
        ])?;
    }
    writer.flush()?;
    Ok(())
}

fn write_playlist_provider_links_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("playlist_provider_links.csv"))?;
    writer.write_record([
        "playlist_id",
        "provider",
        "provider_id",
        "source",
        "confidence",
        "linked_at",
        "last_seen_at",
    ])?;
    for playlist in &state.playlists {
        for (provider, link) in &playlist.provider_links {
            writer.write_record(vec![
                playlist.id.clone(),
                provider.clone(),
                link.provider_id.clone(),
                link.source.as_str().to_string(),
                optional_float(link.confidence),
                encode_datetime(&link.linked_at),
                optional_datetime(link.last_seen_at.as_ref()),
            ])?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn write_playlist_provider_status_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("playlist_provider_status.csv"))?;
    writer.write_record(status_header("playlist_id"))?;
    for playlist in &state.playlists {
        write_status_rows(
            &mut writer,
            "playlist_id",
            &playlist.id,
            &playlist.provider_state,
        )?;
    }
    writer.flush()?;
    Ok(())
}

fn write_playlist_entries_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("playlist_entries.csv"))?;
    writer.write_record(["id", "playlist_id", "position", "track_id", "added_at"])?;
    for playlist in &state.playlists {
        for (position, entry) in playlist.entries.iter().enumerate() {
            writer.write_record([
                entry.id.as_str(),
                playlist.id.as_str(),
                &position.to_string(),
                entry.track_id.as_str(),
                optional_str(entry.added_at.as_deref()),
            ])?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn write_playlist_entry_provider_status_csv(output_dir: &Path, state: &LibraryState) -> Result<()> {
    let mut writer = csv_writer(output_dir.join("playlist_entry_provider_status.csv"))?;
    writer.write_record(status_header("entry_id"))?;
    for playlist in &state.playlists {
        for entry in &playlist.entries {
            write_status_rows(&mut writer, "entry_id", &entry.id, &entry.provider_state)?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn csv_writer(path: PathBuf) -> Result<Writer<std::fs::File>> {
    Writer::from_path(&path).with_context(|| format!("Failed to write {}", path.display()))
}

fn status_header(owner_column: &'static str) -> [&'static str; 9] {
    [
        owner_column,
        "provider",
        "state",
        "message",
        "confidence",
        "provider_item_id",
        "last_attempt_at",
        "last_success_at",
        "last_seen_at",
    ]
}

fn write_status_rows(
    writer: &mut Writer<std::fs::File>,
    _owner_column: &str,
    owner_id: &str,
    statuses: &BTreeMap<String, SyncStatusRecord>,
) -> Result<()> {
    for (provider, status) in statuses {
        writer.write_record(vec![
            owner_id.to_string(),
            provider.clone(),
            status.state.as_str().to_string(),
            status.message.clone().unwrap_or_default(),
            optional_float(status.confidence),
            status.provider_item_id.clone().unwrap_or_default(),
            optional_datetime(status.last_attempt_at.as_ref()),
            optional_datetime(status.last_success_at.as_ref()),
            optional_datetime(status.last_seen_at.as_ref()),
        ])?;
    }
    Ok(())
}

fn optional_str(value: Option<&str>) -> &str {
    value.unwrap_or("")
}

fn optional_number(value: Option<u32>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn optional_float(value: Option<f64>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn optional_datetime(value: Option<&DateTime<Utc>>) -> String {
    value.map(encode_datetime).unwrap_or_default()
}
