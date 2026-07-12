import { startTransition, useEffect } from 'react'
import { useLocation, useNavigate } from 'react-router-dom'
import { useRuntime } from '../context/runtime'

export function NoticeBridge() {
  const location = useLocation()
  const navigate = useNavigate()
  const { notify } = useRuntime()

  useEffect(() => {
    const params = new URLSearchParams(location.search)
    const notice = params.get('notice')
    if (!notice) {
      return
    }

    notify(notice)
    params.delete('notice')
    startTransition(() =>
      navigate(
        {
          pathname: location.pathname,
          search: params.toString() ? `?${params.toString()}` : '',
        },
        { replace: true },
      ),
    )
  }, [location.pathname, location.search, navigate, notify])

  return null
}
