import type { PlaylistEntry, SavedTrackItem, TrackItem } from '../api/types'
import { Artwork } from './Artwork'
import { ProviderChipRow } from './ProviderChipRow'
import { StatusChipRow } from './StatusChipRow'
import { IconButton } from './IconButton'
import { EditIcon, TrashIcon } from './icons'

export function TrackList<T extends SavedTrackItem | TrackItem | PlaylistEntry>({
  items,
  showAdded,
  usageMode,
  onEdit,
  onDelete,
}: {
  items: T[]
  showAdded?: boolean
  usageMode?: boolean
  onEdit: (item: T) => void
  onDelete: (item: T) => void
}) {
  return (
    <div className="track-list">
      {items.map((item) => (
        <article className="track-row" key={'saved_track_id' in item ? item.saved_track_id : 'entry_id' in item ? item.entry_id : item.track_id}>
          <div className="track-row-main">
            <Artwork
              image={item.artwork_url}
              seed={item.track_id}
              size="row"
              title={item.title}
            />
            <div className="track-text">
              <div className="track-title-line">
                <strong>{item.title}</strong>
                {item.coverage ? (
                  <span className="meta-badge meta-badge--coverage">
                    {item.coverage.short_label}
                  </span>
                ) : null}
              </div>
              <div className="track-subline">{item.subtitle || item.artist_summary}</div>
            </div>
          </div>

          <div className="track-row-meta">
            {'providers' in item ? <ProviderChipRow providers={item.providers} /> : null}
            {'status_pills' in item ? <StatusChipRow pills={item.status_pills} /> : null}
            {showAdded && 'added_label' in item ? (
              <span className="track-row-date">{item.added_label}</span>
            ) : null}
            {usageMode && 'saved_count' in item && 'playlist_refs' in item ? (
              <div className="usage-pills">
                <span className="mini-chip">{item.saved_count} saved</span>
                <span className="mini-chip">{item.playlist_refs} playlist refs</span>
              </div>
            ) : null}
            {!showAdded && 'duration_label' in item ? (
              <span className="track-row-duration">{item.duration_label}</span>
            ) : null}
          </div>

          <div className="track-row-actions">
            <IconButton label="Edit item" onClick={() => onEdit(item)}>
              <EditIcon />
            </IconButton>
            <IconButton destructive label="Remove item" onClick={() => onDelete(item)}>
              <TrashIcon />
            </IconButton>
          </div>
        </article>
      ))}
    </div>
  )
}
