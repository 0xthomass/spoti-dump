import {
  createContext,
  startTransition,
  useContext,
  useEffect,
  useRef,
  useState,
} from 'react'
import type { CSSProperties, FormEvent, ReactNode } from 'react'
import {
  Link,
  NavLink,
  Navigate,
  Route,
  Routes,
  useLocation,
  useNavigate,
  useParams,
  useSearchParams,
} from 'react-router-dom'
import './App.css'

type PageResponse<T> = {
  items: T[]
  total: number
  page: number
  page_size: number
  total_pages: number
}

type ProviderMetric = {
  key: string
  name: string
  linked_tracks: number
  missing_track_ids: number
  unmatched_tracks: number
  synced_saved_tracks: number
  pushable_saved_tracks: number
  saved_tracks_missing_identity: number
  unmatched_saved_tracks: number
  linked_playlists: number
  pushable_playlist_entries: number
  playlist_entries_missing_identity: number
  unmatched_playlist_entries: number
}

type Overview = {
  library_updated_at: string
  tracks: number
  saved_tracks: number
  playlists: number
  playlist_entries: number
  canonical_only: number
  multi_provider: number
  unmatched_tracks: number
  identity_conflicts: number
  provider_only_counts: ProviderOnlyCount[]
  provider_metrics: ProviderMetric[]
}

type HealthResponse = {
  status: string
  database_path: string
  integrity_check: string
  tracks: number
  saved_tracks: number
  playlists: number
  playlist_entries: number
  durable_operation_history: boolean
}

type ProviderOnlyCount = {
  key: string
  name: string
  count: number
}

type ActionResponse = {
  message: string
  warnings: string[]
}

type ApplyIdentityResponse = {
  message: string
  result: string
  provider: string
  provider_id: string
  track_id: string
}

type OperationStartResponse = {
  operation_id: string
}

type OperationStatus = 'running' | 'succeeded' | 'failed'
type OperationKind =
  | 'verify'
  | 'pull'
  | 'push'
  | 'reset_push'
  | 'identity'
  | 'identity_all'

type OperationSnapshot = {
  operation_id: string
  provider_key: string
  provider_name: string
  kind: OperationKind
  status: OperationStatus
  stage: string
  detail: string | null
  saved_tracks_done: number
  saved_tracks_total: number | null
  playlists_done: number
  playlists_total: number | null
  playlist_entries_done: number
  playlist_entries_total: number | null
  message: string | null
  warnings: string[]
  error: string | null
  started_at: string
  finished_at: string | null
}

type ProviderConnectionState = {
  key: string
  name: string
  connected: boolean
  connected_at: string | null
  updated_at: string | null
  health_checked_at: string | null
  health_ok: boolean | null
  health_message: string | null
  cooldown_until: string | null
  cooldown_reason: string | null
  preflight: ProviderPreflight
}

type ProvidersResponse = {
  spotify_redirect_uri: string
  providers: ProviderConnectionState[]
}

type BackupItem = {
  file_name: string
  path: string
  backup_type: string
  size_bytes: number
  modified_at: string | null
}

type BackupsResponse = {
  automatic_backup_dir: string
  manual_backup_dir: string
  backups: BackupItem[]
}

type CreateBackupResponse = {
  message: string
  backup: BackupItem
}

type RestoreBackupResponse = {
  message: string
  restored_backup: BackupItem
  pre_restore_backup: BackupItem
}

type ProviderPreflight = {
  can_pull: boolean
  can_push: boolean
  can_reset_push: boolean
  blockers: string[]
  reset_blockers: string[]
  warnings: string[]
  saved_tracks_total: number
  saved_tracks_pushable: number
  saved_tracks_missing_identity: number
  playlists_total: number
  linked_playlists: number
  playlist_entries_total: number
  playlist_entries_pushable: number
  playlist_entries_missing_identity: number
  track_ids_total: number
  track_ids_linked: number
  track_ids_missing: number
}

type ProviderPushPlan = {
  provider: string
  provider_name: string
  preflight: ProviderPreflight
  saved_tracks: PushPlanSection
  playlist_entries: PushPlanSection
  playlists: PushPlaylistPlanSection
}

type PushPlanSection = {
  total: number
  pushable: number
  skipped_missing_identity: number
  skipped_examples: ConflictTrack[]
}

type PushPlaylistPlanSection = {
  total: number
  linked: number
  unlinked: number
  examples: PushPlaylistPlanItem[]
}

type PushPlaylistPlanItem = {
  playlist_id: string
  name: string
  entry_count: number
  linked: boolean
  missing_entries: number
}

const YOUTUBE_HEADERS_SAMPLE = `{
  "cookie": "SAPISID=your_cookie_here; __Secure-3PAPISID=your_cookie_here; SID=your_cookie_here",
  "x-goog-authuser": "paste_from_request",
  "origin": "https://music.youtube.com"
}`

type ProviderBadge = {
  key: string
  label: string
  source: string
  provider_id: string
}

type StatusPill = {
  key: string
  label: string
  title: string
}

type Coverage = {
  key: string
  label: string
  short_label: string
}

type SavedTrackItem = {
  saved_track_id: string
  track_id: string
  title: string
  artists: string[]
  artist_summary: string
  album: string | null
  subtitle: string
  duration_seconds: number | null
  duration_label: string
  isrc: string | null
  added_at: string | null
  added_label: string
  coverage: Coverage
  providers: ProviderBadge[]
  status_pills: StatusPill[]
  artwork_url: string | null
}

type TrackItem = {
  track_id: string
  title: string
  artists: string[]
  artist_summary: string
  album: string | null
  subtitle: string
  duration_seconds: number | null
  duration_label: string
  isrc: string | null
  coverage: Coverage
  providers: ProviderBadge[]
  status_pills: StatusPill[]
  saved_count: number
  playlist_refs: number
  artwork_url: string | null
}

type ProviderStatusDetail = {
  provider: string
  state: string
  message: string | null
  provider_item_id: string | null
  confidence: number | null
  last_attempt_at: string | null
  last_success_at: string | null
  last_seen_at: string | null
}

type TrackDetail = {
  track_id: string
  title: string
  artists: string[]
  artist_summary: string
  album: string | null
  duration_seconds: number | null
  duration_label: string
  isrc: string | null
  coverage: Coverage
  providers: ProviderBadge[]
  provider_status: ProviderStatusDetail[]
  identity_conflicts: TrackIdentityConflict[]
  saved_count: number
  playlist_refs: number
  artwork_url: string | null
}

type TrackIdentityConflict = {
  provider: string
  provider_name: string
  provider_id: string
  owner_track: ConflictTrack
  conflicting_provider_links: ProviderLinkConflict[]
  evidence: TrackIdentityConflictEvidence
  message: string
}

type TrackIdentityConflictEvidence = {
  provider_confidence: number | null
  metadata_similarity: number
  duration_delta_seconds: number | null
  source_saved_tracks: number
  source_playlist_entries: number
  candidate_saved_tracks: number
  candidate_playlist_entries: number
  recommendation: TrackIdentityConflictRecommendation
}

type TrackIdentityConflictRecommendation = {
  key: string
  label: string
  detail: string
}

type ConflictTrack = {
  track_id: string
  title: string
  artist_summary: string
  album: string | null
  coverage: Coverage
  providers: ProviderBadge[]
  saved_count: number
  playlist_refs: number
  artwork_url: string | null
}

type ProviderLinkConflict = {
  provider: string
  provider_name: string
  source_provider_id: string
  target_provider_id: string
}

type MergeTrackResponse = {
  message: string
  source_track_id: string
  target_track_id: string
  resolved_conflicts: {
    provider: string
    provider_name: string
    kept_provider_id: string
    dropped_provider_id: string
    kept_from_source: boolean
  }[]
}

type IdentityConflictQueueItem = {
  source_track: ConflictTrack
  conflict: TrackIdentityConflict
}

type IdentityGapQueueItem = {
  provider: string
  provider_name: string
  track: ConflictTrack
  push_blocking: boolean
}

type PlaylistSummary = {
  playlist_id: string
  name: string
  description: string | null
  entry_count: number
  providers: ProviderBadge[]
  status_pills: StatusPill[]
  artwork_url: string | null
}

type PlaylistEntry = {
  entry_id: string
  track_id: string
  title: string
  artists: string[]
  artist_summary: string
  album: string | null
  subtitle: string
  added_at: string | null
  added_label: string
  coverage: Coverage
  providers: ProviderBadge[]
  status_pills: StatusPill[]
  artwork_url: string | null
}

type PlaylistDetail = {
  playlist: PlaylistSummary
  entries: PlaylistEntry[]
}

type Runtime = {
  revision: number
  refresh: () => void
  notify: (message: string) => void
  openOperation: (operationId: string) => void
}

type ConfirmTone = 'danger' | 'warning'

type ConfirmRequest = {
  title: string
  message: string
  confirmLabel: string
  details?: string
  tone?: ConfirmTone
}

type ConfirmState = ConfirmRequest & {
  tone: ConfirmTone
}

type ConfirmApi = {
  confirm: (request: ConfirmRequest) => Promise<boolean>
}

const RuntimeContext = createContext<Runtime | null>(null)
const ConfirmContext = createContext<ConfirmApi | null>(null)

function useRuntime() {
  const value = useContext(RuntimeContext)
  if (!value) {
    throw new Error('Runtime context is unavailable')
  }
  return value
}

function useConfirm() {
  const value = useContext(ConfirmContext)
  if (!value) {
    throw new Error('Confirm context is unavailable')
  }
  return value.confirm
}

function apiPath(path: string) {
  return `/api${path}`
}

async function apiRequest<T>(path: string, init?: RequestInit): Promise<T> {
  const headers = new Headers(init?.headers)
  if (init?.body && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json')
  }

  const response = await fetch(apiPath(path), {
    ...init,
    headers,
  })

  const payload = await response.json().catch(() => null)
  if (!response.ok) {
    const message =
      payload && typeof payload.error === 'string'
        ? payload.error
        : `Request failed with status ${response.status}`
    throw new Error(message)
  }

  return payload as T
}

function actionMessage(payload: ActionResponse) {
  if (!payload.warnings.length) {
    return payload.message
  }
  return `${payload.message}\n${payload.warnings.map((warning) => `• ${warning}`).join('\n')}`
}

function useApiResource<T>(path: string | null, revision: number) {
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

function App() {
  const [revision, setRevision] = useState(0)
  const [toast, setToast] = useState<string | null>(null)
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null)
  const [activeOperationId, setActiveOperationId] = useState<string | null>(null)
  const confirmResolverRef = useRef<((accepted: boolean) => void) | null>(null)

  useEffect(() => {
    if (!toast) {
      return
    }
    const timeout = window.setTimeout(() => {
      setToast(null)
    }, 3200)
    return () => window.clearTimeout(timeout)
  }, [toast])

  useEffect(() => {
    return () => {
      confirmResolverRef.current?.(false)
      confirmResolverRef.current = null
    }
  }, [])

  function closeConfirm(accepted: boolean) {
    const resolver = confirmResolverRef.current
    confirmResolverRef.current = null
    setConfirmState(null)
    resolver?.(accepted)
  }

  useEffect(() => {
    if (!confirmState) {
      return
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') {
        closeConfirm(false)
      }
    }

    window.addEventListener('keydown', handleKeyDown)
    return () => window.removeEventListener('keydown', handleKeyDown)
  }, [confirmState])

  async function confirm(request: ConfirmRequest) {
    if (confirmResolverRef.current) {
      confirmResolverRef.current(false)
      confirmResolverRef.current = null
    }

    setConfirmState({
      ...request,
      tone: request.tone ?? 'danger',
    })

    return await new Promise<boolean>((resolve) => {
      confirmResolverRef.current = resolve
    })
  }

  const runtime: Runtime = {
    revision,
    refresh() {
      setRevision((current) => current + 1)
    },
    notify(message) {
      setToast(message)
    },
    openOperation(operationId) {
      setActiveOperationId(operationId)
    },
  }

  return (
    <RuntimeContext.Provider value={runtime}>
      <ConfirmContext.Provider value={{ confirm }}>
        <Shell>
          <NoticeBridge />
          <Routes>
            <Route path="/" element={<Navigate replace to="/saved-tracks" />} />
            <Route path="/overview" element={<OverviewPage />} />
            <Route path="/saved-tracks" element={<SavedTracksPage />} />
            <Route path="/tracks" element={<TracksPage />} />
            <Route path="/identity-conflicts" element={<IdentityConflictsPage />} />
            <Route path="/identity-gaps" element={<IdentityGapsPage />} />
            <Route path="/playlists" element={<PlaylistsPage />} />
            <Route path="/playlists/:playlistId" element={<PlaylistsPage />} />
            <Route path="/safety" element={<SafetyPage />} />
            <Route path="/database" element={<Navigate replace to="/overview" />} />
          </Routes>
          {toast ? <Toast message={toast} /> : null}
          {confirmState ? (
            <ConfirmModal
              request={confirmState}
              onCancel={() => closeConfirm(false)}
              onConfirm={() => closeConfirm(true)}
            />
          ) : null}
          {activeOperationId ? (
            <OperationModal
              operationId={activeOperationId}
              onClose={() => setActiveOperationId(null)}
            />
          ) : null}
        </Shell>
      </ConfirmContext.Provider>
    </RuntimeContext.Provider>
  )
}

