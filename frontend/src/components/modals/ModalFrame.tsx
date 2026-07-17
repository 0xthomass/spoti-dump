import { useEffect, useId, useRef } from 'react'
import type { ReactNode } from 'react'
import { IconButton } from '../IconButton'
import { CloseIcon } from '../icons'

const FOCUSABLE_SELECTOR = [
  'a[href]',
  'button:not([disabled])',
  'textarea:not([disabled])',
  'input:not([disabled])',
  'select:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
].join(',')

// Module-level stack of open modals. Only the topmost modal reacts to Escape,
// so a confirm layered over an editor closes just the confirm.
const modalStack: symbol[] = []

export function ModalFrame({
  title,
  children,
  onClose,
}: {
  title: string
  children: ReactNode
  onClose: () => void
}) {
  const dialogRef = useRef<HTMLDivElement>(null)
  const titleId = useId()
  // Read onClose through a ref so the setup effect can stay mount-only: it must
  // not re-run (and steal focus / re-register) when a parent re-creates the
  // onClose callback mid-edit. The ref is synced in an effect (never during
  // render) so the keydown handler always sees the latest callback.
  const onCloseRef = useRef(onClose)
  useEffect(() => {
    onCloseRef.current = onClose
  })

  useEffect(() => {
    const dialog = dialogRef.current
    const opener = document.activeElement as HTMLElement | null
    const token = Symbol('modal')
    modalStack.push(token)

    const focusable = () =>
      dialog
        ? Array.from(
            dialog.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR),
          ).filter((element) => element.tabIndex !== -1)
        : []

    // Move focus into the dialog on open.
    const initial = focusable()[0]
    ;(initial ?? dialog)?.focus()

    function handleKeyDown(event: KeyboardEvent) {
      // Only the topmost modal handles keys.
      if (modalStack[modalStack.length - 1] !== token) {
        return
      }
      if (event.key === 'Escape') {
        event.stopPropagation()
        onCloseRef.current()
        return
      }
      if (event.key !== 'Tab' || !dialog) {
        return
      }
      const items = focusable()
      if (items.length === 0) {
        event.preventDefault()
        dialog.focus()
        return
      }
      const first = items[0]
      const last = items[items.length - 1]
      const active = document.activeElement
      if (event.shiftKey) {
        if (active === first || !dialog.contains(active)) {
          event.preventDefault()
          last.focus()
        }
      } else if (active === last || !dialog.contains(active)) {
        event.preventDefault()
        first.focus()
      }
    }

    document.addEventListener('keydown', handleKeyDown)

    return () => {
      document.removeEventListener('keydown', handleKeyDown)
      const index = modalStack.indexOf(token)
      if (index >= 0) {
        modalStack.splice(index, 1)
      }
      // Restore focus to whatever opened the modal.
      opener?.focus?.()
    }
  }, [])

  return (
    <div className="modal-backdrop" onClick={onClose} role="presentation">
      <div
        aria-labelledby={titleId}
        aria-modal="true"
        className="modal"
        onClick={(event) => event.stopPropagation()}
        ref={dialogRef}
        role="dialog"
        tabIndex={-1}
      >
        <header className="modal-head">
          <div>
            <span className="eyebrow">Editor</span>
            <h2 id={titleId}>{title}</h2>
          </div>
          <IconButton label="Close modal" onClick={onClose}>
            <CloseIcon />
          </IconButton>
        </header>
        {children}
      </div>
    </div>
  )
}
