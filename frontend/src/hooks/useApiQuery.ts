import { useEffect, useState } from 'react'
import { apiRequest } from '../api/client'

export function useApiQuery<T>(path: string | null, revision: number) {
  const [data, setData] = useState<T | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

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
  }, [path, revision])

  return { data, loading, error }
}
