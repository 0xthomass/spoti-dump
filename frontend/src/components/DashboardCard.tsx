import type { ReactNode } from 'react'
import { formatNumber } from '../lib/format'

export function DashboardCard({
  label,
  value,
  children,
}: {
  label: string
  value: number
  children?: ReactNode
}) {
  return (
    <div className="dashboard-card">
      <span className="eyebrow">{label}</span>
      <strong>{formatNumber(value)}</strong>
      {children ? <p>{children}</p> : null}
    </div>
  )
}
