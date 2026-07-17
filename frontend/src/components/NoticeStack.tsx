import { useEffect } from 'react'
import type { Notice } from '../context/runtime'
import { AlertIcon, CheckIcon, CloseIcon, InfoIcon } from './icons'

const TONE_ICON = {
  success: CheckIcon,
  error: AlertIcon,
  info: InfoIcon,
}

const AUTO_DISMISS_MS = 6000

function NoticeItem({
  notice,
  onDismiss,
}: {
  notice: Notice
  onDismiss: (id: number) => void
}) {
  const Icon = TONE_ICON[notice.tone]
  // Errors and multi-line reports stay until the user dismisses them; single
  // line success/info notices auto-clear.
  const persistent = notice.tone === 'error' || notice.message.includes('\n')

  useEffect(() => {
    if (persistent) {
      return
    }
    const timeout = window.setTimeout(
      () => onDismiss(notice.id),
      AUTO_DISMISS_MS,
    )
    return () => window.clearTimeout(timeout)
  }, [notice.id, persistent, onDismiss])

  return (
    <div className={`notice notice--${notice.tone}`}>
      <span className="notice__icon" aria-hidden="true">
        <Icon />
      </span>
      <div className="notice__body">{notice.message}</div>
      <button
        aria-label="Dismiss notification"
        className="notice__dismiss"
        onClick={() => onDismiss(notice.id)}
        type="button"
      >
        <CloseIcon />
      </button>
    </div>
  )
}

export function NoticeStack({
  notices,
  onDismiss,
}: {
  notices: Notice[]
  onDismiss: (id: number) => void
}) {
  return (
    <div className="notice-stack" role="status" aria-live="polite">
      {notices.map((notice) => (
        <NoticeItem key={notice.id} notice={notice} onDismiss={onDismiss} />
      ))}
    </div>
  )
}
