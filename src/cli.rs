use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::domain::{
    merge_provider_snapshot, LibraryState, MergeSummary, ProviderKind, ProviderLibrarySnapshot,
    SyncSummary,
};
use crate::provider::{ProviderCapability, StreamingProvider};
use crate::providers::spotify::SpotifyProvider;
use crate::providers::youtube_music::YoutubeMusicProvider;
use crate::{identity, providers, storage, web};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Clone)]
pub enum Commands {
    Export {
        #[clap(long, value_enum)]
        provider: ProviderKind,
        #[clap(long, action)]
        force: bool,
    },
    Import {
        #[clap(long, value_enum)]
        provider: ProviderKind,
        #[clap(long, action)]
        force: bool,
        #[clap(long, action)]
        reset: bool,
    },
    ExportCsv {
        #[clap(long)]
        output: Option<PathBuf>,
    },
    ResolveIdentities {
        #[clap(long, value_enum)]
        provider: Option<ProviderKind>,
        #[clap(long, action)]
        force: bool,
    },
    Ui {
        #[clap(long, default_value_t = 7878)]
        port: u16,
        #[clap(long, action)]
        no_open: bool,
    },
    Purge {
        #[clap(long, value_enum)]
        provider: ProviderKind,
        #[clap(long, action)]
        force: bool,
    },
    Sync {
        #[clap(long, value_enum)]
        from: ProviderKind,
        #[clap(long, value_enum)]
        to: ProviderKind,
        #[clap(long, action)]
        force: bool,
    },
}

