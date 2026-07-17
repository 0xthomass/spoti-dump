import { act, renderHook, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { apiRequest } from '../api/client'
import { useApiQuery } from './useApiQuery'

vi.mock('../api/client', () => ({
  apiRequest: vi.fn(),
}))

const mockApiRequest = vi.mocked(apiRequest)

beforeEach(() => {
  mockApiRequest.mockReset()
  mockApiRequest.mockResolvedValue({ ok: true })
})

describe('useApiQuery', () => {
  it('resolves data on success and clears loading/error', async () => {
    mockApiRequest.mockResolvedValue({ value: 42 })

    const { result } = renderHook(() => useApiQuery<{ value: number }>('/x', 0))

    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.data).toEqual({ value: 42 })
    expect(result.current.error).toBeNull()
    expect(mockApiRequest).toHaveBeenCalledWith('/x', expect.anything())
  })

  it('surfaces the error message and leaves data null on failure', async () => {
    mockApiRequest.mockRejectedValue(new Error('Track detail unavailable.'))

    const { result } = renderHook(() => useApiQuery('/x', 0))

    await waitFor(() => expect(result.current.error).toBe('Track detail unavailable.'))
    expect(result.current.data).toBeNull()
    expect(result.current.loading).toBe(false)
  })

  it('short-circuits a null path without calling the API', async () => {
    const { result } = renderHook(() => useApiQuery(null, 0))

    await waitFor(() => expect(result.current.loading).toBe(false))
    expect(result.current.data).toBeNull()
    expect(result.current.error).toBeNull()
    expect(mockApiRequest).not.toHaveBeenCalled()
  })

  it('exposes a stable refetch that refires the request', async () => {
    const { result, rerender } = renderHook(
      ({ path, revision }: { path: string; revision: number }) =>
        useApiQuery(path, revision),
      { initialProps: { path: '/x', revision: 0 } },
    )

    await waitFor(() => expect(result.current.loading).toBe(false))
    const firstRefetch = result.current.refetch
    expect(mockApiRequest).toHaveBeenCalledTimes(1)

    // Identity survives an unrelated re-render.
    rerender({ path: '/x', revision: 0 })
    expect(result.current.refetch).toBe(firstRefetch)

    act(() => result.current.refetch())
    await waitFor(() => expect(mockApiRequest).toHaveBeenCalledTimes(2))
    // Same callback instance even after the reload it triggered.
    expect(result.current.refetch).toBe(firstRefetch)
  })

  it('aborts the in-flight request when unmounted', async () => {
    let capturedSignal: AbortSignal | undefined
    mockApiRequest.mockImplementation((_path, init?: RequestInit) => {
      capturedSignal = init?.signal ?? undefined
      return new Promise<never>(() => {})
    })

    const { unmount } = renderHook(() => useApiQuery('/x', 0))

    await waitFor(() => expect(capturedSignal).toBeDefined())
    expect(capturedSignal?.aborted).toBe(false)

    unmount()
    expect(capturedSignal?.aborted).toBe(true)
  })
})
