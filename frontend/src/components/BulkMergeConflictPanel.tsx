import type { BulkMergeIdentityConflictsPlan } from '../api/types'
import { formatNumber } from '../lib/format'

export function BulkMergeConflictPanel({
  plan,
  loading,
  error,
  running,
  onMerge,
}: {
  plan: BulkMergeIdentityConflictsPlan | null
  loading: boolean
  error: string | null
  running: string | null
  onMerge: (resolution: 'keep_source' | 'keep_target') => void
}) {
  if (loading && !plan) {
    return (
      <div className="conflict-bulk-panel">
        <strong>Planning likely-same bulk merge…</strong>
      </div>
    )
  }

  if (error) {
    return (
      <div className="conflict-bulk-panel conflict-bulk-panel--warning">
        <strong>Bulk merge plan failed</strong>
        <p>{error}</p>
      </div>
    )
  }

  if (!plan || plan.eligible_count === 0) {
    return (
      <div className="conflict-bulk-panel">
        <strong>No likely-same bulk merges in these filters</strong>
        <p>
          Bulk merge only considers conflicts classified as likely same recording. Adjust the
          search, provider, or impact filter to review another subset.
        </p>
      </div>
    )
  }

  return (
    <div className="conflict-bulk-panel">
      <div className="conflict-bulk-head">
        <div>
          <span className="eyebrow">Bulk Safety Plan</span>
          <strong>{formatNumber(plan.eligible_count)} likely-same conflict(s)</strong>
          <p>
            The action creates a manual backup, re-checks every row before merging, and never
            changes Spotify or YouTube Music accounts.
          </p>
        </div>
        <div className="modal-actions modal-actions--inline">
          <button
            className="btn btn--secondary"
            disabled={running !== null}
            onClick={() => onMerge('keep_source')}
            type="button"
          >
            {running === 'keep_source' ? 'Merging…' : 'Bulk merge, keep source IDs'}
          </button>
          <button
            className="btn btn--ghost"
            disabled={running !== null}
            onClick={() => onMerge('keep_target')}
            type="button"
          >
            {running === 'keep_target' ? 'Merging…' : 'Bulk merge, keep candidate IDs'}
          </button>
        </div>
      </div>
      <div className="chip-row">
        {plan.warnings.map((warning) => (
          <span className="mini-chip mini-chip--warning" key={warning}>
            {warning}
          </span>
        ))}
      </div>
      {plan.examples.length > 0 ? (
        <div className="push-plan-examples">
          <strong>First examples</strong>
          {plan.examples.slice(0, 5).map((item) => (
            <span key={`${item.source_track.track_id}:${item.conflict.provider_id}`}>
              {item.source_track.title} → {item.conflict.owner_track.title}
            </span>
          ))}
        </div>
      ) : null}
    </div>
  )
}
