import { startTransition, useEffect, useState } from 'react'
import type { FormEvent } from 'react'
import { useSearchParams } from 'react-router-dom'
import type {
  ActionResponse,
  BulkMergeIdentityConflictsPlan,
  BulkMergeIdentityConflictsResponse,
  IdentityConflictQueueItem,
  MergeTrackResponse,
  PageResponse,
} from '../api/types'
import { actionMessage, apiRequest, bulkMergeMessage } from '../api/client'
import { useConfirm, useRuntime } from '../context/runtime'
import { useApiQuery } from '../hooks/useApiQuery'
import {
  formatNumber,
  identityConflictProviderLabel,
  identityConflictQueryString,
  identityConflictRecommendationLabel,
  parsePage,
} from '../lib/format'
import { BulkMergeConflictPanel } from '../components/BulkMergeConflictPanel'
import { ConflictEvidencePanel } from '../components/ConflictEvidencePanel'
import { ConflictTrackCard } from '../components/ConflictTrackCard'
import { EmptyState } from '../components/EmptyState'
import { ErrorState } from '../components/ErrorState'
import { HeroStat } from '../components/HeroStat'
import { LoadingState } from '../components/LoadingState'
import { PageHero } from '../components/PageHero'
import { Pagination } from '../components/Pagination'
import { TrackEditorModal } from '../components/modals/TrackEditorModal'

