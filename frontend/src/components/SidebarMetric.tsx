import { formatNumber } from '../lib/format'

export function SidebarMetric({ label, value }: { label: string; value: number }) {
  return (
    <div className="sidebar-metric">
      <strong>{formatNumber(value)}</strong>
      <span>{label}</span>
    </div>
  )
}
