import { useCallback, useEffect, useState } from 'react'
import { apiRequest } from '../api/client'

export type ApiQueryResult<T> = {
  data: T | null
  loading: boolean
  error: string | null
  refetch: () => void
}

export function useApiQuery<T>(
  path: string | null,
  revision: number,
): ApiQueryResult<T> {
  const [data, setData] = useState<T | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  // A local counter that lets callers force a reload without touching the
  // shared runtime revision. Bumping it re-runs the fetch effect below.
  const [reloadIndex, setReloadIndex] = useState(0)

  // Stable across renders: setState updaters never change identity, so the
  // empty dependency list is correct and consumers can safely put refetch in
  // their own effect/callback dependency arrays.
  const refetch = useCallback(() => {
    setReloadIndex((current) => current + 1)
  }, [])

  useEffect(() => {
    const controller = new AbortController()
    let active = true

    async function load() {
      if (!path) {
        setData(null)
        setError(null)
        setLoading(false)
        return
      }
      setLoading(true)
      setError(null)
      try {
        const payload = await apiRequest<T>(path, { signal: controller.signal })
        if (active) {
          setData(payload)
        }
      } catch (caughtError) {
        if (!controller.signal.aborted && active) {
          setError(
            caughtError instanceof Error ? caughtError.message : 'Unknown error',
          )
        }
      } finally {
        if (active) {
          setLoading(false)
        }
      }
    }

    void load()

    return () => {
      active = false
      controller.abort()
    }
  }, [path, revision, reloadIndex])

  return { data, loading, error, refetch }
}
