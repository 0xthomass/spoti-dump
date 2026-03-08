pub mod access_token;
pub mod export;
pub mod import;
pub mod purge;
pub mod types;
pub mod utils;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Clone)]
pub enum Commands {
    Export {
        #[clap(long, action)]
        force: bool,
    },
    Import {
        #[clap(long, action)]
        force: bool,
    },
    Purge {
        #[clap(long, action)]
        force: bool,
    },
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();

    if let Some(command) = &cli.command {
        let access_token = access_token::get_access_token(command.clone()).await?;

        match command {
            Commands::Export { force } => {
                if !*force {
                    println!("This is a dry run. No tracks will be exported.");
                    println!("Use the --force flag to export tracks.");
                }
                println!("Exporting tracks and playlists...");
                export::saved_tracks::export_saved_tracks(&access_token, *force).await?;
                export::playlists::export_playlists(&access_token, *force).await?;
                if *force {
                    println!("Export completed successfully.");
                }
            }
            Commands::Import { force } => {
                if !*force {
                    println!("This is a dry run. No tracks will be imported.");
                    println!("Use the --force flag to import tracks.");
                }
                println!("Importing tracks and playlists...");
                import::saved_tracks::import_saved_tracks(&access_token, *force).await?;
                import::playlists::import_playlists(&access_token, *force).await?;
                if *force {
                    println!("Import completed successfully.");
                }
            }
            Commands::Purge { force } => {
                if !*force {
                    println!("This is a dry run. No tracks will be deleted.");
                    println!("Use the --force flag to delete tracks.");
                }

                println!("Purging tracks and playlists...");
                let _ = purge::saved_tracks::purge_saved_tracks(&access_token, *force).await?;
                let _ = purge::playlists::purge_playlists(&access_token, *force).await?;
                if *force {
                    println!("Purge completed successfully.");
                }
            }
        }
    } else {
        println!("No command specified. Use --help for usage information.");
    }

    Ok(())
}