function Shell({ children }: { children: ReactNode }) {
  const location = useLocation()
  const { revision } = useRuntime()
  const { data: overview } = useApiResource<Overview>('/overview', revision)

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

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand-lockup">
          <div className="brand-kicker">spoti-dump</div>
          <h1>Canonical Library</h1>
          <p>Edit once. Sync everywhere.</p>
        </div>

        <nav className="sidebar-nav">
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

        <div className="sidebar-card">
          <span className="eyebrow">Focus</span>
          <strong>{heroMetric}</strong>
          <p>Pull. Edit. Push.</p>
        </div>

        {overview ? (
          <div className="sidebar-stats">
            <SidebarMetric label="Multi-provider" value={overview.multi_provider} />
            <SidebarMetric label="Canonical only" value={overview.canonical_only} />
            <SidebarMetric label="Unmatched" value={overview.unmatched_tracks} />
            <SidebarMetric label="Conflicts" value={overview.identity_conflicts} />
          </div>
        ) : null}
      </aside>

      <main className="stage">{children}</main>
    </div>
  )
}

function NoticeBridge() {
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

function SidebarLink({
  to,
  label,
  copy,
}: {
  to: string
  label: string
  copy: string
}) {
  return (
    <NavLink
      className={({ isActive }) =>
        `sidebar-link${isActive ? ' sidebar-link--active' : ''}`
      }
      to={to}
    >
      <strong>{label}</strong>
      <span>{copy}</span>
    </NavLink>
  )
}

function SidebarMetric({ label, value }: { label: string; value: number }) {
  return (
    <div className="sidebar-metric">
      <strong>{formatNumber(value)}</strong>
      <span>{label}</span>
    </div>
  )
}

function PushPlanSummary({ plan }: { plan: ProviderPushPlan }) {
  return (
    <div className="push-plan">
      <div className="readiness-head">
        <span>Push plan</span>
        <strong>{plan.provider_name}</strong>
      </div>
      <div className="readiness-grid">
        <ReadinessItem
          label="Saved tracks"
          ready={plan.saved_tracks.pushable}
          blocked={plan.saved_tracks.skipped_missing_identity}
        />
        <ReadinessItem
          label="Playlist entries"
          ready={plan.playlist_entries.pushable}
          blocked={plan.playlist_entries.skipped_missing_identity}
        />
        <ReadinessItem
          label="Playlists"
          ready={plan.playlists.linked}
          blocked={plan.playlists.unlinked}
        />
      </div>
      {plan.saved_tracks.skipped_examples.length > 0 ? (
        <div className="push-plan-examples">
          <strong>Skipped saved-track examples</strong>
          {plan.saved_tracks.skipped_examples.slice(0, 3).map((track) => (
            <span key={`saved-${track.track_id}`}>
              {track.title || 'Untitled'} · {track.artist_summary || 'Unknown artist'}
            </span>
          ))}
        </div>
      ) : null}
      {plan.playlists.examples.length > 0 ? (
        <div className="push-plan-examples">
          <strong>Playlist risks</strong>
          {plan.playlists.examples.slice(0, 3).map((playlist) => (
            <span key={playlist.playlist_id}>
              {playlist.name}: {playlist.linked ? 'linked' : 'unlinked'}, {playlist.missing_entries}{' '}
              missing entries
            </span>
          ))}
        </div>
      ) : null}
    </div>
  )
}

function OverviewPage() {
  const { revision, refresh, notify, openOperation } = useRuntime()
  const confirm = useConfirm()
  const [spotifyModalOpen, setSpotifyModalOpen] = useState(false)
  const [youtubeModalOpen, setYoutubeModalOpen] = useState(false)
  const [pendingAction, setPendingAction] = useState<string | null>(null)
  const [pushPlans, setPushPlans] = useState<Record<string, ProviderPushPlan>>({})
  const [loadingPushPlan, setLoadingPushPlan] = useState<string | null>(null)
  const overviewResource = useApiResource<Overview>('/overview', revision)
  const providersResource = useApiResource<ProvidersResponse>('/providers', revision)

  useEffect(() => {
    const activeCooldowns =
      providersResource.data?.providers
        .map((provider) => cooldownRemainingMs(provider.cooldown_until))
        .filter((remainingMs) => remainingMs > 0) ?? []

    if (!activeCooldowns.length) {
      return
    }

    const refreshInterval = window.setInterval(() => refresh(), 30_000)
    const nextExpiryRefresh = window.setTimeout(
      () => refresh(),
      Math.min(...activeCooldowns) + 1_000,
    )

    return () => {
      window.clearInterval(refreshInterval)
      window.clearTimeout(nextExpiryRefresh)
    }
  }, [providersResource.data, refresh])

  async function runProviderAction(
    provider: ProviderConnectionState,
    action: 'verify' | 'export' | 'identity' | 'sync' | 'reset-sync' | 'disconnect',
  ) {
    const actionKey = `${provider.key}:${action}`
    setPendingAction(actionKey)
    try {
      if (action === 'disconnect') {
        const accepted = await confirm({
          title: `Disconnect ${provider.name}?`,
          message: `${provider.name} will no longer be controllable from the app until you reconnect it.`,
          details:
            'This removes the stored local connection details only. Canonical library data and provider links stay in SQLite.',
          confirmLabel: 'Disconnect provider',
          tone: 'warning',
        })
        if (!accepted) {
          return
        }
      }
      if (action === 'reset-sync') {
        const accepted = await confirm({
          title: `Reset ${provider.name} then push?`,
          message: `This will remove all saved tracks and playlists from the connected ${provider.name} account before pushing the canonical library.`,
          details:
            'Use this only for a destination account that should be replaced by the source of truth. The local canonical SQLite database is backed up before sync writes, but provider-side deletes are real.',
          confirmLabel: 'Reset & push',
          tone: 'danger',
        })
        if (!accepted) {
          return
        }
      }

      const path =
        action === 'disconnect'
          ? `/providers/${provider.key}/connection`
          : `/providers/${provider.key}/${action}/start`
      const method = 'POST'
      if (action === 'disconnect') {
        const payload = await apiRequest<ActionResponse>(
          `/providers/${provider.key}/connection`,
          { method: 'DELETE' },
        )
        notify(actionMessage(payload))
        refresh()
      } else {
        const payload = await apiRequest<OperationStartResponse>(path, { method })
        openOperation(payload.operation_id)
      }
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Provider action failed.')
    } finally {
      if (action === 'disconnect') {
        setPendingAction(null)
      } else {
        setPendingAction(null)
      }
    }
  }

  async function runLibraryIdentity() {
    const actionKey = 'library:identity-all'
    setPendingAction(actionKey)
    try {
      const accepted = await confirm({
        title: 'Resolve missing provider IDs?',
        message:
          'This will search Spotify and YouTube Music for missing track identities, then consolidate duplicate canonical rows when matches are safe.',
        details:
          'Provider accounts are not changed. The canonical SQLite database is updated, and the app snapshots the database before writing.',
        confirmLabel: 'Resolve IDs',
        tone: 'warning',
      })
      if (!accepted) {
        return
      }

      const payload = await apiRequest<OperationStartResponse>('/identity/start', {
        method: 'POST',
      })
      openOperation(payload.operation_id)
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Identity sync failed.')
    } finally {
      setPendingAction(null)
    }
  }

  async function loadPushPlan(provider: ProviderConnectionState) {
    setLoadingPushPlan(provider.key)
    try {
      const payload = await apiRequest<ProviderPushPlan>(
        `/providers/${provider.key}/push-plan`,
      )
      setPushPlans((current) => ({ ...current, [provider.key]: payload }))
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Failed to load push plan.')
    } finally {
      setLoadingPushPlan(null)
    }
  }

  if (
    (overviewResource.loading && !overviewResource.data) ||
    (providersResource.loading && !providersResource.data)
  ) {
    return <LoadingState label="Loading overview" />
  }
  if (
    overviewResource.error ||
    providersResource.error ||
    !overviewResource.data ||
    !providersResource.data
  ) {
    return (
      <ErrorState
        message={
          overviewResource.error ??
          providersResource.error ??
          'Failed to load overview.'
        }
      />
    )
  }

  const data = overviewResource.data
  const providers = providersResource.data
  const providerMetrics = new Map(
    data.provider_metrics.map((metric) => [metric.key, metric]),
  )
  const connectedProviderCount = providers.providers.filter(
    (provider) => provider.connected,
  ).length
  const blockedConnectedProviderCount = providers.providers.filter(
    (provider) => provider.connected && !provider.preflight.can_pull,
  ).length
  const providerIdentityGaps = providers.providers.reduce(
    (sum, provider) => sum + provider.preflight.track_ids_missing,
    0,
  )

  return (
    <section className="page-stack">
      <PageHero
        eyebrow="Overview"
        title="One library. Many providers."
        copy="Pull from providers. Edit here. Push changes back out."
      >
        <HeroStat label="Library updated" value={formatDateTime(data.library_updated_at)} />
        <HeroStat label="Tracks tracked" value={formatNumber(data.tracks)} />
      </PageHero>

      <div className="metric-grid">
        <DashboardCard label="Saved Tracks" value={data.saved_tracks}>
          Your kept songs.
        </DashboardCard>
        <DashboardCard label="Playlists" value={data.playlists}>
          Canonical collections.
        </DashboardCard>
        {data.provider_only_counts.map((providerOnly) => (
          <DashboardCard key={providerOnly.key} label={`${providerOnly.name} Only`} value={providerOnly.count}>
            Seen only on {providerOnly.name}.
          </DashboardCard>
        ))}
        <DashboardCard label="Multi-provider" value={data.multi_provider}>
          Resolved on more than one side.
        </DashboardCard>
        <DashboardCard label="Canonical Only" value={data.canonical_only}>
          Local only for now.
        </DashboardCard>
        <DashboardCard label="Identity Conflicts" value={data.identity_conflicts}>
          Rows needing explicit merge review.
        </DashboardCard>
      </div>

      <section className="panel">
        <div className="panel-head">
          <div>
            <span className="eyebrow">Identity Maintenance</span>
            <h2>Resolve Missing IDs</h2>
          </div>
          <button
            className="provider-action-button"
            disabled={pendingAction !== null || connectedProviderCount === 0}
            onClick={() => void runLibraryIdentity()}
            type="button"
          >
            {pendingAction === 'library:identity-all' ? 'Resolving…' : 'Resolve Missing IDs'}
          </button>
        </div>
        <div className="panel-body">
          <p>
            Run this before pushing to a new provider account. It searches linked provider
            catalogs for missing Spotify and YouTube Music IDs, deduplicates safe matches, and
            records unmatched rows for review.
          </p>
          <div className="metric-grid">
            <DashboardCard label="Linked Providers" value={connectedProviderCount}>
              Spotify and YouTube Music only.
            </DashboardCard>
            <DashboardCard label="Provider ID Gaps" value={providerIdentityGaps}>
              Missing IDs across both providers.
            </DashboardCard>
            <DashboardCard label="Conflict Queue" value={data.identity_conflicts}>
              Explicit merge decisions left.
            </DashboardCard>
            <DashboardCard label="Blocked Providers" value={blockedConnectedProviderCount}>
              Skipped until relinked, checked, or cooled down.
            </DashboardCard>
          </div>
          {data.identity_conflicts > 0 ? (
            <div className="provider-callout provider-callout--warning">
              <strong>{formatNumber(data.identity_conflicts)} identity conflicts need review</strong>
              <span>
                Resolve these before a final migration push so Spotify and YouTube Music IDs are
                consolidated onto one canonical row. <Link to="/identity-conflicts">Open review queue.</Link>
              </span>
            </div>
          ) : null}
        </div>
      </section>

      <section className="panel">
        <div className="panel-head">
          <div>
            <span className="eyebrow">Providers</span>
            <h2>Link, Pull, Push</h2>
          </div>
        </div>
        <div className="provider-grid">
          {providers.providers.map((provider) => {
            const metrics = providerMetrics.get(provider.key)
            const unresolved =
              (metrics?.unmatched_tracks ?? 0) +
              (metrics?.unmatched_saved_tracks ?? 0) +
              (metrics?.unmatched_playlist_entries ?? 0)
            const connectionCopy = provider.connected
              ? provider.updated_at
                ? `Linked ${formatDateTime(provider.updated_at)}`
                : 'Linked'
              : 'Not linked'
            const cooldownActive = provider.cooldown_until
              ? new Date(provider.cooldown_until).getTime() > Date.now()
              : false
            const cooldownCopy = providerCooldownCopy(provider)
            const healthFailed = provider.health_ok === false
            const providerActionDisabled =
              !provider.connected || pendingAction !== null || cooldownActive
            const preflightTitle =
              provider.preflight.blockers[0] ?? provider.cooldown_reason ?? undefined
            const resetPreflightTitle =
              provider.preflight.reset_blockers[0] ?? preflightTitle
            const savedPushable = provider.preflight.saved_tracks_pushable
            const savedMissingIdentity = provider.preflight.saved_tracks_missing_identity
            const playlistEntriesPushable = provider.preflight.playlist_entries_pushable
            const playlistEntriesMissingIdentity =
              provider.preflight.playlist_entries_missing_identity
            const pushCoverageTotal =
              savedPushable +
              savedMissingIdentity +
              playlistEntriesPushable +
              playlistEntriesMissingIdentity
            const identityGap =
              savedMissingIdentity +
              playlistEntriesMissingIdentity +
              provider.preflight.track_ids_missing
            const healthCopy =
              provider.health_ok === null
                ? 'Not checked'
                : provider.health_ok
                  ? provider.health_checked_at
                    ? `Checked ${formatDateTime(provider.health_checked_at)}`
                    : 'Healthy'
                  : 'Check failed'
            const readinessLabel = !provider.connected
              ? 'Link provider before sync'
              : cooldownActive
                ? 'Provider cooling down'
                : healthFailed
                  ? 'Connection check failed'
                : pushCoverageTotal === 0
                  ? 'No local library content'
                  : identityGap > 0
                    ? 'Ready, with identity gaps'
                    : 'Ready to push'
            const pushPlan = pushPlans[provider.key]
            return (
              <div className="provider-card provider-card--control" key={provider.key}>
                <header>
                  <div className="provider-card-headline">
                    <h3>{provider.name}</h3>
                  </div>
                  <span
                    className={`status-chip ${
                      cooldownActive
                        ? 'status-chip--warning'
                        : healthFailed
                          ? 'status-chip--warning'
                        : provider.connected
                          ? 'status-chip--good'
                          : 'status-chip--local'
                    }`}
                  >
                    {cooldownActive
                      ? 'Cooling down'
                      : healthFailed
                        ? 'Check failed'
                        : provider.connected
                          ? 'Live'
                          : 'Off'}
                  </span>
                </header>
                <div className="provider-readout">
                  <span className="mini-chip">{connectionCopy}</span>
                  {cooldownActive && provider.cooldown_until ? (
                    <span className="mini-chip mini-chip--warning">
                      Wait {formatDuration(cooldownRemainingMs(provider.cooldown_until))}
                    </span>
                  ) : null}
                  <span
                    className={`mini-chip ${
                      provider.health_ok === false
                        ? 'mini-chip--warning'
                        : provider.health_ok
                          ? 'mini-chip--good'
                          : ''
                    }`}
                    title={provider.health_message ?? undefined}
                  >
                    {healthCopy}
                  </span>
                  <span
                    className={`status-chip ${
                      unresolved > 0 ? 'status-chip--warning' : 'status-chip--good'
                    }`}
                  >
                    {unresolved > 0 ? `${unresolved} issues` : 'Clean'}
                  </span>
                </div>
                <div className="provider-readiness">
                  <div className="readiness-head">
                    <span>Push readiness</span>
                    <strong>{readinessLabel}</strong>
                  </div>
                  <div className="readiness-grid">
                    <ReadinessItem
                      label="Saved tracks"
                      ready={savedPushable}
                      blocked={savedMissingIdentity}
                    />
                    <ReadinessItem
                      label="Playlist entries"
                      ready={playlistEntriesPushable}
                      blocked={playlistEntriesMissingIdentity}
                    />
                    <ReadinessItem
                      label="All tracks with IDs"
                      ready={provider.preflight.track_ids_linked}
                      blocked={provider.preflight.track_ids_missing}
                    />
                  </div>
                  {provider.preflight.blockers.length > 0 ? (
                    <ul className="preflight-list preflight-list--blockers">
                      {provider.preflight.blockers.map((blocker) => (
                        <li key={blocker}>{blocker}</li>
                      ))}
                    </ul>
                  ) : null}
                  {provider.preflight.reset_blockers.length > 0 ? (
                    <ul className="preflight-list preflight-list--blockers">
                      {provider.preflight.reset_blockers.map((blocker) => (
                        <li key={blocker}>{blocker}</li>
                      ))}
                    </ul>
                  ) : null}
                  {cooldownCopy ? (
                    <div className="provider-callout provider-callout--warning">
                      <strong>Provider rate limit respected</strong>
                      <span>{cooldownCopy}</span>
                    </div>
                  ) : null}
                  {provider.preflight.warnings.length > 0 ? (
                    <ul className="preflight-list">
                      {provider.preflight.warnings.slice(0, 3).map((warning) => (
                        <li key={warning}>{warning}</li>
                      ))}
                    </ul>
                  ) : identityGap > 0 ? (
                    <p>
                      Run Resolve IDs before pushing to improve coverage, then review{' '}
                      <Link to={`/identity-gaps?provider=${provider.key}`}>
                        missing {provider.name} IDs
                      </Link>
                      . Push will skip tracks without a {provider.name} ID.
                    </p>
                  ) : null}
                  {pushPlan ? <PushPlanSummary plan={pushPlan} /> : null}
                </div>
                <div className="provider-actions">
                  <button
                    className="provider-action-button"
                    disabled={pendingAction !== null}
                    onClick={() => {
                      if (provider.key === 'spotify') {
                        setSpotifyModalOpen(true)
                        return
                      }
                      setYoutubeModalOpen(true)
                    }}
                    type="button"
                  >
                    {provider.connected ? 'Relink' : 'Link'}
                  </button>
                  <button
                    className="provider-action-button provider-action-button--secondary"
                    disabled={!provider.connected || pendingAction !== null || cooldownActive}
                    onClick={() => void runProviderAction(provider, 'verify')}
                    type="button"
                    title={provider.cooldown_reason ?? undefined}
                  >
                    {pendingAction === `${provider.key}:verify`
                      ? 'Checking…'
                      : 'Check Connection'}
                  </button>
                  <button
                    className="provider-action-button provider-action-button--secondary"
                    disabled={providerActionDisabled || !provider.preflight.can_pull}
                    onClick={() => void runProviderAction(provider, 'export')}
                    type="button"
                    title={preflightTitle}
                  >
                    {pendingAction === `${provider.key}:export` ? 'Pulling…' : 'Pull Library'}
                  </button>
                  <button
                    className="provider-action-button provider-action-button--secondary"
                    disabled={providerActionDisabled || !provider.preflight.can_pull}
                    onClick={() => void runProviderAction(provider, 'identity')}
                    type="button"
                    title={preflightTitle}
                  >
                    {pendingAction === `${provider.key}:identity`
                      ? 'Resolving…'
                      : 'Resolve IDs'}
                  </button>
                  <button
                    className="provider-action-button provider-action-button--secondary"
                    disabled={providerActionDisabled || !provider.preflight.can_push}
                    onClick={() => void runProviderAction(provider, 'sync')}
                    type="button"
                    title={preflightTitle}
                  >
                    {pendingAction === `${provider.key}:sync` ? 'Pushing…' : 'Push Changes'}
                  </button>
                  <button
                    className="provider-action-button provider-action-button--secondary"
                    disabled={loadingPushPlan !== null}
                    onClick={() => void loadPushPlan(provider)}
                    type="button"
                  >
                    {loadingPushPlan === provider.key ? 'Planning…' : 'Push Plan'}
                  </button>
                  {provider.key === 'spotify' ? (
                    <button
                      className="provider-action-button provider-action-button--danger"
                      disabled={providerActionDisabled || !provider.preflight.can_reset_push}
                      onClick={() => void runProviderAction(provider, 'reset-sync')}
                      type="button"
                      title={resetPreflightTitle}
                    >
                      {pendingAction === `${provider.key}:reset-sync`
                        ? 'Resetting…'
                        : 'Reset & Push'}
                    </button>
                  ) : null}
                  {provider.connected ? (
                    <button
                      className="provider-action-button provider-action-button--ghost"
                      disabled={pendingAction !== null}
                      onClick={() => void runProviderAction(provider, 'disconnect')}
                      type="button"
                    >
                      {pendingAction === `${provider.key}:disconnect`
                        ? 'Disconnecting…'
                        : 'Disconnect'}
                    </button>
                  ) : null}
                </div>
                <div className="provider-metric-grid">
                  <StatTile label="Tracks" value={metrics?.linked_tracks ?? 0} />
                  <StatTile label="Missing IDs" value={metrics?.missing_track_ids ?? 0} />
                  <StatTile label="Saved" value={metrics?.synced_saved_tracks ?? 0} />
                  <StatTile label="Saved misses" value={metrics?.unmatched_saved_tracks ?? 0} />
                  <StatTile label="Playlists" value={metrics?.linked_playlists ?? 0} />
                  <StatTile
                    label="Playlist misses"
                    value={metrics?.unmatched_playlist_entries ?? 0}
                  />
                </div>
              </div>
            )
          })}
        </div>
      </section>

      {spotifyModalOpen ? (
        <SpotifyConnectModal
          redirectUri={providers.spotify_redirect_uri}
          onClose={() => setSpotifyModalOpen(false)}
        />
      ) : null}

      {youtubeModalOpen ? (
        <YoutubeMusicConnectModal onClose={() => setYoutubeModalOpen(false)} />
      ) : null}
    </section>
  )
}

function SafetyPage() {
  const { revision, refresh, notify } = useRuntime()
  const confirm = useConfirm()
  const backupsResource = useApiResource<BackupsResponse>('/backups', revision)
  const healthResource = useApiResource<HealthResponse>('/health', revision)
  const [creating, setCreating] = useState(false)
  const [restoring, setRestoring] = useState<string | null>(null)

  async function createBackup() {
    setCreating(true)
    try {
      const payload = await apiRequest<CreateBackupResponse>('/backups/manual', {
        method: 'POST',
      })
      notify(payload.message)
      refresh()
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Backup failed.')
    } finally {
      setCreating(false)
    }
  }

  async function restoreBackup(backup: BackupItem) {
    const accepted = await confirm({
      title: 'Restore this backup?',
      message: `This will replace the current canonical library database with ${backup.file_name}.`,
      details:
        'The app will first create a pre-restore manual backup of the current database. Provider accounts are not touched.',
      confirmLabel: 'Restore backup',
      tone: 'danger',
    })
    if (!accepted) {
      return
    }

    setRestoring(backup.path)
    try {
      const payload = await apiRequest<RestoreBackupResponse>('/backups/restore', {
        method: 'POST',
        body: JSON.stringify({
          backup_type: backup.backup_type,
          file_name: backup.file_name,
        }),
      })
      notify(payload.message)
      refresh()
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Restore failed.')
    } finally {
      setRestoring(null)
    }
  }

  if (
    (backupsResource.loading && !backupsResource.data) ||
    (healthResource.loading && !healthResource.data)
  ) {
    return <LoadingState label="Loading safety data" />
  }

  if (
    backupsResource.error ||
    healthResource.error ||
    !backupsResource.data ||
    !healthResource.data
  ) {
    return (
      <ErrorState
        message={
          backupsResource.error ??
          healthResource.error ??
          'Failed to load safety data.'
        }
      />
    )
  }

  const backups = backupsResource.data
  const health = healthResource.data
  const manualCount = backups.backups.filter(
    (backup) => backup.backup_type === 'manual',
  ).length
  const automaticCount = backups.backups.filter(
    (backup) => backup.backup_type === 'automatic',
  ).length

  return (
    <section className="page-stack">
      <PageHero
        eyebrow="Safety"
        title="Source-of-truth protection."
        copy="Manual snapshots are never pruned by automatic backup retention."
      >
        <HeroStat label="Database" value={health.integrity_check} />
        <HeroStat label="Manual backups" value={formatNumber(manualCount)} />
      </PageHero>

      <div className="metric-grid">
        <DashboardCard label="Tracks" value={health.tracks}>
          Canonical track rows.
        </DashboardCard>
        <DashboardCard label="Saved Tracks" value={health.saved_tracks}>
          Source-of-truth saved library.
        </DashboardCard>
        <DashboardCard label="Playlists" value={health.playlists}>
          Canonical playlist records.
        </DashboardCard>
        <DashboardCard label="Automatic Backups" value={automaticCount}>
          Retained rolling snapshots.
        </DashboardCard>
      </div>

      <section className="panel">
        <div className="panel-head">
          <div>
            <span className="eyebrow">Manual Backup</span>
            <h2>Snapshot the canonical database</h2>
            <p>
              Creates a point-in-time copy of <code>{health.database_path}</code> under{' '}
              <code>{backups.manual_backup_dir}</code>.
            </p>
          </div>
          <button
            className="provider-action-button"
            disabled={creating}
            onClick={() => void createBackup()}
            type="button"
          >
            {creating ? 'Creating…' : 'Create Manual Backup'}
          </button>
        </div>
      </section>

      <section className="panel">
        <div className="panel-head">
          <div>
            <span className="eyebrow">Backup Inventory</span>
            <h2>Local snapshots</h2>
            <p>
              Automatic: <code>{backups.automatic_backup_dir}</code>
            </p>
          </div>
        </div>
        <div className="backup-list">
          {backups.backups.length === 0 ? (
            <EmptyState
              compact
              title="No backups found yet."
              copy="Create a manual backup or perform a write operation to create one."
            />
          ) : (
            backups.backups.map((backup) => (
              <div className="backup-row" key={backup.path}>
                <div>
                  <strong>{backup.file_name}</strong>
                  <p>{backup.path}</p>
                </div>
                <div className="backup-meta">
                  <span className="mini-chip">{backup.backup_type}</span>
                  <span className="mini-chip">{formatBytes(backup.size_bytes)}</span>
                  <span className="mini-chip">
                    {backup.modified_at ? formatDateTime(backup.modified_at) : 'Unknown date'}
                  </span>
                  <button
                    className="provider-action-button provider-action-button--danger provider-action-button--compact"
                    disabled={restoring !== null}
                    onClick={() => void restoreBackup(backup)}
                    type="button"
                  >
                    {restoring === backup.path ? 'Restoring…' : 'Restore'}
                  </button>
                </div>
              </div>
            ))
          )}
        </div>
      </section>
    </section>
  )
}

function SpotifyConnectModal({
  redirectUri,
  onClose,
}: {
  redirectUri: string
  onClose: () => void
}) {
  const { notify } = useRuntime()
  const [clientId, setClientId] = useState('')
  const [clientSecret, setClientSecret] = useState('')
  const [submitting, setSubmitting] = useState(false)

  async function connect() {
    setSubmitting(true)
    try {
      const payload = await apiRequest<{ authorization_url: string }>(
        '/providers/spotify/connect/start',
        {
          method: 'POST',
          body: JSON.stringify({
            client_id: clientId,
            client_secret: clientSecret,
          }),
        },
      )
      window.location.assign(payload.authorization_url)
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Spotify connection failed.')
      setSubmitting(false)
    }
  }

  return (
    <ModalFrame title="Link Spotify" onClose={onClose}>
      <div className="modal-stack">
        <div className="confirm-copy">
          <p>
            Add this redirect URI in your Spotify app, then finish login from here.
          </p>
        </div>
        <label className="field">
          <span>Redirect URI</span>
          <input readOnly value={redirectUri} />
        </label>
        <div className="field-grid">
          <label className="field">
            <span>Client ID</span>
            <input
              autoComplete="off"
              onChange={(event) => setClientId(event.target.value)}
              value={clientId}
            />
          </label>
          <label className="field">
            <span>Client Secret</span>
            <input
              autoComplete="off"
              onChange={(event) => setClientSecret(event.target.value)}
              type="password"
              value={clientSecret}
            />
          </label>
        </div>
        <div className="modal-actions">
          <button className="ghost-button" onClick={onClose} type="button">
            Cancel
          </button>
          <button
            disabled={!clientId.trim() || !clientSecret.trim() || submitting}
            onClick={() => void connect()}
            type="button"
          >
            {submitting ? 'Opening…' : 'Open Spotify'}
          </button>
        </div>
      </div>
    </ModalFrame>
  )
}

function YoutubeMusicConnectModal({ onClose }: { onClose: () => void }) {
  const { notify, refresh } = useRuntime()
  const [headersJson, setHeadersJson] = useState(YOUTUBE_HEADERS_SAMPLE)
  const [submitting, setSubmitting] = useState(false)

  async function connect() {
    setSubmitting(true)
    try {
      const payload = await apiRequest<ActionResponse>(
        '/providers/youtube-music/connect',
        {
          method: 'POST',
          body: JSON.stringify({ headers_json: headersJson }),
        },
      )
      notify(actionMessage(payload))
      refresh()
      onClose()
    } catch (error) {
      notify(
        error instanceof Error ? error.message : 'YouTube Music connection failed.',
      )
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <ModalFrame title="Link YouTube Music" onClose={onClose}>
      <div className="modal-stack">
        <div className="confirm-copy">
          <p>
            Paste the signed-in browser headers JSON from YouTube Music.
          </p>
        </div>
        <label className="field">
          <span>Headers JSON</span>
          <textarea
            onChange={(event) => setHeadersJson(event.target.value)}
            placeholder={YOUTUBE_HEADERS_SAMPLE}
            rows={10}
            value={headersJson}
          />
        </label>
        <div className="modal-actions">
          <button className="ghost-button" onClick={onClose} type="button">
            Cancel
          </button>
          <button
            disabled={!headersJson.trim() || submitting}
            onClick={() => void connect()}
            type="button"
          >
            {submitting ? 'Linking…' : 'Save Link'}
          </button>
        </div>
      </div>
    </ModalFrame>
  )
}

function SavedTracksPage() {
  const { revision, refresh, notify } = useRuntime()
  const confirm = useConfirm()
  const [searchParams, setSearchParams] = useSearchParams()
  const [draft, setDraft] = useState(searchParams.get('q') ?? '')
  const [editingTrackId, setEditingTrackId] = useState<string | null>(null)
  const page = parsePage(searchParams.get('page'))
  const query = searchParams.get('q') ?? ''

  useEffect(() => {
    setDraft(query)
  }, [query])

  const resource = useApiResource<PageResponse<SavedTrackItem>>(
    `/saved-tracks?page=${page}${query ? `&q=${encodeURIComponent(query)}` : ''}`,
    revision,
  )

  async function removeSavedTrack(item: SavedTrackItem) {
    const accepted = await confirm({
      title: 'Remove saved track?',
      message: `"${item.title}" will be removed from canonical saved tracks.`,
      details:
        'The app updates the canonical library first, then immediately tries to unlike the linked track on every connected provider.',
      confirmLabel: 'Remove saved track',
      tone: 'warning',
    })
    if (!accepted) {
      return
    }
    try {
      const payload = await apiRequest<ActionResponse>(
        `/saved-tracks/${item.saved_track_id}`,
        { method: 'DELETE' },
      )
      notify(actionMessage(payload))
      refresh()
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Delete failed.')
    }
  }

  function submitSearch(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const next = new URLSearchParams()
    if (draft.trim()) {
      next.set('q', draft.trim())
    }
    next.set('page', '1')
    startTransition(() => setSearchParams(next))
  }

  return (
    <section className="page-stack">
      <PageHero
        eyebrow="Saved Tracks"
        title="The list you keep for life."
        copy="New pulls add here. Nothing leaves until you remove it."
      >
        <HeroStat
          label="Showing"
          value={
            resource.data
              ? `${resource.data.items.length} of ${formatNumber(resource.data.total)}`
              : '...'
          }
        />
      </PageHero>

      <section className="panel">
        <div className="panel-head panel-head--row">
          <div>
            <span className="eyebrow">Browse</span>
            <h2>Saved Library</h2>
          </div>
          <form className="searchbar" onSubmit={submitSearch}>
            <input
              value={draft}
              onChange={(event) => setDraft(event.target.value)}
              placeholder="Search title, artist, album, ISRC"
              type="search"
            />
            <button type="submit">Search</button>
          </form>
        </div>

        {resource.loading && !resource.data ? (
          <LoadingState label="Loading saved tracks" compact />
        ) : resource.error || !resource.data ? (
          <ErrorState message={resource.error ?? 'Failed to load saved tracks.'} compact />
        ) : resource.data.items.length === 0 ? (
          <EmptyState
            title="Nothing matched"
            copy="Try a broader search or import another provider export into the canonical database."
          />
        ) : (
          <>
            <TrackList
              items={resource.data.items}
              showAdded
              onEdit={(item) => setEditingTrackId(item.track_id)}
              onDelete={(item) => void removeSavedTrack(item)}
            />
            <Pagination
              page={resource.data.page}
              totalPages={resource.data.total_pages}
              onPageChange={(nextPage) => {
                const next = new URLSearchParams(searchParams)
                next.set('page', String(nextPage))
                startTransition(() => setSearchParams(next))
              }}
            />
          </>
        )}
      </section>

      {editingTrackId ? (
        <TrackEditorModal
          trackId={editingTrackId}
          onClose={() => setEditingTrackId(null)}
        />
      ) : null}
    </section>
  )
}

function TracksPage() {
  const { revision, refresh, notify } = useRuntime()
  const confirm = useConfirm()
  const [searchParams, setSearchParams] = useSearchParams()
  const [draft, setDraft] = useState(searchParams.get('q') ?? '')
  const [editingTrackId, setEditingTrackId] = useState<string | null>(null)
  const page = parsePage(searchParams.get('page'))
  const query = searchParams.get('q') ?? ''
  const coverage = searchParams.get('coverage') ?? ''

  useEffect(() => {
    setDraft(query)
  }, [query])

  const path = `/tracks?page=${page}${query ? `&q=${encodeURIComponent(query)}` : ''}${
    coverage ? `&coverage=${encodeURIComponent(coverage)}` : ''
  }`
  const resource = useApiResource<PageResponse<TrackItem>>(path, revision)

  async function deleteTrack(item: TrackItem) {
    const accepted = await confirm({
      title: 'Delete track everywhere?',
      message: `"${item.title}" will be removed from canonical saved tracks and every canonical playlist entry that references it.`,
      details:
        'The app updates the canonical library first, then immediately tries to unlike the track and resync affected playlists on every connected provider.',
      confirmLabel: 'Delete everywhere',
      tone: 'danger',
    })
    if (!accepted) {
      return
    }
    try {
      const payload = await apiRequest<ActionResponse>(`/tracks/${item.track_id}`, {
        method: 'DELETE',
      })
      notify(actionMessage(payload))
      refresh()
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Delete failed.')
    }
  }

  function submitSearch(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const next = new URLSearchParams()
    if (draft.trim()) {
      next.set('q', draft.trim())
    }
    if (coverage) {
      next.set('coverage', coverage)
    }
    next.set('page', '1')
    startTransition(() => setSearchParams(next))
  }

  function changeCoverage(nextCoverage: string) {
    const next = new URLSearchParams(searchParams)
    if (nextCoverage) {
      next.set('coverage', nextCoverage)
    } else {
      next.delete('coverage')
    }
    next.set('page', '1')
    startTransition(() => setSearchParams(next))
  }

  return (
    <section className="page-stack">
      <PageHero
        eyebrow="Tracks"
        title="Track coverage."
        copy="See where each track resolves and fix the metadata used for matching."
      >
        <HeroStat label="Coverage filter" value={coverageLabel(coverage)} />
      </PageHero>

      <section className="panel">
        <div className="panel-head panel-head--stack">
          <div>
            <span className="eyebrow">Inspect</span>
            <h2>Canonical Tracks</h2>
          </div>
          <form className="searchbar" onSubmit={submitSearch}>
            <input
              value={draft}
              onChange={(event) => setDraft(event.target.value)}
              placeholder="Search title, artist, album, ISRC"
              type="search"
            />
            <button type="submit">Search</button>
          </form>
          <div className="filter-row">
            {[
              ['', 'All'],
              ['missing-any-provider', 'Missing any ID'],
              ['missing-spotify', 'Missing Spotify ID'],
              ['missing-youtube-music', 'Missing YouTube ID'],
              ['spotify-only', 'Spotify only'],
              ['youtube-music-only', 'YouTube only'],
              ['multi-provider', 'Multi-provider'],
              ['canonical-only', 'Canonical only'],
              ['identity-conflicts', 'Identity conflicts'],
              ['unmatched', 'Unmatched'],
            ].map(([value, label]) => (
              <button
                className={`filter-pill${coverage === value ? ' filter-pill--active' : ''}`}
                key={value || 'all'}
                onClick={() => changeCoverage(value)}
                type="button"
              >
                {label}
              </button>
            ))}
          </div>
        </div>

        {resource.loading && !resource.data ? (
          <LoadingState label="Loading tracks" compact />
        ) : resource.error || !resource.data ? (
          <ErrorState message={resource.error ?? 'Failed to load tracks.'} compact />
        ) : resource.data.items.length === 0 ? (
          <EmptyState
            title="No tracks matched"
            copy="Try a broader filter or drop the coverage constraint."
          />
        ) : (
          <>
            <TrackList
              items={resource.data.items}
              usageMode
              onEdit={(item) => setEditingTrackId(item.track_id)}
              onDelete={(item) => void deleteTrack(item)}
            />
            <Pagination
              page={resource.data.page}
              totalPages={resource.data.total_pages}
              onPageChange={(nextPage) => {
                const next = new URLSearchParams(searchParams)
                next.set('page', String(nextPage))
                startTransition(() => setSearchParams(next))
              }}
            />
          </>
        )}
      </section>

      {editingTrackId ? (
        <TrackEditorModal
          trackId={editingTrackId}
          onClose={() => setEditingTrackId(null)}
        />
      ) : null}
    </section>
  )
}

function IdentityGapsPage() {
  const { revision } = useRuntime()
  const [searchParams, setSearchParams] = useSearchParams()
  const [draft, setDraft] = useState(searchParams.get('q') ?? '')
  const [editingTrackId, setEditingTrackId] = useState<string | null>(null)
  const page = parsePage(searchParams.get('page'))
  const query = searchParams.get('q') ?? ''
  const provider = searchParams.get('provider') ?? ''

  useEffect(() => {
    setDraft(query)
  }, [query])

  const resource = useApiResource<PageResponse<IdentityGapQueueItem>>(
    `/identity/gaps?page=${page}${provider ? `&provider=${encodeURIComponent(provider)}` : ''}${
      query ? `&q=${encodeURIComponent(query)}` : ''
    }`,
    revision,
  )

  function submitSearch(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const next = new URLSearchParams(searchParams)
    if (draft.trim()) {
      next.set('q', draft.trim())
    } else {
      next.delete('q')
    }
    next.set('page', '1')
    startTransition(() => setSearchParams(next))
  }

  function changeProvider(nextProvider: string) {
    const next = new URLSearchParams(searchParams)
    if (nextProvider) {
      next.set('provider', nextProvider)
    } else {
      next.delete('provider')
    }
    next.set('page', '1')
    startTransition(() => setSearchParams(next))
  }

  return (
    <section className="page-stack">
      <PageHero
        eyebrow="Provider ID Gaps"
        title="Repair push coverage."
        copy="Find canonical tracks that still need Spotify or YouTube Music IDs before a complete migration push."
      >
        <HeroStat
          label="Showing"
          value={
            resource.data
              ? `${resource.data.items.length} of ${formatNumber(resource.data.total)}`
              : '...'
          }
        />
        <HeroStat label="Provider" value={identityGapProviderLabel(provider)} />
      </PageHero>

      <section className="panel">
        <div className="panel-head panel-head--stack">
          <div>
            <span className="eyebrow">Repair</span>
            <h2>Missing Provider IDs</h2>
          </div>
          <form className="searchbar" onSubmit={submitSearch}>
            <input
              value={draft}
              onChange={(event) => setDraft(event.target.value)}
              placeholder="Search title, artist, album"
              type="search"
            />
            <button type="submit">Search</button>
          </form>
          <div className="filter-row">
            {[
              ['', 'All'],
              ['spotify', 'Missing Spotify'],
              ['youtube-music', 'Missing YouTube Music'],
            ].map(([value, label]) => (
              <button
                className={`filter-pill${provider === value ? ' filter-pill--active' : ''}`}
                key={value || 'all'}
                onClick={() => changeProvider(value)}
                type="button"
              >
                {label}
              </button>
            ))}
          </div>
        </div>

        {resource.loading && !resource.data ? (
          <LoadingState label="Loading provider ID gaps" compact />
        ) : resource.error || !resource.data ? (
          <ErrorState message={resource.error ?? 'Failed to load ID gaps.'} compact />
        ) : resource.data.items.length === 0 ? (
          <EmptyState
            title="No ID gaps matched"
            copy="Broaden the search, change provider, or run Resolve Missing IDs again."
          />
        ) : (
          <>
            <div className="conflict-list">
              {resource.data.items.map((item) => (
                <article
                  className="conflict-card"
                  key={`${item.provider}:${item.track.track_id}`}
                >
                  <div className="conflict-card-head">
                    <div>
                      <span className="eyebrow">Missing {item.provider_name} ID</span>
                      <h3>{item.track.title}</h3>
                    </div>
                    <span
                      className={`status-chip ${
                        item.push_blocking ? 'status-chip--warning' : 'status-chip--local'
                      }`}
                    >
                      {item.push_blocking ? 'Affects push' : 'No push refs'}
                    </span>
                  </div>
                  <div className="conflict-track-grid conflict-track-grid--single">
                    <ConflictTrackCard
                      label="Canonical row"
                      onEdit={() => setEditingTrackId(item.track.track_id)}
                      track={item.track}
                    />
                  </div>
                  <div className="conflict-detail">
                    <p>
                      This row is missing a {item.provider_name} identity. Open the row and paste
                      the correct {item.provider_name} track URL or ID in the Identity Repair form.
                    </p>
                  </div>
                </article>
              ))}
            </div>
            <Pagination
              page={resource.data.page}
              totalPages={resource.data.total_pages}
              onPageChange={(nextPage) => {
                const next = new URLSearchParams(searchParams)
                next.set('page', String(nextPage))
                startTransition(() => setSearchParams(next))
              }}
            />
          </>
        )}
      </section>

      {editingTrackId ? (
        <TrackEditorModal
          trackId={editingTrackId}
          onClose={() => setEditingTrackId(null)}
        />
      ) : null}
    </section>
  )
}

function IdentityConflictsPage() {
  const { revision, refresh, notify } = useRuntime()
  const confirm = useConfirm()
  const [searchParams, setSearchParams] = useSearchParams()
  const [draft, setDraft] = useState(searchParams.get('q') ?? '')
  const [editingTrackId, setEditingTrackId] = useState<string | null>(null)
  const [mergingConflict, setMergingConflict] = useState<string | null>(null)
  const [rejectingConflict, setRejectingConflict] = useState<string | null>(null)
  const page = parsePage(searchParams.get('page'))
  const query = searchParams.get('q') ?? ''
  const provider = searchParams.get('provider') ?? ''
  const recommendation = searchParams.get('recommendation') ?? ''
  const impact = searchParams.get('impact') ?? ''

  useEffect(() => {
    setDraft(query)
  }, [query])

  const resource = useApiResource<PageResponse<IdentityConflictQueueItem>>(
    `/identity/conflicts?page=${page}${
      provider ? `&provider=${encodeURIComponent(provider)}` : ''
    }${recommendation ? `&recommendation=${encodeURIComponent(recommendation)}` : ''}${
      impact ? `&impact=${encodeURIComponent(impact)}` : ''
    }${query ? `&q=${encodeURIComponent(query)}` : ''}`,
    revision,
  )

  function submitSearch(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const next = new URLSearchParams(searchParams)
    if (draft.trim()) {
      next.set('q', draft.trim())
    } else {
      next.delete('q')
    }
    next.set('page', '1')
    startTransition(() => setSearchParams(next))
  }

  function changeConflictFilter(key: 'provider' | 'recommendation' | 'impact', value: string) {
    const next = new URLSearchParams(searchParams)
    if (value) {
      next.set(key, value)
    } else {
      next.delete(key)
    }
    next.set('page', '1')
    startTransition(() => setSearchParams(next))
  }

  async function mergeConflict(
    item: IdentityConflictQueueItem,
    conflictResolution: 'keep_source' | 'keep_target',
  ) {
    const keepSource = conflictResolution === 'keep_source'
    const accepted = await confirm({
      title: keepSource ? 'Merge and keep source IDs?' : 'Merge and keep candidate IDs?',
      message: `This will merge "${item.source_track.title}" into "${item.conflict.owner_track.title}".`,
      details: keepSource
        ? 'Saved tracks and playlist entries move to the candidate row. For conflicting providers, source row provider IDs win. Provider accounts are not changed.'
        : 'Saved tracks and playlist entries move to the candidate row. For conflicting providers, candidate row provider IDs win. Provider accounts are not changed.',
      confirmLabel: keepSource ? 'Merge, keep source' : 'Merge, keep candidate',
      tone: 'danger',
    })
    if (!accepted) {
      return
    }

    const mergeKey = `${item.source_track.track_id}:${item.conflict.provider}:${item.conflict.provider_id}:${conflictResolution}`
    setMergingConflict(mergeKey)
    try {
      const payload = await apiRequest<MergeTrackResponse>(
        `/tracks/${item.source_track.track_id}/merge`,
        {
          method: 'POST',
          body: JSON.stringify({
            target_track_id: item.conflict.owner_track.track_id,
            conflict_resolution: conflictResolution,
          }),
        },
      )
      notify(payload.message)
      refresh()
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Merge failed.')
    } finally {
      setMergingConflict(null)
    }
  }

  async function rejectConflict(item: IdentityConflictQueueItem) {
    const accepted = await confirm({
      title: 'Mark candidate as not same track?',
      message: `This will reject ${item.conflict.provider_name} candidate ${item.conflict.provider_id} for "${item.source_track.title}".`,
      details:
        'The rows will not be merged. Provider accounts are not changed. The source row will stay missing that provider ID until you link the correct identity or a different match is found.',
      confirmLabel: 'Mark not same',
      tone: 'warning',
    })
    if (!accepted) {
      return
    }

    const rejectKey = `${item.source_track.track_id}:${item.conflict.provider}:${item.conflict.provider_id}:reject`
    setRejectingConflict(rejectKey)
    try {
      const payload = await apiRequest<ActionResponse>(
        `/tracks/${item.source_track.track_id}/identity-conflicts/reject`,
        {
          method: 'POST',
          body: JSON.stringify({
            provider: item.conflict.provider,
            provider_id: item.conflict.provider_id,
            owner_track_id: item.conflict.owner_track.track_id,
          }),
        },
      )
      notify(actionMessage(payload))
      refresh()
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Reject failed.')
    } finally {
      setRejectingConflict(null)
    }
  }

  return (
    <section className="page-stack">
      <PageHero
        eyebrow="Identity Conflicts"
        title="Merge review queue."
        copy="Resolve ambiguous Spotify and YouTube Music matches without touching provider accounts."
      >
        <HeroStat
          label="Queue"
          value={
            resource.data
              ? `${resource.data.items.length} of ${formatNumber(resource.data.total)}`
              : '...'
          }
        />
        <HeroStat label="Provider" value={identityConflictProviderLabel(provider)} />
        <HeroStat
          label="Recommendation"
          value={identityConflictRecommendationLabel(recommendation)}
        />
      </PageHero>

      <section className="panel">
        <div className="panel-head panel-head--stack">
          <div>
            <span className="eyebrow">Review</span>
            <h2>Identity Conflict Queue</h2>
          </div>
          <form className="searchbar" onSubmit={submitSearch}>
            <input
              value={draft}
              onChange={(event) => setDraft(event.target.value)}
              placeholder="Search title, artist, album, provider ID"
              type="search"
            />
            <button type="submit">Search</button>
          </form>
          <div className="filter-row">
            {[
              ['', 'All providers'],
              ['spotify', 'Spotify candidate'],
              ['youtube-music', 'YouTube candidate'],
            ].map(([value, label]) => (
              <button
                className={`filter-pill${provider === value ? ' filter-pill--active' : ''}`}
                key={value || 'all-providers'}
                onClick={() => changeConflictFilter('provider', value)}
                type="button"
              >
                {label}
              </button>
            ))}
          </div>
          <div className="filter-row">
            {[
              ['', 'All recommendations'],
              ['likely_same_recording', 'Likely same'],
              ['needs_manual_review', 'Manual review'],
              ['likely_different_recording', 'Likely different'],
            ].map(([value, label]) => (
              <button
                className={`filter-pill${
                  recommendation === value ? ' filter-pill--active' : ''
                }`}
                key={value || 'all-recommendations'}
                onClick={() => changeConflictFilter('recommendation', value)}
                type="button"
              >
                {label}
              </button>
            ))}
          </div>
          <div className="filter-row">
            {[
              ['', 'All impact'],
              ['library_impact', 'Affects saved/playlists'],
              ['source_impact', 'Source row impact'],
              ['candidate_impact', 'Candidate row impact'],
            ].map(([value, label]) => (
              <button
                className={`filter-pill${impact === value ? ' filter-pill--active' : ''}`}
                key={value || 'all-impact'}
                onClick={() => changeConflictFilter('impact', value)}
                type="button"
              >
                {label}
              </button>
            ))}
          </div>
        </div>

        {resource.loading && !resource.data ? (
          <LoadingState label="Loading identity conflicts" compact />
        ) : resource.error || !resource.data ? (
          <ErrorState
            message={resource.error ?? 'Failed to load identity conflicts.'}
            compact
          />
        ) : resource.data.items.length === 0 ? (
          <EmptyState
            title="No conflicts matched"
            copy="Run Resolve Missing IDs again after reviewing current conflicts or broaden the search."
          />
        ) : (
          <>
            <div className="conflict-list">
              {resource.data.items.map((item) => {
                const mergeKey = `${item.source_track.track_id}:${item.conflict.provider}:${item.conflict.provider_id}`
                return (
                  <article className="conflict-card" key={mergeKey}>
                    <div className="conflict-card-head">
                      <div>
                        <span className="eyebrow">{item.conflict.provider_name} conflict</span>
                        <h3>{item.source_track.title}</h3>
                      </div>
                      <span className="status-chip status-chip--warning">
                        Candidate {item.conflict.provider_id}
                      </span>
                    </div>

                    <div className="conflict-track-grid">
                      <ConflictTrackCard
                        label="Source row"
                        onEdit={() => setEditingTrackId(item.source_track.track_id)}
                        track={item.source_track}
                      />
                      <ConflictTrackCard
                        label="Candidate owner"
                        onEdit={() =>
                          setEditingTrackId(item.conflict.owner_track.track_id)
                        }
                        track={item.conflict.owner_track}
                      />
                    </div>

                    <ConflictEvidencePanel conflict={item.conflict} />

                    <div className="conflict-detail">
                      <p>{item.conflict.message}</p>
                      {item.conflict.conflicting_provider_links.map((link) => (
                        <p key={link.provider}>
                          {link.provider_name}: source {link.source_provider_id} · candidate{' '}
                          {link.target_provider_id}
                        </p>
                      ))}
                    </div>

                    <div className="modal-actions modal-actions--inline">
                      <button
                        className="provider-action-button provider-action-button--secondary"
                        disabled={mergingConflict !== null || rejectingConflict !== null}
                        onClick={() => void mergeConflict(item, 'keep_source')}
                        type="button"
                      >
                        {mergingConflict === `${mergeKey}:keep_source`
                          ? 'Merging…'
                          : 'Merge, keep source IDs'}
                      </button>
                      <button
                        className="provider-action-button provider-action-button--secondary"
                        disabled={mergingConflict !== null || rejectingConflict !== null}
                        onClick={() => void mergeConflict(item, 'keep_target')}
                        type="button"
                      >
                        {mergingConflict === `${mergeKey}:keep_target`
                          ? 'Merging…'
                          : 'Merge, keep candidate IDs'}
                      </button>
                      <button
                        className="ghost-button"
                        disabled={mergingConflict !== null || rejectingConflict !== null}
                        onClick={() => void rejectConflict(item)}
                        type="button"
                      >
                        {rejectingConflict === `${mergeKey}:reject`
                          ? 'Marking…'
                          : 'Mark not same track'}
                      </button>
                    </div>
                  </article>
                )
              })}
            </div>
            <Pagination
              page={resource.data.page}
              totalPages={resource.data.total_pages}
              onPageChange={(nextPage) => {
                const next = new URLSearchParams(searchParams)
                next.set('page', String(nextPage))
                startTransition(() => setSearchParams(next))
              }}
            />
          </>
        )}
      </section>

      {editingTrackId ? (
        <TrackEditorModal
          trackId={editingTrackId}
          onClose={() => setEditingTrackId(null)}
        />
      ) : null}
    </section>
  )
}

function ConflictEvidencePanel({
  conflict,
  compact,
}: {
  conflict: TrackIdentityConflict
  compact?: boolean
}) {
  const evidence = conflict.evidence
  const recommendationClass = recommendationClassName(evidence.recommendation.key)

  return (
    <div className={`conflict-evidence${compact ? ' conflict-evidence--compact' : ''}`}>
      <div className="conflict-evidence-head">
        <span className={`status-chip ${recommendationClass}`}>
          {evidence.recommendation.label}
        </span>
        <p>{evidence.recommendation.detail}</p>
      </div>
      <div className="conflict-evidence-grid">
        <EvidenceMetric
          label="Metadata score"
          value={formatScorePercent(evidence.metadata_similarity)}
        />
        <EvidenceMetric
          label="Provider confidence"
          value={formatOptionalScorePercent(evidence.provider_confidence)}
        />
        <EvidenceMetric
          label="Duration delta"
          value={formatDurationDelta(evidence.duration_delta_seconds)}
        />
        <EvidenceMetric
          label="Source impact"
          value={`${formatNumber(evidence.source_saved_tracks)} saved · ${formatNumber(
            evidence.source_playlist_entries,
          )} refs`}
        />
        <EvidenceMetric
          label="Candidate impact"
          value={`${formatNumber(evidence.candidate_saved_tracks)} saved · ${formatNumber(
            evidence.candidate_playlist_entries,
          )} refs`}
        />
      </div>
    </div>
  )
}

function EvidenceMetric({ label, value }: { label: string; value: string }) {
  return (
    <div className="evidence-metric">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  )
}

function ConflictTrackCard({
  label,
  track,
  onEdit,
}: {
  label: string
  track: ConflictTrack
  onEdit: () => void
}) {
  return (
    <div className="conflict-track-card">
      <div className="conflict-track-main">
        <Artwork image={track.artwork_url} seed={track.track_id} size="row" title={track.title} />
        <div className="track-text">
          <span className="eyebrow">{label}</span>
          <strong>{track.title}</strong>
          <span>{track.artist_summary}</span>
          {track.album ? <span>{track.album}</span> : null}
        </div>
      </div>
      <div className="chip-row">
        <span className="meta-badge meta-badge--coverage">{track.coverage.short_label}</span>
        <span className="mini-chip">{track.saved_count} saved</span>
        <span className="mini-chip">{track.playlist_refs} playlist refs</span>
      </div>
      <ProviderChipRow providers={track.providers} />
      <button className="ghost-button" onClick={onEdit} type="button">
        Open row
      </button>
    </div>
  )
}

function PlaylistsPage() {
  const { revision, refresh, notify } = useRuntime()
  const confirm = useConfirm()
  const navigate = useNavigate()
  const { playlistId } = useParams()
  const [searchParams, setSearchParams] = useSearchParams()
  const [draft, setDraft] = useState(searchParams.get('q') ?? '')
  const [editing, setEditing] = useState<PlaylistSummary | null>(null)
  const [editingTrackId, setEditingTrackId] = useState<string | null>(null)
  const page = parsePage(searchParams.get('page'))
  const query = searchParams.get('q') ?? ''

  useEffect(() => {
    setDraft(query)
  }, [query])

  const listPath = `/playlists?page=${page}${query ? `&q=${encodeURIComponent(query)}` : ''}`
  const listResource = useApiResource<PageResponse<PlaylistSummary>>(listPath, revision)
  const activePlaylistId =
    playlistId ?? listResource.data?.items[0]?.playlist_id ?? null

  const detailResource = useApiResource<PlaylistDetail>(
    activePlaylistId ? `/playlists/${activePlaylistId}` : null,
    revision,
  )

  useEffect(() => {
    const firstPlaylist = listResource.data?.items[0]
    if (!playlistId && firstPlaylist) {
      const currentSearch = searchParams.toString()
      startTransition(() =>
        navigate(
          `/playlists/${firstPlaylist.playlist_id}${
            currentSearch ? `?${currentSearch}` : ''
          }`,
          { replace: true },
        ),
      )
    }
  }, [navigate, playlistId, listResource.data, searchParams])

  async function removeEntry(playlist: PlaylistDetail, entry: PlaylistEntry) {
    const accepted = await confirm({
      title: 'Remove playlist entry?',
      message: `"${entry.title}" will be removed from "${playlist.playlist.name}".`,
      details:
        'The canonical playlist changes first, then the app immediately tries to push the updated playlist shape to every connected provider that links to it.',
      confirmLabel: 'Remove entry',
      tone: 'warning',
    })
    if (!accepted) {
      return
    }
    try {
      const payload = await apiRequest<ActionResponse>(
        `/playlists/${playlist.playlist.playlist_id}/entries/${entry.entry_id}`,
        { method: 'DELETE' },
      )
      notify(actionMessage(payload))
      refresh()
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Delete failed.')
    }
  }

  async function deletePlaylist(playlist: PlaylistSummary) {
    const accepted = await confirm({
      title: 'Delete playlist?',
      message: `"${playlist.name}" will be removed from the canonical library.`,
      details:
        'The canonical playlist is deleted first, then the app immediately tries to delete the linked provider playlist on every connected provider.',
      confirmLabel: 'Delete playlist',
      tone: 'danger',
    })
    if (!accepted) {
      return
    }
    try {
      const payload = await apiRequest<ActionResponse>(
        `/playlists/${playlist.playlist_id}`,
        { method: 'DELETE' },
      )
      notify(actionMessage(payload))
      refresh()
      startTransition(() => navigate('/playlists'))
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Delete failed.')
    }
  }

  function submitSearch(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const next = new URLSearchParams()
    if (draft.trim()) {
      next.set('q', draft.trim())
    }
    next.set('page', '1')
    startTransition(() => setSearchParams(next))
  }

  return (
    <section className="page-stack">
      <PageHero
        eyebrow="Playlists"
        title="Canonical playlists."
        copy="Edit the playlist here. Then push the result back out."
      >
        <HeroStat
          label="Selected"
          value={detailResource.data?.playlist.name ?? 'Choose one'}
        />
      </PageHero>

      <section className="split-layout">
        <aside className="panel split-panel">
          <div className="panel-head">
            <div>
              <span className="eyebrow">Index</span>
              <h2>Playlist Shelf</h2>
            </div>
          </div>
          <div className="panel-body">
            <form className="searchbar searchbar--stacked" onSubmit={submitSearch}>
              <input
                value={draft}
                onChange={(event) => setDraft(event.target.value)}
                placeholder="Search playlists"
                type="search"
              />
              <button type="submit">Search</button>
            </form>
            {listResource.loading && !listResource.data ? (
              <LoadingState label="Loading playlists" compact />
            ) : listResource.error || !listResource.data ? (
              <ErrorState
                message={listResource.error ?? 'Failed to load playlists.'}
                compact
              />
            ) : listResource.data.items.length === 0 ? (
              <EmptyState
                title="No playlists found"
                copy="Try another filter or import more provider state."
                compact
              />
            ) : (
              <>
                <div className="playlist-list">
                  {listResource.data.items.map((playlist) => (
                    <button
                      className={`playlist-card${
                        activePlaylistId === playlist.playlist_id
                          ? ' playlist-card--active'
                          : ''
                      }`}
                      key={playlist.playlist_id}
                      onClick={() => {
                        startTransition(() =>
                          navigate(`/playlists/${playlist.playlist_id}${window.location.search}`),
                        )
                      }}
                      type="button"
                    >
                      <Artwork
                        image={playlist.artwork_url}
                        seed={playlist.playlist_id}
                        title={playlist.name}
                        size="playlist"
                      />
                      <div className="playlist-card-copy">
                        <strong>{playlist.name}</strong>
                        <span>
                          {playlist.entry_count} tracks
                          {playlist.description ? ` · ${playlist.description}` : ''}
                        </span>
                      </div>
                    </button>
                  ))}
                </div>
                <Pagination
                  page={listResource.data.page}
                  totalPages={listResource.data.total_pages}
                  onPageChange={(nextPage) => {
                    const next = new URLSearchParams(searchParams)
                    next.set('page', String(nextPage))
                    startTransition(() => setSearchParams(next))
                  }}
                  compact
                />
              </>
            )}
          </div>
        </aside>

        <section className="panel split-panel split-panel--wide">
          {detailResource.loading && !detailResource.data ? (
            <LoadingState label="Loading playlist detail" />
          ) : detailResource.error || !detailResource.data ? (
            <ErrorState
              message={
                activePlaylistId
                  ? detailResource.error ?? 'Failed to load playlist.'
                  : 'Choose a playlist from the left rail.'
              }
            />
          ) : (
            <>
              <div className="playlist-hero">
                <Artwork
                  image={detailResource.data.playlist.artwork_url}
                  seed={detailResource.data.playlist.playlist_id}
                  title={detailResource.data.playlist.name}
                  size="hero"
                />
                <div className="playlist-hero-copy">
                  <span className="eyebrow">Playlist Detail</span>
                  <h2>{detailResource.data.playlist.name}</h2>
                  <p>
                    {detailResource.data.playlist.description ??
                      'No description yet. This playlist still exists canonically even if provider coverage is uneven.'}
                  </p>
                  <div className="row-meta">
                    <span className="meta-badge">
                      {detailResource.data.playlist.entry_count} tracks
                    </span>
                    <ProviderChipRow providers={detailResource.data.playlist.providers} />
                    <StatusChipRow pills={detailResource.data.playlist.status_pills} />
                  </div>
                </div>
                <div className="playlist-hero-actions">
                  <IconButton
                    label="Edit playlist"
                    onClick={() => setEditing(detailResource.data!.playlist)}
                  >
                    <EditIcon />
                  </IconButton>
                  <IconButton
                    destructive
                    label="Delete playlist"
                    onClick={() => void deletePlaylist(detailResource.data!.playlist)}
                  >
                    <TrashIcon />
                  </IconButton>
                </div>
              </div>

              {detailResource.data.entries.length === 0 ? (
                <EmptyState
                  title="No entries"
                  copy="This playlist exists canonically but currently has no tracks."
                />
              ) : (
                <TrackList
                  items={detailResource.data.entries}
                  showAdded
                  onEdit={(entry) => setEditingTrackId(entry.track_id)}
                  onDelete={(entry) => void removeEntry(detailResource.data!, entry)}
                />
              )}
            </>
          )}
        </section>
      </section>

      {editing ? (
        <PlaylistEditorModal
          playlist={editing}
          onClose={() => setEditing(null)}
        />
      ) : null}
      {editingTrackId ? (
        <TrackEditorModal
          trackId={editingTrackId}
          onClose={() => setEditingTrackId(null)}
        />
      ) : null}
    </section>
  )
}

function TrackList<T extends SavedTrackItem | TrackItem | PlaylistEntry>({
  items,
  showAdded,
  usageMode,
  onEdit,
  onDelete,
}: {
  items: T[]
  showAdded?: boolean
  usageMode?: boolean
  onEdit: (item: T) => void
  onDelete: (item: T) => void
}) {
  return (
    <div className="track-list">
      {items.map((item) => (
        <article className="track-row" key={'saved_track_id' in item ? item.saved_track_id : 'entry_id' in item ? item.entry_id : item.track_id}>
          <div className="track-row-main">
            <Artwork
              image={item.artwork_url}
              seed={item.track_id}
              size="row"
              title={item.title}
            />
            <div className="track-text">
              <div className="track-title-line">
                <strong>{item.title}</strong>
                {item.coverage ? (
                  <span className="meta-badge meta-badge--coverage">
                    {item.coverage.short_label}
                  </span>
                ) : null}
              </div>
              <div className="track-subline">{item.subtitle || item.artist_summary}</div>
            </div>
          </div>

          <div className="track-row-meta">
            {'providers' in item ? <ProviderChipRow providers={item.providers} /> : null}
            {'status_pills' in item ? <StatusChipRow pills={item.status_pills} /> : null}
            {showAdded && 'added_label' in item ? (
              <span className="track-row-date">{item.added_label}</span>
            ) : null}
            {usageMode && 'saved_count' in item && 'playlist_refs' in item ? (
              <div className="usage-pills">
                <span className="mini-chip">{item.saved_count} saved</span>
                <span className="mini-chip">{item.playlist_refs} playlist refs</span>
              </div>
            ) : null}
            {!showAdded && 'duration_label' in item ? (
              <span className="track-row-duration">{item.duration_label}</span>
            ) : null}
          </div>

          <div className="track-row-actions">
            <IconButton label="Edit item" onClick={() => onEdit(item)}>
              <EditIcon />
            </IconButton>
            <IconButton destructive label="Remove item" onClick={() => onDelete(item)}>
              <TrashIcon />
            </IconButton>
          </div>
        </article>
      ))}
    </div>
  )
}

function ProviderChipRow({ providers }: { providers: ProviderBadge[] }) {
  if (providers.length === 0) {
    return <span className="status-chip status-chip--local">Local</span>
  }
  return (
    <div className="chip-row">
      {providers.map((provider) => (
        <span
          className="status-chip status-chip--provider"
          key={`${provider.key}-${provider.provider_id}`}
          title={`${provider.label} · ${provider.source} · ${provider.provider_id}`}
        >
          {provider.label}
        </span>
      ))}
    </div>
  )
}

function StatusChipRow({ pills }: { pills: StatusPill[] }) {
  return (
    <div className="chip-row">
      {pills.map((pill, index) => (
        <span
          className={`status-chip status-chip--${statusTone(pill.key)}`}
          key={`${pill.key}-${index}`}
          title={pill.title}
        >
          {pill.label}
        </span>
      ))}
    </div>
  )
}

function Artwork({
  image,
  seed,
  title,
  size,
}: {
  image: string | null
  seed: string
  title: string
  size: 'row' | 'playlist' | 'hero'
}) {
  const monogram = coverMonogram(title)
  const palette = coverPalette(seed)
  return (
    <div
      className={`artwork artwork--${size}`}
      style={
        {
          '--cover-a': palette[0],
          '--cover-b': palette[1],
        } as CSSProperties
      }
    >
      {image ? <img alt={`Artwork for ${title}`} loading="lazy" src={image} /> : null}
      {!image ? <span>{monogram}</span> : null}
    </div>
  )
}

function Pagination({
  page,
  totalPages,
  onPageChange,
  compact,
}: {
  page: number
  totalPages: number
  onPageChange: (page: number) => void
  compact?: boolean
}) {
  if (!totalPages || totalPages <= 1) {
    return null
  }
  return (
    <div className={`pagination${compact ? ' pagination--compact' : ''}`}>
      <button disabled={page <= 1} onClick={() => onPageChange(page - 1)} type="button">
        Previous
      </button>
      <span>
        Page {page} of {totalPages}
      </span>
      <button
        disabled={page >= totalPages}
        onClick={() => onPageChange(page + 1)}
        type="button"
      >
        Next
      </button>
    </div>
  )
}

function TrackEditorModal({
  trackId,
  onClose,
}: {
  trackId: string
  onClose: () => void
}) {
  const { revision, refresh, notify } = useRuntime()
  const confirm = useConfirm()
  const resource = useApiResource<TrackDetail>(`/tracks/${trackId}`, revision)
  const [title, setTitle] = useState('')
  const [artists, setArtists] = useState('')
  const [album, setAlbum] = useState('')
  const [duration, setDuration] = useState('')
  const [isrc, setIsrc] = useState('')
  const [identityProvider, setIdentityProvider] = useState('spotify')
  const [identityValue, setIdentityValue] = useState('')
  const [saving, setSaving] = useState(false)
  const [linkingIdentity, setLinkingIdentity] = useState(false)
  const [mergingConflict, setMergingConflict] = useState<string | null>(null)
  const [rejectingConflict, setRejectingConflict] = useState<string | null>(null)

  useEffect(() => {
    if (!resource.data) {
      return
    }
    setTitle(resource.data.title)
    setArtists(resource.data.artists.join('\n'))
    setAlbum(resource.data.album ?? '')
    setDuration(
      resource.data.duration_seconds ? String(resource.data.duration_seconds) : '',
    )
    setIsrc(resource.data.isrc ?? '')
  }, [resource.data])

  async function save() {
    if (!resource.data) {
      return
    }
    setSaving(true)
    try {
      const payload = await apiRequest<ActionResponse>(`/tracks/${trackId}`, {
        method: 'PATCH',
        body: JSON.stringify({
          title,
          artists: artists
            .split(/\n|,/)
            .map((artist) => artist.trim())
            .filter(Boolean),
          album: album || null,
          duration_seconds: duration ? Number(duration) : null,
          isrc: isrc || null,
        }),
      })
      notify(actionMessage(payload))
      refresh()
      onClose()
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Save failed.')
    } finally {
      setSaving(false)
    }
  }

  async function applyIdentity() {
    if (!resource.data || !identityValue.trim()) {
      return
    }

    const providerName =
      identityProvider === 'spotify' ? 'Spotify' : 'YouTube Music'
    const accepted = await confirm({
      title: `Link ${providerName} identity?`,
      message: `This will attach the pasted ${providerName} track identity to "${resource.data.title}".`,
      details:
        'If that provider ID already belongs to another canonical row, the app will merge the rows only when their other provider IDs do not conflict.',
      confirmLabel: 'Link identity',
      tone: 'warning',
    })
    if (!accepted) {
      return
    }

    setLinkingIdentity(true)
    try {
      const payload = await apiRequest<ApplyIdentityResponse>(
        `/tracks/${trackId}/identities`,
        {
          method: 'POST',
          body: JSON.stringify({
            provider: identityProvider,
            provider_id: identityValue,
          }),
        },
      )
      notify(payload.message)
      setIdentityValue('')
      refresh()
      if (payload.track_id !== trackId) {
        onClose()
      }
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Identity link failed.')
    } finally {
      setLinkingIdentity(false)
    }
  }

  async function mergeConflict(
    conflict: TrackIdentityConflict,
    conflictResolution: 'keep_source' | 'keep_target',
  ) {
    if (!resource.data) {
      return
    }

    const keepCurrent = conflictResolution === 'keep_source'
    const accepted = await confirm({
      title: keepCurrent ? 'Merge and keep current IDs?' : 'Merge and keep candidate IDs?',
      message: `This will merge "${resource.data.title}" into "${conflict.owner_track.title}".`,
      details: keepCurrent
        ? 'Saved tracks and playlist entries will move to the candidate row. For conflicting providers, the current row provider ID wins and the candidate alternate ID is recorded in audit status. Provider accounts are not changed.'
        : 'Saved tracks and playlist entries will move to the candidate row. For conflicting providers, the candidate row provider ID wins and the current alternate ID is recorded in audit status. Provider accounts are not changed.',
      confirmLabel: keepCurrent ? 'Merge, keep current' : 'Merge, keep candidate',
      tone: 'danger',
    })
    if (!accepted) {
      return
    }

    setMergingConflict(`${conflict.provider}:${conflict.provider_id}:${conflictResolution}`)
    try {
      const payload = await apiRequest<MergeTrackResponse>(`/tracks/${trackId}/merge`, {
        method: 'POST',
        body: JSON.stringify({
          target_track_id: conflict.owner_track.track_id,
          conflict_resolution: conflictResolution,
        }),
      })
      notify(payload.message)
      refresh()
      onClose()
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Merge failed.')
    } finally {
      setMergingConflict(null)
    }
  }

  async function rejectConflict(conflict: TrackIdentityConflict) {
    if (!resource.data) {
      return
    }

    const accepted = await confirm({
      title: 'Mark candidate as not same track?',
      message: `This will reject ${conflict.provider_name} candidate ${conflict.provider_id} for "${resource.data.title}".`,
      details:
        'The rows will not be merged. Provider accounts are not changed. This track will stay missing that provider ID until you link the correct identity or a different match is found.',
      confirmLabel: 'Mark not same',
      tone: 'warning',
    })
    if (!accepted) {
      return
    }

    const rejectKey = `${conflict.provider}:${conflict.provider_id}:reject`
    setRejectingConflict(rejectKey)
    try {
      const payload = await apiRequest<ActionResponse>(
        `/tracks/${trackId}/identity-conflicts/reject`,
        {
          method: 'POST',
          body: JSON.stringify({
            provider: conflict.provider,
            provider_id: conflict.provider_id,
            owner_track_id: conflict.owner_track.track_id,
          }),
        },
      )
      notify(actionMessage(payload))
      refresh()
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Reject failed.')
    } finally {
      setRejectingConflict(null)
    }
  }

  return (
    <ModalFrame title="Edit Track" onClose={onClose}>
      {resource.loading && !resource.data ? (
        <LoadingState label="Loading track detail" compact />
      ) : resource.error || !resource.data ? (
        <ErrorState message={resource.error ?? 'Track detail unavailable.'} compact />
      ) : (
        <div className="modal-stack">
          <div className="modal-track-head">
            <Artwork
              image={resource.data.artwork_url}
              seed={resource.data.track_id}
              size="playlist"
              title={resource.data.title}
            />
            <div>
              <strong>{resource.data.title}</strong>
              <p>{resource.data.artist_summary}</p>
              <div className="chip-row">
                <span className="mini-chip">{resource.data.coverage.label}</span>
                <span className="mini-chip">{resource.data.saved_count} saved</span>
                <span className="mini-chip">{resource.data.playlist_refs} playlist refs</span>
              </div>
            </div>
          </div>

          <label className="field">
            <span>Title</span>
            <input onChange={(event) => setTitle(event.target.value)} value={title} />
          </label>
          <label className="field">
            <span>Artists</span>
            <textarea
              onChange={(event) => setArtists(event.target.value)}
              rows={4}
              value={artists}
            />
          </label>
          <div className="field-grid">
            <label className="field">
              <span>Album</span>
              <input onChange={(event) => setAlbum(event.target.value)} value={album} />
            </label>
            <label className="field">
              <span>Duration (seconds)</span>
              <input
                onChange={(event) => setDuration(event.target.value)}
                type="number"
                value={duration}
              />
            </label>
          </div>
          <label className="field">
            <span>ISRC</span>
            <input onChange={(event) => setIsrc(event.target.value)} value={isrc} />
          </label>

          <div className="section-stack">
            <h3>Provider Links</h3>
            <ProviderChipRow providers={resource.data.providers} />
          </div>

          <div className="section-stack">
            <h3>Manual Identity Repair</h3>
            <p className="section-copy">
              Paste a Spotify track URL/ID or YouTube Music watch URL/video ID to repair a
              remaining unmatched or conflicted row.
            </p>
            <div className="identity-repair-form">
              <label className="field">
                <span>Provider</span>
                <select
                  onChange={(event) => setIdentityProvider(event.target.value)}
                  value={identityProvider}
                >
                  <option value="spotify">Spotify</option>
                  <option value="youtube-music">YouTube Music</option>
                </select>
              </label>
              <label className="field">
                <span>Track ID or URL</span>
                <input
                  onChange={(event) => setIdentityValue(event.target.value)}
                  placeholder={
                    identityProvider === 'spotify'
                      ? 'https://open.spotify.com/track/...'
                      : 'https://music.youtube.com/watch?v=...'
                  }
                  value={identityValue}
                />
              </label>
              <button
                className="provider-action-button provider-action-button--secondary"
                disabled={linkingIdentity || !identityValue.trim()}
                onClick={() => void applyIdentity()}
                type="button"
              >
                {linkingIdentity ? 'Linking…' : 'Link Identity'}
              </button>
            </div>
          </div>

          {resource.data.identity_conflicts.length ? (
            <div className="section-stack">
              <h3>Identity Conflicts</h3>
              <p className="section-copy">
                These provider matches point at another canonical row. Merge only after checking
                which provider identity should win.
              </p>
              <div className="provider-state-list">
                {resource.data.identity_conflicts.map((conflict) => {
                  const mergeKey = `${conflict.provider}:${conflict.provider_id}`
                  return (
                    <div className="provider-state-card" key={mergeKey}>
                      <strong>
                        {conflict.provider_name} candidate: {conflict.provider_id}
                      </strong>
                      <p>{conflict.message}</p>
                      <div className="chip-row">
                        <span className="mini-chip">
                          Candidate row: {conflict.owner_track.title}
                        </span>
                        <span className="mini-chip">
                          {conflict.owner_track.artist_summary}
                        </span>
                        <span className="mini-chip">
                          {conflict.owner_track.coverage.label}
                        </span>
                      </div>
                      <ConflictEvidencePanel conflict={conflict} compact />
                      {conflict.conflicting_provider_links.map((link) => (
                        <p key={link.provider}>
                          {link.provider_name}: current {link.source_provider_id} · candidate{' '}
                          {link.target_provider_id}
                        </p>
                      ))}
                      <div className="modal-actions modal-actions--inline">
                        <button
                          className="provider-action-button provider-action-button--secondary"
                          disabled={mergingConflict !== null || rejectingConflict !== null}
                          onClick={() => void mergeConflict(conflict, 'keep_source')}
                          type="button"
                        >
                          {mergingConflict === `${mergeKey}:keep_source`
                            ? 'Merging…'
                            : 'Merge, keep current IDs'}
                        </button>
                        <button
                          className="provider-action-button provider-action-button--secondary"
                          disabled={mergingConflict !== null || rejectingConflict !== null}
                          onClick={() => void mergeConflict(conflict, 'keep_target')}
                          type="button"
                        >
                          {mergingConflict === `${mergeKey}:keep_target`
                            ? 'Merging…'
                            : 'Merge, keep candidate IDs'}
                        </button>
                        <button
                          className="ghost-button"
                          disabled={mergingConflict !== null || rejectingConflict !== null}
                          onClick={() => void rejectConflict(conflict)}
                          type="button"
                        >
                          {rejectingConflict === `${mergeKey}:reject`
                            ? 'Marking…'
                            : 'Mark not same track'}
                        </button>
                      </div>
                    </div>
                  )
                })}
              </div>
            </div>
          ) : null}

          <div className="section-stack">
            <h3>Provider State</h3>
            <div className="provider-state-list">
              {resource.data.provider_status.map((status) => (
                <div className="provider-state-card" key={`${status.provider}-${status.state}`}>
                  <strong>{status.provider}</strong>
                  <span className={`status-chip status-chip--${statusTone(status.state)}`}>
                    {status.state}
                  </span>
                  <p>{status.message ?? 'No details recorded.'}</p>
                </div>
              ))}
            </div>
          </div>

          <div className="modal-actions">
            <button className="ghost-button" onClick={onClose} type="button">
              Cancel
            </button>
            <button disabled={saving} onClick={() => void save()} type="button">
              {saving ? 'Saving…' : 'Save Track'}
            </button>
          </div>
        </div>
      )}
    </ModalFrame>
  )
}

function PlaylistEditorModal({
  playlist,
  onClose,
}: {
  playlist: PlaylistSummary
  onClose: () => void
}) {
  const { notify, refresh } = useRuntime()
  const [name, setName] = useState(playlist.name)
  const [description, setDescription] = useState(playlist.description ?? '')
  const [saving, setSaving] = useState(false)

  async function save() {
    setSaving(true)
    try {
      const payload = await apiRequest<ActionResponse>(
        `/playlists/${playlist.playlist_id}`,
        {
          method: 'PATCH',
          body: JSON.stringify({
            name,
            description: description || null,
          }),
        },
      )
      notify(actionMessage(payload))
      refresh()
      onClose()
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Save failed.')
    } finally {
      setSaving(false)
    }
  }

  return (
    <ModalFrame title="Edit Playlist" onClose={onClose}>
      <div className="modal-stack">
        <label className="field">
          <span>Name</span>
          <input onChange={(event) => setName(event.target.value)} value={name} />
        </label>
        <label className="field">
          <span>Description</span>
          <textarea
            onChange={(event) => setDescription(event.target.value)}
            rows={5}
            value={description}
          />
        </label>
        <div className="chip-row">
          {playlist.providers.map((provider) => (
            <span className="mini-chip" key={`${provider.key}-${provider.provider_id}`}>
              {provider.label}
            </span>
          ))}
        </div>
        <div className="modal-actions">
          <button className="ghost-button" onClick={onClose} type="button">
            Cancel
          </button>
          <button disabled={saving} onClick={() => void save()} type="button">
            {saving ? 'Saving…' : 'Save Playlist'}
          </button>
        </div>
      </div>
    </ModalFrame>
  )
}

function OperationModal({
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
              <button className="ghost-button" onClick={onClose} type="button">
                Hide
              </button>
            ) : (
              <button onClick={onClose} type="button">
                Done
              </button>
            )}
          </div>
        </div>
      )}
    </ModalFrame>
  )
}

