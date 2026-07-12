import type { ConflictTrack } from '../api/types'
import { Artwork } from './Artwork'
import { ProviderChipRow } from './ProviderChipRow'

export function ConflictTrackCard({
  label,
  track,
  onEdit,
}: {
  label: string
  track: ConflictTrack
  onEdit: () => void
}) {
  return (
    <div className="conflict-track-card">
      <div className="conflict-track-main">
        <Artwork image={track.artwork_url} seed={track.track_id} size="row" title={track.title} />
        <div className="track-text">
          <span className="eyebrow">{label}</span>
          <strong>{track.title}</strong>
          <span>{track.artist_summary}</span>
          {track.album ? <span>{track.album}</span> : null}
        </div>
      </div>
      <div className="chip-row">
        <span className="meta-badge meta-badge--coverage">{track.coverage.short_label}</span>
        <span className="mini-chip">{track.saved_count} saved</span>
        <span className="mini-chip">{track.playlist_refs} playlist refs</span>
      </div>
      <ProviderChipRow providers={track.providers} />
      <button className="btn btn--ghost" onClick={onEdit} type="button">
        Open row
      </button>
    </div>
  )
}
