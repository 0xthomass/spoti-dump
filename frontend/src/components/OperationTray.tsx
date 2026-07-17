import type { TrackedOperation } from '../hooks/useOperationTracker'
import { operationDisplayTitle } from '../lib/format'

function chipPercent(operation: TrackedOperation) {
  const snapshot = operation.snapshot
  if (!snapshot || snapshot.saved_tracks_total === null) {
    return null
  }
  if (snapshot.saved_tracks_total <= 0) {
    return null
  }
  return Math.min(
    100,
    Math.round((snapshot.saved_tracks_done / snapshot.saved_tracks_total) * 100),
  )
}

export function OperationTray({
  operations,
  onOpen,
}: {
  operations: TrackedOperation[]
  onOpen: (operationId: string) => void
}) {
  if (operations.length === 0) {
    return null
  }

  return (
    <div className="op-tray">
      {operations.map((operation) => {
        const title = operation.snapshot
          ? operationDisplayTitle(operation.snapshot)
          : 'Starting operation'
        const percent = chipPercent(operation)
        return (
          <button
            aria-label={`Reopen ${title} progress`}
            className="op-tray-chip"
            key={operation.id}
            onClick={() => onOpen(operation.id)}
            type="button"
          >
            <span className="spinner spinner--sm" aria-hidden="true" />
            <span className="op-tray-chip__title">{title}</span>
            {percent !== null ? (
              <span className="op-tray-chip__pct">{percent}%</span>
            ) : null}
          </button>
        )
      })}
    </div>
  )
}
