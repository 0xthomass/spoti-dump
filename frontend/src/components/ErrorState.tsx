export function ErrorState({
  message,
  compact,
  onRetry,
}: {
  message: string
  compact?: boolean
  onRetry?: () => void
}) {
  return (
    <div className={`state-card state-card--error${compact ? ' state-card--compact' : ''}`}>
      <strong>Something failed</strong>
      <span>{message}</span>
      {onRetry ? (
        <div className="state-card-actions">
          <button className="btn btn--secondary" onClick={onRetry} type="button">
            Retry
          </button>
        </div>
      ) : null}
    </div>
  )
}
