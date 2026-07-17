import { useEffect, useState } from 'react'

/**
 * Subscribe to a CSS media query. Returns whether it currently matches and
 * updates on viewport changes. Used to switch between the desktop sidebar and
 * the mobile top-bar + "Library stats" disclosure layout.
 */
export function useMediaQuery(query: string) {
  const [matches, setMatches] = useState(
    () => typeof window !== 'undefined' && window.matchMedia(query).matches,
  )

  useEffect(() => {
    const mediaQueryList = window.matchMedia(query)
    const onChange = () => setMatches(mediaQueryList.matches)
    onChange()
    mediaQueryList.addEventListener('change', onChange)
    return () => mediaQueryList.removeEventListener('change', onChange)
  }, [query])

  return matches
}
