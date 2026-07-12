import type { ProviderPushPlan } from '../api/types'
import { ReadinessItem } from './ReadinessItem'

export function PushPlanSummary({ plan }: { plan: ProviderPushPlan }) {
  return (
    <div className="push-plan">
      <div className="readiness-head">
        <span>Push plan</span>
        <strong>{plan.provider_name}</strong>
      </div>
      <div className="readiness-grid">
        <ReadinessItem
          label="Saved tracks"
          ready={plan.saved_tracks.pushable}
          blocked={plan.saved_tracks.skipped_missing_identity}
        />
        <ReadinessItem
          label="Playlist entries"
          ready={plan.playlist_entries.pushable}
          blocked={plan.playlist_entries.skipped_missing_identity}
        />
        <ReadinessItem
          label="Playlists"
          ready={plan.playlists.linked}
          blocked={plan.playlists.unlinked}
        />
      </div>
      {plan.saved_tracks.skipped_examples.length > 0 ? (
        <div className="push-plan-examples">
          <strong>Skipped saved-track examples</strong>
          {plan.saved_tracks.skipped_examples.slice(0, 3).map((track) => (
            <span key={`saved-${track.track_id}`}>
              {track.title || 'Untitled'} · {track.artist_summary || 'Unknown artist'}
            </span>
          ))}
        </div>
      ) : null}
      {plan.playlists.examples.length > 0 ? (
        <div className="push-plan-examples">
          <strong>Playlist risks</strong>
          {plan.playlists.examples.slice(0, 3).map((playlist) => (
            <span key={playlist.playlist_id}>
              {playlist.name}: {playlist.linked ? 'linked' : 'unlinked'}, {playlist.missing_entries}{' '}
              missing entries
            </span>
          ))}
        </div>
      ) : null}
    </div>
  )
}
