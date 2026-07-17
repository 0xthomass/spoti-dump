import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { NotifyOptions } from '../context/runtime'
import type { OperationSnapshot } from '../api/types'
import { actionMessage, apiRequest } from '../api/client'
import { operationDisplayTitle } from '../lib/format'

const POLL_INTERVAL_MS = 600

export type TrackedOperation = {
  id: string
  snapshot: OperationSnapshot | null
  error: string | null
}

type TrackerDeps = {
  notify: (message: string, options?: NotifyOptions) => void
  refresh: () => void
}

export type OperationTracker = {
  /** Begin (or restart) tracking an operation id. Safe to call at op start. */
  track: (operationId: string) => void
  /** Every tracked operation, keyed by id (running and finished). */
  operations: Record<string, TrackedOperation>
  /** Operations still running — what the tray renders as chips. */
  runningOperations: TrackedOperation[]
}

/**
 * App-level operation registry + poller.
 *
 * Design notes (why a hook, not a context/component):
 *  - The only consumer that needs the registry is App, which threads the
 *    results into the runtime value (openOperation → track) and passes derived
 *    slices to the tray and the modal as props. No other component reads it, so
 *    a context would add indirection without a second consumer. A hook also
 *    keeps this file free of component exports, which the react-refresh lint
 *    rule prefers.
 *
 * Stale-closure / interval-leak protections:
 *  - notify/refresh are read through refs, so the poll loop always calls the
 *    latest callbacks without re-subscribing the effect.
 *  - The poll effect depends only on `runningKey` (a primitive derived from the
 *    set of running ids). It re-runs when that set changes and its cleanup
 *    cancels the in-flight timer chain, so there is never more than one live
 *    timer, even across React 19 StrictMode double-mounts.
 *  - The loop is a self-scheduling setTimeout (not setInterval): each tick only
 *    schedules the next one after its fetches settle, so slow responses cannot
 *    stack overlapping intervals.
 *  - Terminal notifications are guarded by `notifiedRef` (per id, once). The
 *    guard entry is cleared in `track`, so re-tracking the same id announces
 *    again.
 */
export function useOperationTracker({
  notify,
  refresh,
}: TrackerDeps): OperationTracker {
  const [operations, setOperations] = useState<Record<string, TrackedOperation>>(
    {},
  )

  const notifyRef = useRef(notify)
  const refreshRef = useRef(refresh)
  const notifiedRef = useRef<Set<string>>(new Set())

  // Keep the latest callbacks reachable from the poll loop without making the
  // loop re-subscribe. Synced in an effect (never written during render).
  useEffect(() => {
    notifyRef.current = notify
    refreshRef.current = refresh
  })

  const runningOperations = useMemo(
    () =>
      Object.values(operations).filter(
        (operation) =>
          !operation.snapshot || operation.snapshot.status === 'running',
      ),
    [operations],
  )

  // Primitive dependency: stable while the running set is unchanged, so the
  // poll effect below only restarts when an op is added or finishes.
  const runningKey = runningOperations.map((operation) => operation.id).join('|')

  const track = useCallback((operationId: string) => {
    notifiedRef.current.delete(operationId)
    setOperations((current) => ({
      ...current,
      [operationId]: { id: operationId, snapshot: null, error: null },
    }))
  }, [])

  useEffect(() => {
    if (!runningKey) {
      return
    }
    const ids = runningKey.split('|')
    let cancelled = false
    let timer: number | undefined

    async function pollOnce(operationId: string) {
      try {
        const snapshot = await apiRequest<OperationSnapshot>(
          `/operations/${operationId}`,
        )
        if (cancelled) {
          return
        }
        setOperations((current) =>
          current[operationId]
            ? {
                ...current,
                [operationId]: {
                  ...current[operationId],
                  snapshot,
                  error: null,
                },
              }
            : current,
        )
        if (
          snapshot.status !== 'running' &&
          !notifiedRef.current.has(operationId)
        ) {
          notifiedRef.current.add(operationId)
          const title = operationDisplayTitle(snapshot)
          if (snapshot.status === 'succeeded') {
            notifyRef.current(
              actionMessage({
                message: snapshot.message ?? `${title} finished.`,
                warnings: snapshot.warnings,
              }),
              { tone: 'success' },
            )
          } else {
            notifyRef.current(snapshot.error ?? `${title} failed.`, {
              tone: 'error',
            })
          }
          refreshRef.current()
        }
      } catch (caughtError) {
        if (cancelled) {
          return
        }
        setOperations((current) =>
          current[operationId]
            ? {
                ...current,
                [operationId]: {
                  ...current[operationId],
                  error:
                    caughtError instanceof Error
                      ? caughtError.message
                      : 'Operation status unavailable.',
                },
              }
            : current,
        )
      }
    }

    async function tick() {
      await Promise.all(ids.map(pollOnce))
      if (!cancelled) {
        timer = window.setTimeout(() => void tick(), POLL_INTERVAL_MS)
      }
    }

    void tick()

    return () => {
      cancelled = true
      if (timer !== undefined) {
        window.clearTimeout(timer)
      }
    }
  }, [runningKey])

  return { track, operations, runningOperations }
}