function operationTitle(kind: OperationKind) {
  switch (kind) {
    case 'verify':
      return 'Checking connection'
    case 'pull':
      return 'Pulling'
    case 'push':
      return 'Pushing'
    case 'reset_push':
      return 'Resetting & pushing'
    case 'identity':
      return 'Resolving identities'
    case 'identity_all':
      return 'Resolving library identities'
  }
}

function ProgressCard({
  label,
  done,
  total,
}: {
  label: string
  done: number
  total: number | null
}) {
  return (
    <div className="stat-tile">
      <strong>{total === null ? formatNumber(done) : `${formatNumber(done)} / ${formatNumber(total)}`}</strong>
      <span>{label}</span>
    </div>
  )
}

function ConfirmModal({
  request,
  onCancel,
  onConfirm,
}: {
  request: ConfirmState
  onCancel: () => void
  onConfirm: () => void
}) {
  return (
    <ModalFrame title={request.title} onClose={onCancel}>
      <div className="modal-stack">
        <div className="confirm-copy">
          <p>{request.message}</p>
          {request.details ? (
            <p className="confirm-details">{request.details}</p>
          ) : null}
        </div>
        <div
          className={`confirm-warning confirm-warning--${request.tone}`}
        >
          <strong>
            {request.tone === 'danger' ? 'Destructive action' : 'Confirm change'}
          </strong>
          <span>
            {request.tone === 'danger'
              ? 'This will update the canonical database immediately.'
              : 'This will change the canonical source of truth immediately.'}
          </span>
        </div>
        <div className="modal-actions">
          <button className="ghost-button" onClick={onCancel} type="button">
            Cancel
          </button>
          <button
            className={`confirm-button confirm-button--${request.tone}`}
            onClick={onConfirm}
            type="button"
          >
            {request.confirmLabel}
          </button>
        </div>
      </div>
    </ModalFrame>
  )
}

