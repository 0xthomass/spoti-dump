use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::model::{
    LibraryState, ProviderKind, ProviderLibrarySnapshot, PurgeReport, SyncSummary, TrackMetadata,
};

#[derive(Clone, Copy, Debug)]
pub enum ProviderCapability {
    Read,
    Write,
    ReadWrite,
}

#[derive(Clone, Debug, Default)]
pub struct ProviderProgress {
    pub stage: String,
    pub detail: Option<String>,
    pub saved_tracks_done: usize,
    pub saved_tracks_total: Option<usize>,
    pub playlists_done: usize,
    pub playlists_total: Option<usize>,
    pub playlist_entries_done: usize,
    pub playlist_entries_total: Option<usize>,
}

pub type ProgressHandler = Arc<dyn Fn(ProviderProgress) + Send + Sync>;

#[async_trait]
pub trait StreamingProvider: Send + Sync {
    fn kind(&self) -> ProviderKind;

    async fn verify_connection(&self) -> Result<()>;

    async fn export_library_with_progress(
        &self,
        progress: Option<ProgressHandler>,
    ) -> Result<ProviderLibrarySnapshot>;

    async fn export_library(&self) -> Result<ProviderLibrarySnapshot> {
        self.export_library_with_progress(None).await
    }

    async fn sync_library_with_progress(
        &self,
        state: &mut LibraryState,
        force: bool,
        progress: Option<ProgressHandler>,
    ) -> Result<SyncSummary>;

    async fn sync_library(&self, state: &mut LibraryState, force: bool) -> Result<SyncSummary> {
        self.sync_library_with_progress(state, force, None).await
    }

    async fn resolve_track_identity(
        &self,
        metadata: &TrackMetadata,
    ) -> Result<Option<(String, f64)>>;

    async fn purge_library(&self, force: bool) -> Result<PurgeReport>;

    async fn remove_saved_track(&self, provider_track_id: &str) -> Result<()>;

    async fn delete_playlist(&self, provider_playlist_id: &str) -> Result<()>;
}
