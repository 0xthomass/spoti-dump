import { useState } from 'react'
import { apiRequest } from '../../api/client'
import { useRuntime } from '../../context/runtime'
import { ModalFrame } from './ModalFrame'

export function SpotifyConnectModal({
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
      notify(
        error instanceof Error ? error.message : 'Spotify connection failed.',
        { tone: 'error' },
      )
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
          <button className="btn btn--ghost" onClick={onClose} type="button">
            Cancel
          </button>
          <button
            className="btn btn--primary"
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
