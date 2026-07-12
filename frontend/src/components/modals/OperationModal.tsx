import { useEffect, useRef, useState } from 'react'
import type { OperationSnapshot } from '../../api/types'
import { actionMessage, apiRequest } from '../../api/client'
import { useRuntime } from '../../context/runtime'
import { operationTitle } from '../../lib/format'
import { ErrorState } from '../ErrorState'
import { LoadingState } from '../LoadingState'
import { ProgressCard } from '../ProgressCard'
import { ModalFrame } from './ModalFrame'

export function OperationModal({
  operationId,
  onClose,
}: {
  operationId: string
  onClose: () => void
}) {
  const { notify, refresh } = useRuntime()
  const [operation, setOperation] = useState<OperationSnapshot | null>(null)
  const [error, setError] = useState<string | null>(null)
  const announcedRef = useRef(false)

  useEffect(() => {
    let active = true
    let timeoutId: number | null = null

    async function load() {
      try {
        const payload = await apiRequest<OperationSnapshot>(`/operations/${operationId}`)
        if (!active) {
          return
        }
        setOperation(payload)
        setError(null)
        if (payload.status === 'running') {
          timeoutId = window.setTimeout(() => void load(), 600)
        }
      } catch (caughtError) {
        if (!active) {
          return
        }
        setError(caughtError instanceof Error ? caughtError.message : 'Operation failed.')
      }
    }

    void load()

    return () => {
      active = false
      if (timeoutId) {
        window.clearTimeout(timeoutId)
      }
    }
  }, [operationId])

  useEffect(() => {
    if (!operation || operation.status === 'running' || announcedRef.current) {
      return
    }
    announcedRef.current = true
    if (operation.status === 'succeeded') {
      notify(
        actionMessage({
          message: operation.message ?? 'Operation complete.',
          warnings: operation.warnings,
        }),
      )
      refresh()
    } else if (operation.error) {
      notify(operation.error)
    }
  }, [operation, notify, refresh])

  const title = operation
    ? operation.kind === 'identity_all'
      ? operationTitle(operation.kind)
      : `${operationTitle(operation.kind)} ${operation.provider_name}`
    : 'Working'
  const primaryProgressLabel =
    operation?.kind === 'identity' || operation?.kind === 'identity_all'
      ? 'Tracks'
      : 'Saved tracks'

  return (
    <ModalFrame title={title} onClose={onClose}>
      {error ? (
        <ErrorState message={error} compact />
      ) : !operation ? (
        <LoadingState label="Starting operation" compact />
      ) : (
        <div className="modal-stack">
          <div className="operation-head">
            <span className="eyebrow">
              {operation.status === 'running'
                ? 'In progress'
                : operation.status === 'succeeded'
                  ? 'Complete'
                  : 'Failed'}
            </span>
            <strong>{operation.stage}</strong>
            {operation.detail ? <p>{operation.detail}</p> : null}
          </div>

          <div className="operation-grid">
            <ProgressCard
              label={primaryProgressLabel}
              done={operation.saved_tracks_done}
              total={operation.saved_tracks_total}
            />
            <ProgressCard
              label="Playlists"
              done={operation.playlists_done}
              total={operation.playlists_total}
            />
            <ProgressCard
              label="Playlist tracks"
              done={operation.playlist_entries_done}
              total={operation.playlist_entries_total}
            />
          </div>

          {operation.message ? (
            <div className="confirm-copy">
              <p>{operation.message}</p>
            </div>
          ) : null}

          {operation.error ? (
            <div className="confirm-warning confirm-warning--danger">
              <strong>Operation failed</strong>
              <span>{operation.error}</span>
            </div>
          ) : null}

          {operation.warnings.length ? (
            <div className="confirm-warning confirm-warning--warning">
              <strong>Warnings</strong>
              <ul className="operation-warning-list">
                {operation.warnings.map((warning) => (
                  <li key={warning}>{warning}</li>
                ))}
              </ul>
            </div>
          ) : null}

          <div className="modal-actions">
            {operation.status === 'running' ? (
              <button className="btn btn--ghost" onClick={onClose} type="button">
                Hide
              </button>
            ) : (
              <button className="btn btn--primary" onClick={onClose} type="button">
                Done
              </button>
            )}
          </div>
        </div>
      )}
    </ModalFrame>
  )
}
