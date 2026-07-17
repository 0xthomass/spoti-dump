import { startTransition, useEffect, useState } from 'react'
import type { FormEvent } from 'react'
import { useSearchParams } from 'react-router-dom'
import type { IdentityGapQueueItem, PageResponse } from '../api/types'
import { useRuntime } from '../context/runtime'
import { useApiQuery } from '../hooks/useApiQuery'
import { formatNumber, identityGapProviderLabel, parsePage } from '../lib/format'
import { ConflictTrackCard } from '../components/ConflictTrackCard'
import { EmptyState } from '../components/EmptyState'
import { ErrorState } from '../components/ErrorState'
import { HeroStat } from '../components/HeroStat'
import { LoadingState } from '../components/LoadingState'
import { PageHero } from '../components/PageHero'
import { Pagination } from '../components/Pagination'
import { TrackEditorModal } from '../components/modals/TrackEditorModal'

export function IdentityGapsPage() {
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

  const resource = useApiQuery<PageResponse<IdentityGapQueueItem>>(
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
        title="Provider ID gaps"
        subtitle="Tracks still missing a provider ID."
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
            <button className="btn btn--primary" type="submit">Search</button>
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
          <ErrorState
            message={resource.error ?? 'Failed to load ID gaps.'}
            compact
            onRetry={resource.refetch}
          />
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