function ModalFrame({
  title,
  children,
  onClose,
}: {
  title: string
  children: ReactNode
  onClose: () => void
}) {
  return (
    <div className="modal-backdrop" onClick={onClose} role="presentation">
      <div
        className="modal"
        onClick={(event) => event.stopPropagation()}
        role="dialog"
      >
        <header className="modal-head">
          <div>
            <span className="eyebrow">Editor</span>
            <h2>{title}</h2>
          </div>
          <IconButton label="Close modal" onClick={onClose}>
            <CloseIcon />
          </IconButton>
        </header>
        {children}
      </div>
    </div>
  )
}

function PageHero({
  eyebrow,
  title,
  copy,
  children,
}: {
  eyebrow: string
  title: string
  copy: string
  children?: ReactNode
}) {
  return (
    <section className="hero-panel">
      <div className="hero-copy">
        <span className="eyebrow">{eyebrow}</span>
        <h2>{title}</h2>
        <p>{copy}</p>
      </div>
      <div className="hero-aside">{children}</div>
    </section>
  )
}

function HeroStat({ label, value }: { label: string; value: string }) {
  return (
    <div className="hero-stat">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  )
}

function DashboardCard({
  label,
  value,
  children,
}: {
  label: string
  value: number
  children: ReactNode
}) {
  return (
    <div className="dashboard-card">
      <span className="eyebrow">{label}</span>
      <strong>{formatNumber(value)}</strong>
      <p>{children}</p>
    </div>
  )
}