pub async fn run() -> Result<()> {
    // Load environment variables from a local `.env` file once at startup so
    // every downstream provider sees them. `dotenv().ok()` is idempotent and
    // does not override variables that are already set, so the redundant
    // provider-level calls remain harmless.
    dotenvy::dotenv().ok();

    let cli = Cli::parse();

    if let Some(command) = &cli.command {
        match command {
            Commands::Export { provider, force } => {
                let provider = build_provider(*provider, ProviderCapability::Read).await?;
                let mut state = storage::read_library_state_or_new()?;

                if !*force {
                    println!(
                        "This is a dry run. The canonical library state will not be modified."
                    );
                    println!(
                        "Use the --force flag to merge the provider export into {}.",
                        storage::library_state_path().display()
                    );
                }

                println!(
                    "Exporting {} library into the canonical state...",
                    provider.kind()
                );
                let snapshot = export_and_track(provider.as_ref()).await?;
                let merge_summary = merge_provider_snapshot(&mut state, snapshot);

                if *force {
                    let output_path = storage::write_library_state(&state)?;
                    println!(
                        "Merged {} saved tracks and {} playlists from {} into {}.",
                        merge_summary.saved_tracks_seen,
                        merge_summary.playlists_seen,
                        provider.kind(),
                        output_path.display()
                    );
                } else {
                    println!(
                        "Dry run: would merge {} saved tracks and {} playlists from {} into the canonical state.",
                        merge_summary.saved_tracks_seen,
                        merge_summary.playlists_seen,
                        provider.kind()
                    );
                }

                print_merge_summary(&merge_summary, &state);
            }
            Commands::Import {
                provider,
                force,
                reset,
            } => {
                if *reset {
                    ensure_provider_supports_library_reset(*provider)?;
                }
                let capability = if *reset {
                    ProviderCapability::ReadWrite
                } else {
                    ProviderCapability::Write
                };
                let provider = build_provider(*provider, capability).await?;
                let mut state = storage::read_library_state()?;

                if !*force {
                    println!("This is a dry run. No destination account will be modified.");
                    println!("Use the --force flag to sync the canonical state into the provider.");
                }

                if *reset {
                    println!(
                        "Resetting {} destination before syncing canonical library state...",
                        provider.kind()
                    );
                    let purge_report = match provider.purge_library(*force).await {
                        Ok(report) => report,
                        Err(error) => {
                            remember_provider_failure(provider.kind(), &error);
                            return Err(error);
                        }
                    };
                    println!(
                        "Reset touched {} saved tracks and {} playlists.",
                        purge_report.saved_tracks, purge_report.playlists
                    );
                    state.clear_playlist_provider_state(provider.kind());
                }

                println!(
                    "Syncing canonical library state into {} ({} saved tracks, {} playlists)...",
                    provider.kind(),
                    state.saved_tracks.len(),
                    state.playlists.len()
                );

                let summary = if *force {
                    let summary =
                        sync_provider_and_persist(provider.as_ref(), &mut state, true).await?;
                    println!(
                        "Persisted {} sync results to {}.",
                        provider.kind(),
                        storage::library_state_path().display()
                    );
                    summary
                } else {
                    preview_sync(provider.as_ref(), &state).await?
                };

                print_sync_summary(provider.kind(), &summary);
            }
            Commands::ExportCsv { output } => {
                let state = storage::read_library_state()?;
                let output_path =
                    storage::export_csv(&storage::data_root(), &state, output.as_deref())?;
                println!(
                    "Exported normalized CSV tables from {} to {}.",
                    storage::library_state_path().display(),
                    output_path.display()
                );
            }
            Commands::ResolveIdentities { provider, force } => {
                let state = storage::read_library_state()?;
                let providers = provider
                    .map(|provider| vec![provider])
                    .unwrap_or_else(|| ProviderKind::all().to_vec());

                if !*force {
                    println!("This is a dry run. The canonical database will not be modified.");
                    println!(
                        "Use the --force flag to persist resolved identities into {}.",
                        storage::library_state_path().display()
                    );
                }

                let mut working = state.clone();
                for provider_kind in providers {
                    let provider_client =
                        build_provider(provider_kind, ProviderCapability::Read).await?;
                    println!(
                        "Resolving {} identities across the canonical library...",
                        provider_kind.display_name()
                    );
                    let result = identity::reconcile_provider_identities(
                        provider_client.as_ref(),
                        &mut working,
                        None,
                    )
                    .await;
                    match result {
                        Ok(summary) => print_identity_summary(&summary),
                        Err(error) if *force => {
                            remember_provider_failure(provider_kind, &error);
                            let path = storage::write_library_state(&working)?;
                            return Err(error.context(format!(
                                "Partial identity state was persisted to {}",
                                path.display()
                            )));
                        }
                        Err(error) => {
                            remember_provider_failure(provider_kind, &error);
                            return Err(error);
                        }
                    }
                }

                if *force {
                    let output_path = storage::write_library_state(&working)?;
                    println!(
                        "Persisted resolved identities to {}.",
                        output_path.display()
                    );
                }
            }
            Commands::Ui { port, no_open } => {
                web::serve(*port, !*no_open).await?;
            }
            Commands::Purge { provider, force } => {
                ensure_provider_supports_library_reset(*provider)?;
                let provider = build_provider(*provider, ProviderCapability::ReadWrite).await?;
                if !*force {
                    println!("This is a dry run. No account data will be deleted.");
                    println!("Use the --force flag to purge the selected provider.");
                }

                println!("Purging {} library...", provider.kind());
                let report = match provider.purge_library(*force).await {
                    Ok(report) => {
                        storage::clear_provider_cooldown(provider.kind())?;
                        report
                    }
                    Err(error) => {
                        remember_provider_failure(provider.kind(), &error);
                        return Err(error);
                    }
                };
                println!(
                    "Purge touched {} saved tracks and {} playlists.",
                    report.saved_tracks, report.playlists
                );
            }
            Commands::Sync { from, to, force } => {
                let source = build_provider(*from, ProviderCapability::Read).await?;
                let destination = build_provider(*to, ProviderCapability::Write).await?;
                let mut state = storage::read_library_state_or_new()?;

                if !*force {
                    println!("This is a dry run. No destination account will be modified.");
                    println!("Use the --force flag to merge the source export into the database and sync it into the destination provider.");
                }

                println!("Reading library from {}...", source.kind());
                let snapshot = export_and_track(source.as_ref()).await?;
                let merge_summary = merge_provider_snapshot(&mut state, snapshot);

                println!(
                    "Merged {} saved tracks and {} playlists from {} into the canonical state.",
                    merge_summary.saved_tracks_seen,
                    merge_summary.playlists_seen,
                    source.kind()
                );
                print_merge_summary(&merge_summary, &state);

                if *force {
                    let output_path = storage::write_library_state(&state)?;
                    println!(
                        "Persisted merged canonical database to {} before syncing {}.",
                        output_path.display(),
                        destination.kind()
                    );
                }

                println!(
                    "Syncing canonical state into {} ({} saved tracks, {} playlists)...",
                    destination.kind(),
                    state.saved_tracks.len(),
                    state.playlists.len()
                );

                let summary = if *force {
                    sync_provider_and_persist(destination.as_ref(), &mut state, true).await?
                } else {
                    preview_sync(destination.as_ref(), &state).await?
                };

                print_sync_summary(destination.kind(), &summary);
            }
        }
    } else {
        println!("No command specified. Use --help for usage information.");
    }

    Ok(())
}

