import { startTransition, useEffect, useState } from 'react'
import type { FormEvent } from 'react'
import { useSearchParams } from 'react-router-dom'
import type { ActionResponse, PageResponse, SavedTrackItem } from '../api/types'
import { actionMessage, apiRequest } from '../api/client'
import { useConfirm, useRuntime } from '../context/runtime'
import { useApiQuery } from '../hooks/useApiQuery'
import { formatNumber, parsePage } from '../lib/format'
import { EmptyState } from '../components/EmptyState'
import { ErrorState } from '../components/ErrorState'
import { HeroStat } from '../components/HeroStat'
import { LoadingState } from '../components/LoadingState'
import { PageHero } from '../components/PageHero'
import { Pagination } from '../components/Pagination'
import { TrackList } from '../components/TrackList'
import { TrackEditorModal } from '../components/modals/TrackEditorModal'

export function SavedTracksPage() {
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

  const resource = useApiQuery<PageResponse<SavedTrackItem>>(
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
            <button className="btn btn--primary" type="submit">Search</button>
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
