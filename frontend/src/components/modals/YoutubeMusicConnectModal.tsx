import { useState } from 'react'
import type { ActionResponse } from '../../api/types'
import { actionMessage, apiRequest } from '../../api/client'
import { useRuntime } from '../../context/runtime'
import { ModalFrame } from './ModalFrame'

export function YoutubeMusicConnectModal({ onClose }: { onClose: () => void }) {
  const { notify, refresh } = useRuntime()
  const [cookie, setCookie] = useState('')
  const [authuser, setAuthuser] = useState('')
  const [cookieError, setCookieError] = useState<string | null>(null)
  const [authuserError, setAuthuserError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  function validate() {
    let ok = true
    if (!cookie.trim()) {
      setCookieError('Paste the cookie header value.')
      ok = false
    } else if (!/SAPISID/.test(cookie)) {
      setCookieError(
        'That cookie is missing SAPISID — copy the full cookie header value.',
      )
      ok = false
    } else {
      setCookieError(null)
    }
    if (authuser.trim() && !/^\d+$/.test(authuser.trim())) {
      setAuthuserError('x-goog-authuser must be a number (for example 0).')
      ok = false
    } else {
      setAuthuserError(null)
    }
    return ok
  }

  async function connect() {
    if (!validate()) {
      return
    }
    setSubmitting(true)
    try {
      const headersJson = JSON.stringify({
        cookie: cookie.trim(),
        ...(authuser.trim() ? { 'x-goog-authuser': authuser.trim() } : {}),
        origin: 'https://music.youtube.com',
      })
      const payload = await apiRequest<ActionResponse>(
        '/providers/youtube-music/connect',
        {
          method: 'POST',
          body: JSON.stringify({ headers_json: headersJson }),
        },
      )
      notify(actionMessage(payload))
      refresh()
      onClose()
    } catch (error) {
      notify(
        error instanceof Error ? error.message : 'YouTube Music connection failed.',
        { tone: 'error' },
      )
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <ModalFrame title="Link YouTube Music" onClose={onClose}>
      <div className="modal-stack">
        <div className="confirm-copy">
          <p>Grab the signed-in request headers from your browser:</p>
        </div>
        <ol className="setup-steps">
          <li>
            Open <code>music.youtube.com</code> while logged in.
          </li>
          <li>Open your browser DevTools and switch to the Network tab.</li>
          <li>
            Click any request to <code>music.youtube.com</code>.
          </li>
          <li>
            Copy the <code>cookie</code> request header value (plus{' '}
            <code>x-goog-authuser</code> if present) and paste it below.
          </li>
        </ol>
        <label className="field">
          <span>Cookie header value</span>
          <textarea
            aria-describedby={cookieError ? 'ytm-cookie-error' : undefined}
            aria-invalid={cookieError ? true : undefined}
            onChange={(event) => setCookie(event.target.value)}
            placeholder="VISITOR_INFO1_LIVE=…; SAPISID=…; __Secure-3PAPISID=…; SID=…"
            rows={6}
            value={cookie}
          />
          {cookieError ? (
            <span className="field-error" id="ytm-cookie-error" role="alert">
              {cookieError}
            </span>
          ) : null}
        </label>
        <label className="field">
          <span>x-goog-authuser (optional)</span>
          <input
            aria-describedby={authuserError ? 'ytm-authuser-error' : undefined}
            aria-invalid={authuserError ? true : undefined}
            inputMode="numeric"
            onChange={(event) => setAuthuser(event.target.value)}
            placeholder="0"
            value={authuser}
          />
          {authuserError ? (
            <span className="field-error" id="ytm-authuser-error" role="alert">
              {authuserError}
            </span>
          ) : null}
        </label>
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
            {submitting ? 'Linking…' : 'Save Link'}
          </button>
        </div>
      </div>
    </ModalFrame>
  )
}
