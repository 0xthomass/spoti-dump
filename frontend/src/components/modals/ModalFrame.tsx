import type { ReactNode } from 'react'
import { IconButton } from '../IconButton'
import { CloseIcon } from '../icons'

export function ModalFrame({
  title,
  children,
  onClose,
}: {
  title: string
  children: ReactNode
  onClose: () => void
}) {
  return (
    <div className="modal-backdrop" onClick={onClose} role="presentation">
      <div
        className="modal"
        onClick={(event) => event.stopPropagation()}
        role="dialog"
      >
        <header className="modal-head">
          <div>
            <span className="eyebrow">Editor</span>
            <h2>{title}</h2>
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
