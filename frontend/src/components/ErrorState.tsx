export function ErrorState({
  message,
  compact,
}: {
  message: string
  compact?: boolean
}) {
  return (
    <div className={`state-card state-card--error${compact ? ' state-card--compact' : ''}`}>
      <strong>Something failed</strong>
      <span>{message}</span>
    </div>
  )
}