function StatTile({ label, value }: { label: string; value: number }) {
  return (
    <div className="stat-tile">
      <strong>{formatNumber(value)}</strong>
      <span>{label}</span>
    </div>
  )
}

function ReadinessItem({
  label,
  ready,
  blocked,
}: {
  label: string
  ready: number
  blocked: number
}) {
  const total = ready + blocked
  const percent = total === 0 ? 100 : Math.round((ready / total) * 100)
  return (
    <div className="readiness-item">
      <span>{label}</span>
      <strong>
        {formatNumber(ready)} / {formatNumber(total)}
      </strong>
      <small>{blocked > 0 ? `${formatNumber(blocked)} missing IDs` : `${percent}% ready`}</small>
    </div>
  )
}

function LoadingState({
  label,
  compact,
}: {
  label: string
  compact?: boolean
}) {
  return (
    <div className={`state-card${compact ? ' state-card--compact' : ''}`}>
      <div className="spinner" />
      <span>{label}</span>
    </div>
  )
}

function ErrorState({
  message,
  compact,
}: {
  message: string
  compact?: boolean
}) {
  return (
    <div className={`state-card state-card--error${compact ? ' state-card--compact' : ''}`}>
      <strong>Something failed</strong>
      <span>{message}</span>
    </div>
  )
}

