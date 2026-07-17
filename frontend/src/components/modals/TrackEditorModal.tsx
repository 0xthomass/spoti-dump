import { useEffect, useRef, useState } from 'react'
import type {
  ActionResponse,
  ApplyIdentityResponse,
  MergeTrackResponse,
  TrackDetail,
  TrackIdentityConflict,
} from '../../api/types'
import { actionMessage, apiRequest } from '../../api/client'
import { useConfirm, useRuntime } from '../../context/runtime'
import { useApiQuery } from '../../hooks/useApiQuery'
import { statusTone } from '../../lib/format'
import { Artwork } from '../Artwork'
import { ConflictEvidencePanel } from '../ConflictEvidencePanel'
import { ErrorState } from '../ErrorState'
import { LoadingState } from '../LoadingState'
import { ProviderChipRow } from '../ProviderChipRow'
import { ModalFrame } from './ModalFrame'

type EditorSnapshot = {
  title: string
  artists: string[]
  album: string
  duration: string
  isrc: string
}

export function TrackEditorModal({
  trackId,
  onClose,
}: {
  trackId: string
  onClose: () => void
}) {
  const { revision, refresh, notify } = useRuntime()
  const confirm = useConfirm()
  const resource = useApiQuery<TrackDetail>(`/tracks/${trackId}`, revision)
  const [title, setTitle] = useState('')
  const [artists, setArtists] = useState<string[]>([])
  const [album, setAlbum] = useState('')
  const [duration, setDuration] = useState('')
  const [isrc, setIsrc] = useState('')
  const [identityProvider, setIdentityProvider] = useState('spotify')
  const [identityValue, setIdentityValue] = useState('')
  const [saving, setSaving] = useState(false)
  const [linkingIdentity, setLinkingIdentity] = useState(false)
  const [mergingConflict, setMergingConflict] = useState<string | null>(null)
  const [rejectingConflict, setRejectingConflict] = useState<string | null>(null)

  // Track which entity the form was seeded from. Re-seeding only happens when a
  // genuinely different track loads — a background refetch of the same track
  // (revision bump) must not clobber in-progress edits.
  const loadedTrackIdRef = useRef<string | null>(null)
  const initialRef = useRef<EditorSnapshot>({
    title: '',
    artists: [],
    album: '',
    duration: '',
    isrc: '',
  })

  useEffect(() => {
    const data = resource.data
    if (!data || loadedTrackIdRef.current === data.track_id) {
      return
    }
    loadedTrackIdRef.current = data.track_id
    const snapshot: EditorSnapshot = {
      title: data.title,
      artists: data.artists,
      album: data.album ?? '',
      duration: data.duration_seconds ? String(data.duration_seconds) : '',
      isrc: data.isrc ?? '',
    }
    initialRef.current = snapshot
    setTitle(snapshot.title)
    setArtists(snapshot.artists)
    setAlbum(snapshot.album)
    setDuration(snapshot.duration)
    setIsrc(snapshot.isrc)
  }, [resource.data])

  const initial = initialRef.current
  const dirty =
    loadedTrackIdRef.current !== null &&
    (title !== initial.title ||
      album !== initial.album ||
      duration !== initial.duration ||
      isrc !== initial.isrc ||
      artists.length !== initial.artists.length ||
      artists.some((artist, index) => artist !== initial.artists[index]))

  async function requestClose() {
    if (dirty) {
      const discard = await confirm({
        title: 'Discard unsaved changes?',
        message: 'You have edited this track but not saved those changes yet.',
        details: 'Closing now will lose the edits. This does not touch providers.',
        confirmLabel: 'Discard changes',
        tone: 'danger',
      })
      if (!discard) {
        return
      }
    }
    onClose()
  }

  function updateArtist(index: number, value: string) {
    setArtists((current) =>
      current.map((artist, position) => (position === index ? value : artist)),
    )
  }

  function addArtist() {
    setArtists((current) => [...current, ''])
  }

  function removeArtist(index: number) {
    setArtists((current) => current.filter((_, position) => position !== index))
  }

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
      notify(error instanceof Error ? error.message : 'Save failed.', {
        tone: 'error',
      })
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
      notify(error instanceof Error ? error.message : 'Identity link failed.', {
        tone: 'error',
      })
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
      notify(error instanceof Error ? error.message : 'Merge failed.', {
        tone: 'error',
      })
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
      notify(error instanceof Error ? error.message : 'Reject failed.', {
        tone: 'error',
      })
    } finally {
      setRejectingConflict(null)
    }
  }

  return (
    <ModalFrame title="Edit Track" onClose={() => void requestClose()}>
      {resource.loading && !resource.data ? (
        <LoadingState label="Loading track detail" compact />
      ) : resource.error || !resource.data ? (
        <ErrorState
          message={resource.error ?? 'Track detail unavailable.'}
          compact
          onRetry={resource.refetch}
        />
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
          <div className="field">
            <span>Artists</span>
            <div className="artist-editor">
              {artists.map((artist, index) => (
                <div className="artist-row" key={index}>
                  <input
                    aria-label={`Artist ${index + 1}`}
                    onChange={(event) => updateArtist(index, event.target.value)}
                    value={artist}
                  />
                  <button
                    aria-label={`Remove artist ${index + 1}`}
                    className="btn btn--ghost btn--sm"
                    onClick={() => removeArtist(index)}
                    type="button"
                  >
                    Remove
                  </button>
                </div>
              ))}
              <div className="artist-editor-actions">
                <button
                  className="btn btn--ghost btn--sm"
                  onClick={addArtist}
                  type="button"
                >
                  Add artist
                </button>
              </div>
            </div>
          </div>
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
                className="btn btn--secondary"
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
                          className="btn btn--secondary"
                          disabled={mergingConflict !== null || rejectingConflict !== null}
                          onClick={() => void mergeConflict(conflict, 'keep_source')}
                          type="button"
                        >
                          {mergingConflict === `${mergeKey}:keep_source`
                            ? 'Merging…'
                            : 'Merge, keep current IDs'}
                        </button>
                        <button
                          className="btn btn--secondary"
                          disabled={mergingConflict !== null || rejectingConflict !== null}
                          onClick={() => void mergeConflict(conflict, 'keep_target')}
                          type="button"
                        >
                          {mergingConflict === `${mergeKey}:keep_target`
                            ? 'Merging…'
                            : 'Merge, keep candidate IDs'}
                        </button>
                        <button
                          className="btn btn--ghost"
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
            <button className="btn btn--ghost" onClick={onClose} type="button">
              Cancel
            </button>
            <button className="btn btn--primary" disabled={saving} onClick={() => void save()} type="button">
              {saving ? 'Saving…' : 'Save Track'}
            </button>
          </div>
        </div>
      )}
    </ModalFrame>
  )
}
