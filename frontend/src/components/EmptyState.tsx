export function EmptyState({
  title,
  copy,
  compact,
}: {
  title: string
  copy: string
  compact?: boolean
}) {
  return (
    <div className={`state-card${compact ? ' state-card--compact' : ''}`}>
      <strong>{title}</strong>
      <span>{copy}</span>
    </div>
  )
}
