import { startTransition, useEffect, useState } from 'react'
import type { FormEvent } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router-dom'
import type {
  ActionResponse,
  PageResponse,
  PlaylistDetail,
  PlaylistEntry,
  PlaylistSummary,
} from '../api/types'
import { actionMessage, apiRequest } from '../api/client'
import { useConfirm, useRuntime } from '../context/runtime'
import { useApiQuery } from '../hooks/useApiQuery'
import { parsePage } from '../lib/format'
import { Artwork } from '../components/Artwork'
import { EmptyState } from '../components/EmptyState'
import { ErrorState } from '../components/ErrorState'
import { HeroStat } from '../components/HeroStat'
import { IconButton } from '../components/IconButton'
import { LoadingState } from '../components/LoadingState'
import { PageHero } from '../components/PageHero'
import { Pagination } from '../components/Pagination'
import { ProviderChipRow } from '../components/ProviderChipRow'
import { StatusChipRow } from '../components/StatusChipRow'
import { TrackList } from '../components/TrackList'
import { EditIcon, TrashIcon } from '../components/icons'
import { PlaylistEditorModal } from '../components/modals/PlaylistEditorModal'
import { TrackEditorModal } from '../components/modals/TrackEditorModal'

export function PlaylistsPage() {
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
  const listResource = useApiQuery<PageResponse<PlaylistSummary>>(listPath, revision)
  const activePlaylistId =
    playlistId ?? listResource.data?.items[0]?.playlist_id ?? null

  const detailResource = useApiQuery<PlaylistDetail>(
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
      notify(error instanceof Error ? error.message : 'Delete failed.', {
        tone: 'error',
      })
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
      notify(error instanceof Error ? error.message : 'Delete failed.', {
        tone: 'error',
      })
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
              <button className="btn btn--primary" type="submit">Search</button>
            </form>
            {listResource.loading && !listResource.data ? (
              <LoadingState label="Loading playlists" compact />
            ) : listResource.error || !listResource.data ? (
              <ErrorState
                message={listResource.error ?? 'Failed to load playlists.'}
                compact
                onRetry={listResource.refetch}
              />
            ) : listResource.data.items.length === 0 ? (
              !query && listResource.data.total === 0 ? (
                <EmptyState
                  title="No playlists yet"
                  copy="Connect a provider and pull your library to import playlists here."
                  compact
                  action={
                    <Link className="btn btn--primary" to="/overview">
                      Connect a provider
                    </Link>
                  }
                />
              ) : (
                <EmptyState
                  title="No playlists found"
                  copy="Try another filter or import more provider state."
                  compact
                />
              )
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
              onRetry={activePlaylistId ? detailResource.refetch : undefined}
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
