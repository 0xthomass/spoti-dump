import { useState } from 'react'
import type { ActionResponse } from '../../api/types'
import { actionMessage, apiRequest } from '../../api/client'
import { useRuntime } from '../../context/runtime'
import { ModalFrame } from './ModalFrame'

const YOUTUBE_HEADERS_SAMPLE = `{
  "cookie": "SAPISID=your_cookie_here; __Secure-3PAPISID=your_cookie_here; SID=your_cookie_here",
  "x-goog-authuser": "paste_from_request",
  "origin": "https://music.youtube.com"
}`

export function YoutubeMusicConnectModal({ onClose }: { onClose: () => void }) {
  const { notify, refresh } = useRuntime()
  const [headersJson, setHeadersJson] = useState(YOUTUBE_HEADERS_SAMPLE)
  const [submitting, setSubmitting] = useState(false)

  async function connect() {
    setSubmitting(true)
    try {
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
      )
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <ModalFrame title="Link YouTube Music" onClose={onClose}>
      <div className="modal-stack">
        <div className="confirm-copy">
          <p>
            Paste the signed-in browser headers JSON from YouTube Music.
          </p>
        </div>
        <label className="field">
          <span>Headers JSON</span>
          <textarea
            onChange={(event) => setHeadersJson(event.target.value)}
            placeholder={YOUTUBE_HEADERS_SAMPLE}
            rows={10}
            value={headersJson}
          />
        </label>
        <div className="modal-actions">
          <button className="btn btn--ghost" onClick={onClose} type="button">
            Cancel
          </button>
          <button
            className="btn btn--primary"
            disabled={!headersJson.trim() || submitting}
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
