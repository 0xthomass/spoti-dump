import { startTransition, useEffect, useState } from 'react'
import type { FormEvent } from 'react'
import { useSearchParams } from 'react-router-dom'
import type { ActionResponse, PageResponse, TrackItem } from '../api/types'
import { actionMessage, apiRequest } from '../api/client'
import { useConfirm, useRuntime } from '../context/runtime'
import { useApiQuery } from '../hooks/useApiQuery'
import { coverageLabel, parsePage } from '../lib/format'
import { EmptyState } from '../components/EmptyState'
import { ErrorState } from '../components/ErrorState'
import { HeroStat } from '../components/HeroStat'
import { LoadingState } from '../components/LoadingState'
import { PageHero } from '../components/PageHero'
import { Pagination } from '../components/Pagination'
import { TrackList } from '../components/TrackList'
import { TrackEditorModal } from '../components/modals/TrackEditorModal'

export function TracksPage() {
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
  const resource = useApiQuery<PageResponse<TrackItem>>(path, revision)

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
            <button className="btn btn--primary" type="submit">Search</button>
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
