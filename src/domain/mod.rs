pub mod library;
pub mod merge;
pub mod mutate;
pub mod playlist;
pub mod provider;
pub mod snapshot;
pub mod sync;
pub mod track;

pub use library::{LibraryState, SavedTrackEntry, LIBRARY_STATE_FORMAT_VERSION};
pub use merge::merge_provider_snapshot;
pub use mutate::{
    apply_push_outcome, new_canonical_id, ResolvedTrackMergeConflict, TrackIdentityApplyResult,
    TrackMergeConflictResolution, TrackMergeResult,
};
pub use playlist::{PlaylistEntity, PlaylistEntry, ProviderPlaylistLink};
pub use provider::{
    ProviderConnection, ProviderConnectionConfig, ProviderCooldown, ProviderHealth, ProviderKind,
    SpotifyConnectionConfig, YoutubeMusicConnectionConfig,
};
pub use snapshot::{
    ObservedArtwork, ObservedPlaylist, ObservedPlaylistTrack, ObservedSavedTrack, ObservedTrack,
    ProviderLibrarySnapshot,
};
pub use sync::{
    MergeSummary, NewProviderLink, PlaylistEntrySyncTarget, PlaylistSyncTarget, PurgeReport,
    PushEntryResult, PushItemResult, PushMode, PushOutcome, PushPlan, PushPlaylistResult,
    SavedTrackSyncTarget, SyncState, SyncStatusRecord, SyncSummary, TrackIdentityMatch,
};
pub use track::{
    IdentityConflictStatus, LinkSource, ProviderTrackArtwork, ProviderTrackLink, TrackEntity,
    TrackIdentityConflict, TrackMetadata,
};
