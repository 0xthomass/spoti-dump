import { formatNumber } from '../lib/format'

export function ProgressCard({
  label,
  done,
  total,
}: {
  label: string
  done: number
  total: number | null
}) {
  return (
    <div className="stat-tile">
      <strong>{total === null ? formatNumber(done) : `${formatNumber(done)} / ${formatNumber(total)}`}</strong>
      <span>{label}</span>
    </div>
  )
}
