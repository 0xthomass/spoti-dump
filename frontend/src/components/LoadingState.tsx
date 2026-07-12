export function LoadingState({
  label,
  compact,
}: {
  label: string
  compact?: boolean
}) {
  return (
    <div className={`state-card${compact ? ' state-card--compact' : ''}`}>
      <div className="spinner" />
      <span>{label}</span>
    </div>
  )
}
