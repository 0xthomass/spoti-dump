import type { TrackedOperation } from '../../hooks/useOperationTracker'
import { operationDisplayTitle } from '../../lib/format'
import { ErrorState } from '../ErrorState'
import { LoadingState } from '../LoadingState'
import { ProgressCard } from '../ProgressCard'
import { ModalFrame } from './ModalFrame'

export function OperationModal({
  operation,
  onClose,
}: {
  operation: TrackedOperation | undefined
  onClose: () => void
}) {
  const snapshot = operation?.snapshot ?? null
  const running = !snapshot || snapshot.status === 'running'
  const title = snapshot ? operationDisplayTitle(snapshot) : 'Working'
  const primaryProgressLabel =
    snapshot?.kind === 'identity' || snapshot?.kind === 'identity_all'
      ? 'Tracks'
      : 'Saved tracks'

  return (
    <ModalFrame title={title} onClose={onClose}>
      {operation?.error ? (
        <div className="modal-stack">
          <ErrorState message={operation.error} compact />
          <div className="modal-actions">
            <button className="btn btn--primary" onClick={onClose} type="button">
              Close
            </button>
          </div>
        </div>
      ) : !snapshot ? (
        <div className="modal-stack">
          <LoadingState label="Starting operation" compact />
          <div className="modal-actions">
            <button className="btn btn--ghost" onClick={onClose} type="button">
              Run in background
            </button>
          </div>
        </div>
      ) : (
        <div className="modal-stack">
          <div className="operation-head">
            <span className="eyebrow">
              {snapshot.status === 'running'
                ? 'In progress'
                : snapshot.status === 'succeeded'
                  ? 'Complete'
                  : 'Failed'}
            </span>
            <strong>{snapshot.stage}</strong>
            {snapshot.detail ? <p>{snapshot.detail}</p> : null}
          </div>

          <div className="operation-grid">
            <ProgressCard
              label={primaryProgressLabel}
              done={snapshot.saved_tracks_done}
              total={snapshot.saved_tracks_total}
              running={running}
            />
            <ProgressCard
              label="Playlists"
              done={snapshot.playlists_done}
              total={snapshot.playlists_total}
              running={running}
            />
            <ProgressCard
              label="Playlist tracks"
              done={snapshot.playlist_entries_done}
              total={snapshot.playlist_entries_total}
              running={running}
            />
          </div>

          {snapshot.message ? (
            <div className="confirm-copy">
              <p>{snapshot.message}</p>
            </div>
          ) : null}

          {snapshot.error ? (
            <div className="confirm-warning confirm-warning--danger">
              <strong>Operation failed</strong>
              <span>{snapshot.error}</span>
            </div>
          ) : null}

          {snapshot.warnings.length ? (
            <div className="confirm-warning confirm-warning--warning">
              <strong>Warnings</strong>
              <ul className="operation-warning-list">
                {snapshot.warnings.map((warning) => (
                  <li key={warning}>{warning}</li>
                ))}
              </ul>
            </div>
          ) : null}

          <div className="modal-actions">
            {running ? (
              <button className="btn btn--ghost" onClick={onClose} type="button">
                Run in background
              </button>
            ) : (
              <button className="btn btn--primary" onClick={onClose} type="button">
                Close
              </button>
            )}
          </div>
        </div>
      )}
    </ModalFrame>
  )
}
