import { act, renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { apiRequest } from '../api/client'
import type { OperationSnapshot, OperationStatus } from '../api/types'
import { useOperationTracker } from './useOperationTracker'

vi.mock('../api/client', async (importActual) => {
  const actual = await importActual<typeof import('../api/client')>()
  return { ...actual, apiRequest: vi.fn() }
})

const mockApiRequest = vi.mocked(apiRequest)

function snapshot(
  status: OperationStatus,
  over: Partial<OperationSnapshot> = {},
): OperationSnapshot {
  return {
    operation_id: 'op1',
    provider_key: 'spotify',
    provider_name: 'Spotify',
    kind: 'pull',
    status,
    stage: 'syncing',
    detail: null,
    saved_tracks_done: 0,
    saved_tracks_total: null,
    playlists_done: 0,
    playlists_total: null,
    playlist_entries_done: 0,
    playlist_entries_total: null,
    message: status === 'succeeded' ? 'Pulled 3 tracks.' : null,
    warnings: [],
    error: status === 'failed' ? 'Network down' : null,
    started_at: '2026-01-01T00:00:00Z',
    finished_at: status === 'running' ? null : '2026-01-01T00:01:00Z',
    ...over,
  }
}

/** Install an apiRequest implementation that ignores generics cleanly. */
function respond(fn: () => Promise<OperationSnapshot>) {
  mockApiRequest.mockImplementation(fn as unknown as typeof apiRequest)
}

let consoleError: ReturnType<typeof vi.spyOn>

beforeEach(() => {
  vi.useFakeTimers()
  mockApiRequest.mockReset()
  consoleError = vi.spyOn(console, 'error')
})

afterEach(() => {
  // The hook must never leak state updates past cleanup.
  const actWarning = consoleError.mock.calls.find((args: unknown[]) =>
    String(args[0]).includes('not wrapped in act'),
  )
  expect(actWarning).toBeUndefined()
  consoleError.mockRestore()
  vi.useRealTimers()
})

describe('useOperationTracker', () => {
  it('polls a tracked op, fires terminal notify + refresh once, then stops', async () => {
    const notify = vi.fn()
    const refresh = vi.fn()
    let calls = 0
    respond(() => {
      calls += 1
      return Promise.resolve(calls === 1 ? snapshot('running') : snapshot('succeeded'))
    })

    const { result } = renderHook(() => useOperationTracker({ notify, refresh }))
    act(() => result.current.track('op1'))

    // First poll runs immediately when the effect mounts.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
    })
    expect(mockApiRequest).toHaveBeenCalledWith('/operations/op1')
    expect(notify).not.toHaveBeenCalled()

    // Next tick lands the terminal snapshot.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(600)
    })
    expect(notify).toHaveBeenCalledTimes(1)
    expect(notify).toHaveBeenCalledWith('Pulled 3 tracks.', { tone: 'success' })
    expect(refresh).toHaveBeenCalledTimes(1)

    const callsAfterTerminal = mockApiRequest.mock.calls.length
    // Polling has stopped: further time yields no new requests or notifications.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2000)
    })
    expect(mockApiRequest.mock.calls.length).toBe(callsAfterTerminal)
    expect(notify).toHaveBeenCalledTimes(1)
    expect(result.current.runningOperations).toHaveLength(0)
  })

  it('reports a failed operation with an error notice', async () => {
    const notify = vi.fn()
    const refresh = vi.fn()
    let calls = 0
    respond(() => {
      calls += 1
      return Promise.resolve(calls === 1 ? snapshot('running') : snapshot('failed'))
    })

    const { result } = renderHook(() => useOperationTracker({ notify, refresh }))
    act(() => result.current.track('op1'))

    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
    })
    await act(async () => {
      await vi.advanceTimersByTimeAsync(600)
    })

    expect(notify).toHaveBeenCalledTimes(1)
    expect(notify).toHaveBeenCalledWith('Network down', { tone: 'error' })
    expect(refresh).toHaveBeenCalledTimes(1)
    expect(result.current.runningOperations).toHaveLength(0)
  })

  it('re-announces when the same id is tracked again', async () => {
    const notify = vi.fn()
    const refresh = vi.fn()
    respond(() => Promise.resolve(snapshot('succeeded')))

    const { result } = renderHook(() => useOperationTracker({ notify, refresh }))

    act(() => result.current.track('op1'))
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
    })
    expect(notify).toHaveBeenCalledTimes(1)

    // Re-tracking clears the once-guard and announces the terminal state again.
    act(() => result.current.track('op1'))
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
    })
    expect(notify).toHaveBeenCalledTimes(2)
  })

  it('cleans up its timer on unmount without further polling', async () => {
    const notify = vi.fn()
    const refresh = vi.fn()
    respond(() => Promise.resolve(snapshot('running')))

    const { result, unmount } = renderHook(() =>
      useOperationTracker({ notify, refresh }),
    )
    act(() => result.current.track('op1'))
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0)
    })
    const callsBeforeUnmount = mockApiRequest.mock.calls.length
    expect(callsBeforeUnmount).toBeGreaterThan(0)

    unmount()

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000)
    })
    expect(mockApiRequest.mock.calls.length).toBe(callsBeforeUnmount)
    expect(notify).not.toHaveBeenCalled()
  })
})
