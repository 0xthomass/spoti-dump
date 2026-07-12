import { useState } from 'react'
import type { ActionResponse, PlaylistSummary } from '../../api/types'
import { actionMessage, apiRequest } from '../../api/client'
import { useRuntime } from '../../context/runtime'
import { ModalFrame } from './ModalFrame'

export function PlaylistEditorModal({
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
          <button className="btn btn--ghost" onClick={onClose} type="button">
            Cancel
          </button>
          <button className="btn btn--primary" disabled={saving} onClick={() => void save()} type="button">
            {saving ? 'Saving…' : 'Save Playlist'}
          </button>
        </div>
      </div>
    </ModalFrame>
  )
}