function EmptyState({
  title,
  copy,
  compact,
}: {
  title: string
  copy: string
  compact?: boolean
}) {
  return (
    <div className={`state-card${compact ? ' state-card--compact' : ''}`}>
      <strong>{title}</strong>
      <span>{copy}</span>
    </div>
  )
}

function Toast({ message }: { message: string }) {
  return <div className="toast">{message}</div>
}

function IconButton({
  children,
  label,
  onClick,
  destructive,
}: {
  children: ReactNode
  label: string
  onClick: () => void
  destructive?: boolean
}) {
  return (
    <button
      aria-label={label}
      className={`icon-button${destructive ? ' icon-button--destructive' : ''}`}
      onClick={onClick}
      type="button"
      title={label}
    >
      {children}
    </button>
  )
}

function EditIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24">
      <path d="M12 20h9" />
      <path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z" />
    </svg>
  )
}

function TrashIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24">
      <path d="M3 6h18" />
      <path d="M8 6V4h8v2" />
      <path d="M19 6l-1 14H6L5 6" />
      <path d="M10 11v6" />
      <path d="M14 11v6" />
    </svg>
  )
}

function CloseIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24">
      <path d="m6 6 12 12" />
      <path d="M18 6 6 18" />
    </svg>
  )
}

function parsePage(raw: string | null) {
  if (!raw) {
    return 1
  }
  const value = Number(raw)
  return Number.isFinite(value) && value > 0 ? value : 1
}