async fn build_provider(
    provider: ProviderKind,
    capability: ProviderCapability,
) -> Result<Box<dyn StreamingProvider>> {
    ensure_provider_not_cooling_down(provider)?;
    match provider {
        ProviderKind::Spotify => {
            if let Some(connection) = storage::read_provider_connection(ProviderKind::Spotify)? {
                if let crate::domain::ProviderConnectionConfig::Spotify(config) = connection.config
                {
                    return Ok(Box::new(
                        SpotifyProvider::from_connection(&config, capability).await?,
                    ));
                }
            }
            Ok(Box::new(SpotifyProvider::new(capability).await?))
        }
        ProviderKind::YoutubeMusic => {
            if let Some(connection) = storage::read_provider_connection(ProviderKind::YoutubeMusic)?
            {
                if let crate::domain::ProviderConnectionConfig::YoutubeMusic(config) =
                    connection.config
                {
                    return Ok(Box::new(YoutubeMusicProvider::from_connection(&config)?));
                }
            }
            Ok(Box::new(YoutubeMusicProvider::new()?))
        }
    }
}

/// Export a provider's library while keeping cooldown bookkeeping in sync:
/// clear any cooldown on success, record a failure-driven cooldown on error.
async fn export_and_track(provider: &dyn StreamingProvider) -> Result<ProviderLibrarySnapshot> {
    match provider.export_library().await {
        Ok(snapshot) => {
            storage::clear_provider_cooldown(provider.kind())?;
            Ok(snapshot)
        }
        Err(error) => {
            remember_provider_failure(provider.kind(), &error);
            Err(error)
        }
    }
}

/// Run a non-persisting (dry-run) sync against a clone of `state`, recording a
/// failure-driven cooldown if the provider rejects the request.
async fn preview_sync(
    provider: &dyn StreamingProvider,
    state: &LibraryState,
) -> Result<SyncSummary> {
    let mut preview = state.clone();
    match provider.sync_library(&mut preview, false).await {
        Ok(summary) => Ok(summary),
        Err(error) => {
            remember_provider_failure(provider.kind(), &error);
            Err(error)
        }
    }
}

async fn sync_provider_and_persist(
    provider: &dyn StreamingProvider,
    state: &mut LibraryState,
    force: bool,
) -> Result<SyncSummary> {
    let result = provider.sync_library(state, force).await;

    if !force {
        return result;
    }

    match result {
        Ok(summary) => {
            storage::write_library_state(state).with_context(|| {
                format!(
                    "Provider sync finished but failed to persist {}",
                    storage::library_state_path().display()
                )
            })?;
            storage::clear_provider_cooldown(provider.kind())?;
            Ok(summary)
        }
        Err(sync_error) => {
            if let Some(cooldown) =
                providers::policy::cooldown_from_error(provider.kind(), &sync_error)
            {
                storage::save_provider_cooldown(&cooldown)?;
            }
            match storage::write_library_state(state) {
                Ok(path) => Err(sync_error.context(format!(
                    "Partial sync state was persisted to {}",
                    path.display()
                ))),
                Err(write_error) => Err(anyhow::anyhow!(
                    "{sync_error}\nAdditionally failed to persist partial sync state: {write_error}"
                )),
            }
        }
    }
}

