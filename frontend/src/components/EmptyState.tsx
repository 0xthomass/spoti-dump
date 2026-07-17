import type { ReactNode } from 'react'

export function EmptyState({
  title,
  copy,
  compact,
  action,
}: {
  title: string
  copy: string
  compact?: boolean
  action?: ReactNode
}) {
  return (
    <div className={`state-card${compact ? ' state-card--compact' : ''}`}>
      <strong>{title}</strong>
      <span>{copy}</span>
      {action ? <div className="state-card-actions">{action}</div> : null}
    </div>
  )
}