function formatNumber(value: number) {
  return new Intl.NumberFormat().format(value)
}

function formatScorePercent(value: number) {
  return `${Math.round(value * 100)}%`
}

function formatOptionalScorePercent(value: number | null) {
  if (value === null) {
    return 'Unknown'
  }
  return formatScorePercent(value)
}

function formatDurationDelta(value: number | null) {
  if (value === null) {
    return 'Unknown'
  }
  if (value === 1) {
    return '1 sec'
  }
  return `${formatNumber(value)} sec`
}

function recommendationClassName(key: string) {
  if (key === 'likely_same_recording') {
    return 'status-chip--good'
  }
  if (key === 'likely_different_recording') {
    return 'status-chip--danger'
  }
  return 'status-chip--warning'
}

function formatDateTime(value: string) {
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(new Date(value))
}

function cooldownRemainingMs(cooldownUntil: string | null) {
  if (!cooldownUntil) {
    return 0
  }
  const until = new Date(cooldownUntil).getTime()
  if (!Number.isFinite(until)) {
    return 0
  }
  return Math.max(0, until - Date.now())
}

function formatDuration(valueMs: number) {
  const totalSeconds = Math.ceil(Math.max(0, valueMs) / 1000)
  if (totalSeconds <= 0) {
    return 'now'
  }

  const hours = Math.floor(totalSeconds / 3600)
  const minutes = Math.floor((totalSeconds % 3600) / 60)
  const seconds = totalSeconds % 60
  const parts = []

  if (hours > 0) {
    parts.push(`${hours}h`)
  }
  if (minutes > 0 || hours > 0) {
    parts.push(`${minutes}m`)
  }
  if (hours === 0 && minutes < 5) {
    parts.push(`${seconds}s`)
  }

  return parts.join(' ')
}

