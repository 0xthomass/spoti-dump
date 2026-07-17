import { useEffect, useState } from 'react'
import { Link } from 'react-router-dom'
import type {
  ActionResponse,
  Overview,
  OperationStartResponse,
  ProviderConnectionState,
  ProviderPushPlan,
  ProvidersResponse,
} from '../api/types'
import { actionMessage, apiRequest } from '../api/client'
import { useConfirm, useRuntime } from '../context/runtime'
import { useApiQuery } from '../hooks/useApiQuery'
import {
  cooldownRemainingMs,
  formatDateTime,
  formatDuration,
  formatNumber,
  providerCooldownCopy,
} from '../lib/format'
import { DashboardCard } from '../components/DashboardCard'
import { ErrorState } from '../components/ErrorState'
import { HeroStat } from '../components/HeroStat'
import { LoadingState } from '../components/LoadingState'
import { PageHero } from '../components/PageHero'
import { PushPlanSummary } from '../components/PushPlanSummary'
import { ReadinessItem } from '../components/ReadinessItem'
import { StatTile } from '../components/StatTile'
import { SpotifyConnectModal } from '../components/modals/SpotifyConnectModal'
import { YoutubeMusicConnectModal } from '../components/modals/YoutubeMusicConnectModal'

export function OverviewPage() {
  const { revision, refresh, notify, openOperation } = useRuntime()
  const confirm = useConfirm()
  const [spotifyModalOpen, setSpotifyModalOpen] = useState(false)
  const [youtubeModalOpen, setYoutubeModalOpen] = useState(false)
  const [pendingAction, setPendingAction] = useState<string | null>(null)
  const [pushPlans, setPushPlans] = useState<Record<string, ProviderPushPlan>>({})
  const [loadingPushPlan, setLoadingPushPlan] = useState<string | null>(null)
  const overviewResource = useApiQuery<Overview>('/overview', revision)
  const providersResource = useApiQuery<ProvidersResponse>('/providers', revision)

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
      notify(error instanceof Error ? error.message : 'Provider action failed.', {
        tone: 'error',
      })
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
      notify(error instanceof Error ? error.message : 'Identity sync failed.', {
        tone: 'error',
      })
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
      notify(error instanceof Error ? error.message : 'Failed to load push plan.', {
        tone: 'error',
      })
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
        onRetry={() => {
          overviewResource.refetch()
          providersResource.refetch()
        }}
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
            className="btn btn--primary"
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
                    className="btn btn--primary"
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
                    className="btn btn--secondary"
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
                    className="btn btn--secondary"
                    disabled={providerActionDisabled || !provider.preflight.can_pull}
                    onClick={() => void runProviderAction(provider, 'export')}
                    type="button"
                    title={preflightTitle}
                  >
                    {pendingAction === `${provider.key}:export` ? 'Pulling…' : 'Pull Library'}
                  </button>
                  <button
                    className="btn btn--secondary"
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
                    className="btn btn--secondary"
                    disabled={providerActionDisabled || !provider.preflight.can_push}
                    onClick={() => void runProviderAction(provider, 'sync')}
                    type="button"
                    title={preflightTitle}
                  >
                    {pendingAction === `${provider.key}:sync` ? 'Pushing…' : 'Push Changes'}
                  </button>
                  <button
                    className="btn btn--secondary"
                    disabled={loadingPushPlan !== null}
                    onClick={() => void loadPushPlan(provider)}
                    type="button"
                  >
                    {loadingPushPlan === provider.key ? 'Planning…' : 'Push Plan'}
                  </button>
                  {provider.key === 'spotify' ? (
                    <button
                      className="btn btn--danger"
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
                      className="btn btn--ghost"
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