export function IdentityConflictsPage() {
  const { revision, refresh, notify } = useRuntime()
  const confirm = useConfirm()
  const [searchParams, setSearchParams] = useSearchParams()
  const [draft, setDraft] = useState(searchParams.get('q') ?? '')
  const [editingTrackId, setEditingTrackId] = useState<string | null>(null)
  const [mergingConflict, setMergingConflict] = useState<string | null>(null)
  const [rejectingConflict, setRejectingConflict] = useState<string | null>(null)
  const [runningBulkMerge, setRunningBulkMerge] = useState<string | null>(null)
  const page = parsePage(searchParams.get('page'))
  const query = searchParams.get('q') ?? ''
  const provider = searchParams.get('provider') ?? ''
  const recommendation = searchParams.get('recommendation') ?? ''
  const impact = searchParams.get('impact') ?? ''
  const canBulkMergeRecommendation =
    recommendation === '' || recommendation === 'likely_same_recording'

  useEffect(() => {
    setDraft(query)
  }, [query])

  const resource = useApiQuery<PageResponse<IdentityConflictQueueItem>>(
    `/identity/conflicts?${identityConflictQueryString({
      page,
      query,
      provider,
      recommendation,
      impact,
    })}`,
    revision,
  )
  const bulkPlan = useApiQuery<BulkMergeIdentityConflictsPlan>(
    canBulkMergeRecommendation
      ? `/identity/conflicts/bulk-merge-plan?${identityConflictQueryString({
          query,
          provider,
          impact,
        })}`
      : null,
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
      notify(error instanceof Error ? error.message : 'Merge failed.', {
        tone: 'error',
      })
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
      notify(error instanceof Error ? error.message : 'Reject failed.', {
        tone: 'error',
      })
    } finally {
      setRejectingConflict(null)
    }
  }

  async function bulkMergeLikelySame(
    conflictResolution: 'keep_source' | 'keep_target',
  ) {
    if (!bulkPlan.data || bulkPlan.data.eligible_count === 0) {
      return
    }
    const keepSource = conflictResolution === 'keep_source'
    const accepted = await confirm({
      title: keepSource
        ? 'Bulk merge likely-same rows and keep source IDs?'
        : 'Bulk merge likely-same rows and keep candidate IDs?',
      message: `This will merge ${formatNumber(
        bulkPlan.data.eligible_count,
      )} likely-same identity conflict(s) matching the current search, provider, and impact filters.`,
      details: keepSource
        ? 'The app creates a manual source-of-truth backup first. Saved tracks and playlist entries move to the candidate rows. For conflicting providers, source row provider IDs win. Provider accounts are not changed.'
        : 'The app creates a manual source-of-truth backup first. Saved tracks and playlist entries move to the candidate rows. For conflicting providers, candidate row provider IDs win. Provider accounts are not changed.',
      confirmLabel: keepSource ? 'Bulk merge, keep source' : 'Bulk merge, keep candidate',
      tone: 'danger',
    })
    if (!accepted) {
      return
    }

    setRunningBulkMerge(conflictResolution)
    try {
      const payload = await apiRequest<BulkMergeIdentityConflictsResponse>(
        '/identity/conflicts/bulk-merge',
        {
          method: 'POST',
          body: JSON.stringify({
            q: query || null,
            provider: provider || null,
            impact: impact || null,
            conflict_resolution: conflictResolution,
          }),
        },
      )
      notify(bulkMergeMessage(payload))
      refresh()
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Bulk merge failed.', {
        tone: 'error',
      })
    } finally {
      setRunningBulkMerge(null)
    }
  }

  return (
    <section className="page-stack">
      <PageHero
        title="Identity conflicts"
        subtitle="Review ambiguous matches before merging."
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
            <button className="btn btn--primary" type="submit">Search</button>
          </form>
          <div className="filter-group">
            <span className="filter-caption" id="conflict-filter-provider">
              Provider
            </span>
            <div
              aria-labelledby="conflict-filter-provider"
              className="filter-row"
              role="group"
            >
              {[
                ['', 'All providers'],
                ['spotify', 'Spotify candidate'],
                ['youtube-music', 'YouTube candidate'],
              ].map(([value, label]) => (
                <button
                  aria-pressed={provider === value}
                  className={`filter-pill${provider === value ? ' filter-pill--active' : ''}`}
                  key={value || 'all-providers'}
                  onClick={() => changeConflictFilter('provider', value)}
                  type="button"
                >
                  {label}
                </button>
              ))}
            </div>
          </div>
          <div className="filter-group">
            <span className="filter-caption" id="conflict-filter-recommendation">
              Recommendation
            </span>
            <div
              aria-labelledby="conflict-filter-recommendation"
              className="filter-row"
              role="group"
            >
              {[
                ['', 'All recommendations'],
                ['likely_same_recording', 'Likely same'],
                ['needs_manual_review', 'Manual review'],
                ['likely_different_recording', 'Likely different'],
              ].map(([value, label]) => (
                <button
                  aria-pressed={recommendation === value}
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
          </div>
          <div className="filter-group">
            <span className="filter-caption" id="conflict-filter-impact">
              Impact
            </span>
            <div
              aria-labelledby="conflict-filter-impact"
              className="filter-row"
              role="group"
            >
              {[
                ['', 'All impact'],
                ['library_impact', 'Affects saved/playlists'],
                ['source_impact', 'Source row impact'],
                ['candidate_impact', 'Candidate row impact'],
              ].map(([value, label]) => (
                <button
                  aria-pressed={impact === value}
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
          {canBulkMergeRecommendation ? (
            <BulkMergeConflictPanel
              plan={bulkPlan.data}
              loading={bulkPlan.loading}
              error={bulkPlan.error}
              running={runningBulkMerge}
              onMerge={(resolution) => void bulkMergeLikelySame(resolution)}
            />
          ) : (
            <div className="conflict-bulk-panel">
              <strong>Bulk merge unavailable for this recommendation filter</strong>
              <p>
                Bulk merging is intentionally limited to conflicts classified as likely same
                recording. Switch the recommendation filter back to All or Likely same to plan a
                guarded bulk merge.
              </p>
            </div>
          )}
        </div>

        {resource.loading && !resource.data ? (
          <LoadingState label="Loading identity conflicts" compact />
        ) : resource.error || !resource.data ? (
          <ErrorState
            message={resource.error ?? 'Failed to load identity conflicts.'}
            compact
            onRetry={resource.refetch}
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
                const evidence = item.conflict.evidence
                // Recommended direction = keep the IDs of the row carrying more
                // library weight (saved tracks + playlist refs); ties favour the
                // candidate owner. We only surface it as "Recommended" when the
                // evidence classifies the pair as the same recording.
                const sourceWeight =
                  evidence.source_saved_tracks + evidence.source_playlist_entries
                const candidateWeight =
                  evidence.candidate_saved_tracks + evidence.candidate_playlist_entries
                const recommendedResolution: 'keep_source' | 'keep_target' =
                  sourceWeight > candidateWeight ? 'keep_source' : 'keep_target'
                const promoteMerge =
                  evidence.recommendation.key === 'likely_same_recording'
                const conflictProviderNames =
                  item.conflict.conflicting_provider_links
                    .map((link) => link.provider_name)
                    .join(', ') || item.conflict.provider_name
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

                    <div className="merge-decision">
                      <div className="merge-option">
                        <div className="merge-option-head">
                          <button
                            className={`btn ${
                              promoteMerge && recommendedResolution === 'keep_source'
                                ? 'btn--primary'
                                : 'btn--secondary'
                            }`}
                            disabled={
                              mergingConflict !== null || rejectingConflict !== null
                            }
                            onClick={() => void mergeConflict(item, 'keep_source')}
                            type="button"
                          >
                            {mergingConflict === `${mergeKey}:keep_source`
                              ? 'Merging…'
                              : "Keep this track's IDs"}
                          </button>
                          {promoteMerge && recommendedResolution === 'keep_source' ? (
                            <span className="status-chip status-chip--good merge-recommend">
                              Recommended
                            </span>
                          ) : null}
                        </div>
                        <span className="merge-option-hint">
                          {conflictProviderNames} ID from this track wins on conflicts.
                        </span>
                      </div>
                      <div className="merge-option">
                        <div className="merge-option-head">
                          <button
                            className={`btn ${
                              promoteMerge && recommendedResolution === 'keep_target'
                                ? 'btn--primary'
                                : 'btn--secondary'
                            }`}
                            disabled={
                              mergingConflict !== null || rejectingConflict !== null
                            }
                            onClick={() => void mergeConflict(item, 'keep_target')}
                            type="button"
                          >
                            {mergingConflict === `${mergeKey}:keep_target`
                              ? 'Merging…'
                              : "Use candidate's IDs"}
                          </button>
                          {promoteMerge && recommendedResolution === 'keep_target' ? (
                            <span className="status-chip status-chip--good merge-recommend">
                              Recommended
                            </span>
                          ) : null}
                        </div>
                        <span className="merge-option-hint">
                          {conflictProviderNames} ID from the candidate wins on conflicts.
                        </span>
                      </div>
                      <div className="merge-option merge-option--reject">
                        <button
                          className="btn btn--ghost"
                          disabled={mergingConflict !== null || rejectingConflict !== null}
                          onClick={() => void rejectConflict(item)}
                          type="button"
                        >
                          {rejectingConflict === `${mergeKey}:reject`
                            ? 'Marking…'
                            : 'Mark not same track'}
                        </button>
                        <span className="merge-option-hint">
                          Keep both rows separate; no provider IDs change.
                        </span>
                      </div>
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
