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
  const [clientIdError, setClientIdError] = useState<string | null>(null)
  const [clientSecretError, setClientSecretError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  function validate() {
    let ok = true
    if (clientId.trim().length < 20) {
      setClientIdError('Enter the full client ID (about 32 characters).')
      ok = false
    } else {
      setClientIdError(null)
    }
    if (clientSecret.trim().length < 20) {
      setClientSecretError('Enter the full client secret (about 32 characters).')
      ok = false
    } else {
      setClientSecretError(null)
    }
    return ok
  }

  async function connect() {
    if (!validate()) {
      return
    }
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
          <p>Create a Spotify app, then paste its credentials here:</p>
        </div>
        <ol className="setup-steps">
          <li>
            Open the{' '}
            <a
              href="https://developer.spotify.com/dashboard"
              rel="noreferrer"
              target="_blank"
            >
              Spotify developer dashboard
            </a>{' '}
            and create an app.
          </li>
          <li>Add the redirect URI below to the app&apos;s settings.</li>
          <li>Copy the app&apos;s Client ID and Client Secret.</li>
          <li>Paste them here and continue to Spotify login.</li>
        </ol>
        <label className="field">
          <span>Redirect URI</span>
          <input readOnly value={redirectUri} />
        </label>
        <div className="field-grid">
          <label className="field">
            <span>Client ID</span>
            <input
              aria-describedby={clientIdError ? 'spotify-client-id-error' : undefined}
              aria-invalid={clientIdError ? true : undefined}
              autoComplete="off"
              onChange={(event) => setClientId(event.target.value)}
              value={clientId}
            />
            {clientIdError ? (
              <span className="field-error" id="spotify-client-id-error" role="alert">
                {clientIdError}
              </span>
            ) : null}
          </label>
          <label className="field">
            <span>Client Secret</span>
            <input
              aria-describedby={
                clientSecretError ? 'spotify-client-secret-error' : undefined
              }
              aria-invalid={clientSecretError ? true : undefined}
              autoComplete="off"
              onChange={(event) => setClientSecret(event.target.value)}
              type="password"
              value={clientSecret}
            />
            {clientSecretError ? (
              <span
                className="field-error"
                id="spotify-client-secret-error"
                role="alert"
              >
                {clientSecretError}
              </span>
            ) : null}
          </label>
        </div>
        <div className="modal-actions">
          <button className="btn btn--ghost" onClick={onClose} type="button">
            Cancel
          </button>
          <button
            className="btn btn--primary"
            disabled={submitting}
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