function providerCooldownCopy(provider: ProviderConnectionState) {
  const remainingMs = cooldownRemainingMs(provider.cooldown_until)
  if (!provider.cooldown_until || remainingMs <= 0) {
    return null
  }

  const reason = provider.cooldown_reason
    ? ` Provider response: ${provider.cooldown_reason}`
    : ''
  return `${provider.name} asked us to wait ${formatDuration(remainingMs)}. The app will avoid ${provider.name} API calls until ${formatDateTime(provider.cooldown_until)}.${reason}`
}

function formatBytes(value: number) {
  if (value < 1024) {
    return `${value} B`
  }
  const units = ['KB', 'MB', 'GB', 'TB']
  let current = value / 1024
  let unitIndex = 0
  while (current >= 1024 && unitIndex < units.length - 1) {
    current /= 1024
    unitIndex += 1
  }
  return `${current.toFixed(current >= 10 ? 0 : 1)} ${units[unitIndex]}`
}

function coverMonogram(title: string) {
  const parts = title
    .split(/[^a-zA-Z0-9]+/)
    .filter(Boolean)
    .slice(0, 2)
  return parts.map((part) => part[0].toUpperCase()).join('') || 'TR'
}

function coverPalette(seed: string) {
  const palettes: [string, string][] = [
    ['#46c2ff', '#18202c'],
    ['#ff5f6d', '#240913'],
    ['#5be7c4', '#102620'],
    ['#ffb347', '#2e170a'],
    ['#c77dff', '#180828'],
    ['#73fbd3', '#101f1e'],
    ['#ffd166', '#33210c'],
    ['#ff6b6b', '#250f12'],
  ]
  let hash = 0
  for (const character of seed) {
    hash = (hash + character.charCodeAt(0)) % palettes.length
  }
  return palettes[hash]
}

function statusTone(key: string) {
  if (key === 'error') {
    return 'danger'
  }
  if (key === 'unmatched' || key === 'missing') {
    return 'warning'
  }
  if (key === 'synced') {
    return 'good'
  }
  if (key === 'provider') {
    return 'provider'
  }
  return 'local'
}

function coverageLabel(value: string) {
  if (value === 'missing-any-provider') {
    return 'Missing any ID'
  }
  if (value === 'missing-spotify') {
    return 'Missing Spotify ID'
  }
  if (value === 'missing-youtube-music') {
    return 'Missing YouTube ID'
  }
  if (value === 'spotify-only') {
    return 'Spotify only'
  }
  if (value === 'youtube-music-only') {
    return 'YouTube only'
  }
  if (value === 'multi-provider') {
    return 'Multi-provider'
  }
  if (value === 'canonical-only') {
    return 'Canonical only'
  }
  if (value === 'identity-conflicts') {
    return 'Identity conflicts'
  }
  if (value === 'unmatched') {
    return 'Unmatched'
  }
  return 'All coverage'
}

function identityGapProviderLabel(value: string) {
  if (value === 'spotify') {
    return 'Spotify'
  }
  if (value === 'youtube-music') {
    return 'YouTube Music'
  }
  return 'All providers'
}

function identityConflictProviderLabel(value: string) {
  if (value === 'spotify') {
    return 'Spotify candidates'
  }
  if (value === 'youtube-music') {
    return 'YouTube candidates'
  }
  return 'All providers'
}

function identityConflictRecommendationLabel(value: string) {
  if (value === 'likely_same_recording') {
    return 'Likely same'
  }
  if (value === 'needs_manual_review') {
    return 'Manual review'
  }
  if (value === 'likely_different_recording') {
    return 'Likely different'
  }
  return 'All recommendations'
}

export default App
