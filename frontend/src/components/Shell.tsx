import type { ReactNode } from 'react'
import { useLocation } from 'react-router-dom'
import type { Overview } from '../api/types'
import { useRuntime } from '../context/runtime'
import { useApiQuery } from '../hooks/useApiQuery'
import { useMediaQuery } from '../hooks/useMediaQuery'
import { SidebarLink } from './SidebarLink'
import { SidebarMetric } from './SidebarMetric'

export function Shell({ children }: { children: ReactNode }) {
  const location = useLocation()
  const { revision } = useRuntime()
  const { data: overview } = useApiQuery<Overview>('/overview', revision)
  const isCompact = useMediaQuery('(max-width: 980px)')

  const heroMetric =
    location.pathname.indexOf('/playlists') >= 0
      ? `${overview?.playlists ?? 0} playlists`
      : location.pathname.indexOf('/identity-conflicts') >= 0
        ? `${overview?.identity_conflicts ?? 0} conflicts`
        : location.pathname.indexOf('/identity-gaps') >= 0
          ? 'ID gaps'
      : location.pathname.indexOf('/tracks') >= 0
        ? `${overview?.tracks ?? 0} tracks`
        : `${overview?.saved_tracks ?? 0} saved tracks`

  // Focus card + library metrics. On desktop they sit in the sidebar; on mobile
  // they move into a collapsible disclosure rendered AFTER the main content.
  const stats = overview ? (
    <>
      <div className="sidebar-card">
        <span className="eyebrow">Focus</span>
        <strong>{heroMetric}</strong>
        <p>Pull. Edit. Push.</p>
      </div>
      <div className="sidebar-stats">
        <SidebarMetric label="Multi-provider" value={overview.multi_provider} />
        <SidebarMetric label="Canonical only" value={overview.canonical_only} />
        <SidebarMetric label="Unmatched" value={overview.unmatched_tracks} />
        <SidebarMetric label="Conflicts" value={overview.identity_conflicts} />
      </div>
    </>
  ) : null

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand-lockup">
          <div className="brand-kicker">spoti-dump</div>
          <h1>Canonical Library</h1>
          <p>Edit once. Sync everywhere.</p>
        </div>

        <nav aria-label="Primary" className="sidebar-nav">
          <SidebarLink
            to="/saved-tracks"
            label="Saved Tracks"
            copy="Your ground truth"
          />
          <SidebarLink
            to="/playlists"
            label="Playlists"
            copy="Edit collections"
          />
          <SidebarLink
            to="/tracks"
            label="Tracks"
            copy="Fix matches"
          />
          <SidebarLink
            to="/identity-conflicts"
            label="Conflicts"
            copy="Review merges"
          />
          <SidebarLink
            to="/identity-gaps"
            label="ID Gaps"
            copy="Repair coverage"
          />
          <SidebarLink
            to="/overview"
            label="Overview"
            copy="Providers and sync"
          />
          <SidebarLink
            to="/safety"
            label="Safety"
            copy="Backups and audit"
          />
        </nav>

        {!isCompact ? stats : null}
      </aside>

      <main className="stage">{children}</main>

      {isCompact && stats ? (
        <details className="library-stats">
          <summary>Library stats</summary>
          <div className="library-stats__body">{stats}</div>
        </details>
      ) : null}
    </div>
  )
}
