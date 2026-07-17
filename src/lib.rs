pub mod cli;
pub mod domain;
pub mod error;
pub mod identity;
pub mod matching;
pub mod provider;
pub mod providers;
pub mod storage;
pub mod web;

/// Thin re-export so `main.rs` and any external callers keep working after the
/// CLI implementation moved into [`cli`].
pub async fn run() -> anyhow::Result<()> {
    cli::run().await
}
