import { formatNumber } from '../lib/format'

export function StatTile({ label, value }: { label: string; value: number }) {
  return (
    <div className="stat-tile">
      <strong>{formatNumber(value)}</strong>
      <span>{label}</span>
    </div>
  )
}