fn ensure_provider_not_cooling_down(provider: ProviderKind) -> Result<()> {
    if let Some(cooldown) = storage::read_provider_cooldown(provider)? {
        anyhow::bail!(
            "{} is cooling down until {} because the provider recently rejected requests: {}",
            provider.display_name(),
            cooldown.blocked_until.to_rfc3339(),
            cooldown.reason
        );
    }
    Ok(())
}

fn ensure_provider_supports_library_reset(provider: ProviderKind) -> Result<()> {
    if provider.supports_library_reset() {
        return Ok(());
    }

    anyhow::bail!(
        "{} does not support account-wide library reset in this app. Normal pull and push are supported, but purge/reset is only enabled for providers with verified reset semantics.",
        provider.display_name()
    )
}

fn remember_provider_failure(provider: ProviderKind, error: &anyhow::Error) {
    let Some(cooldown) = providers::policy::cooldown_from_error(provider, error) else {
        return;
    };

    if let Err(save_error) = storage::save_provider_cooldown(&cooldown) {
        eprintln!(
            "Warning: failed to persist {} cooldown: {save_error}",
            provider.display_name()
        );
        return;
    }

    eprintln!(
        "{} will be held until {} to avoid hammering the provider.",
        provider.display_name(),
        cooldown.blocked_until.to_rfc3339()
    );
}

fn print_merge_summary(summary: &MergeSummary, state: &LibraryState) {
    println!(
        "Canonical state now contains {} tracks, {} saved tracks, {} playlists, and {} playlist entries.",
        state.track_count(),
        state.saved_tracks.len(),
        state.playlists.len(),
        state.playlist_entry_count()
    );
    if summary.tracks_created > 0 {
        println!(
            "Created {} new canonical tracks during this merge.",
            summary.tracks_created
        );
    }
    for warning in &summary.warnings {
        eprintln!("Warning: {warning}");
    }
}

fn print_sync_summary(provider: ProviderKind, summary: &SyncSummary) {
    println!(
        "{} sync resolved {} of {} saved tracks and {} of {} playlist tracks across {} playlists.",
        provider,
        summary.saved_tracks_synced,
        summary.saved_tracks_requested,
        summary.playlist_entries_synced,
        summary.playlist_entries_requested,
        summary.playlists_processed
    );

    if summary.saved_tracks_unmatched > 0 || summary.playlist_entries_unmatched > 0 {
        println!(
            "Unmatched items persisted in the canonical state: {} saved tracks, {} playlist entries.",
            summary.saved_tracks_unmatched,
            summary.playlist_entries_unmatched
        );
    }
    for warning in &summary.warnings {
        eprintln!("Warning: {warning}");
    }
}

fn print_identity_summary(summary: &identity::IdentityReconcileSummary) {
    let deferred = summary.unprocessed_due_rate_limit + summary.unprocessed_due_safety_limit;
    println!(
        "{} identity sync scanned {} tracks, performed {} provider identity lookups, found {} missing IDs, added {} links, merged {} duplicate track rows, skipped {} merge conflicts, flagged {} invalid metadata rows, removed {} duplicate saved rows, left {} unmatched, and deferred {} tracks for a later run.",
        summary.provider,
        summary.tracks_scanned,
        summary.provider_searches,
        summary.tracks_missing_provider_id,
        summary.provider_links_added,
        summary.tracks_merged,
        summary.merge_conflicts,
        summary.invalid_metadata,
        summary.duplicate_saved_tracks_removed,
        summary.unmatched,
        deferred
    );
    for warning in &summary.warnings {
        eprintln!("Warning: {warning}");
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::ProviderKind;

    use super::ensure_provider_supports_library_reset;

    #[test]
    fn destructive_library_reset_is_only_enabled_for_verified_providers() {
        assert!(ensure_provider_supports_library_reset(ProviderKind::Spotify).is_ok());

        let error = ensure_provider_supports_library_reset(ProviderKind::YoutubeMusic)
            .expect_err("YouTube Music should not expose account-wide reset");
        assert!(error.to_string().contains("YouTube Music"));
        assert!(error.to_string().contains("pull and push are supported"));
    }
}
