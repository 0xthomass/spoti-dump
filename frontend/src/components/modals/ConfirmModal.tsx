import type { ConfirmState } from '../../context/runtime'
import { ModalFrame } from './ModalFrame'

export function ConfirmModal({
  request,
  onCancel,
  onConfirm,
}: {
  request: ConfirmState
  onCancel: () => void
  onConfirm: () => void
}) {
  return (
    <ModalFrame title={request.title} onClose={onCancel}>
      <div className="modal-stack">
        <div className="confirm-copy">
          <p>{request.message}</p>
          {request.details ? (
            <p className="confirm-details">{request.details}</p>
          ) : null}
        </div>
        <div
          className={`confirm-warning confirm-warning--${request.tone}`}
        >
          <strong>
            {request.tone === 'danger' ? 'Destructive action' : 'Confirm change'}
          </strong>
          <span>
            {request.tone === 'danger'
              ? 'This will update the canonical database immediately.'
              : 'This will change the canonical source of truth immediately.'}
          </span>
        </div>
        <div className="modal-actions">
          <button className="btn btn--ghost" onClick={onCancel} type="button">
            Cancel
          </button>
          <button
            className={`btn ${request.tone === 'danger' ? 'btn--danger' : 'btn--primary'}`}
            onClick={onConfirm}
            type="button"
          >
            {request.confirmLabel}
          </button>
        </div>
      </div>
    </ModalFrame>
  )
}
