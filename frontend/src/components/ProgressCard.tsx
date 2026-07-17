import { formatNumber } from '../lib/format'

export function ProgressCard({
  label,
  done,
  total,
  running,
}: {
  label: string
  done: number
  total: number | null
  running: boolean
}) {
  const hasTotal = total !== null && total > 0
  const percent = hasTotal
    ? Math.min(100, Math.round((done / (total as number)) * 100))
    : null
  // Indeterminate only while we genuinely do not know the total AND work is
  // still ongoing — a finished op with an unknown total shows a filled bar
  // rather than an animation that never stops.
  const indeterminate = total === null && running

  return (
    <div className="stat-tile progress-metric">
      <strong>
        {total === null
          ? formatNumber(done)
          : `${formatNumber(done)} / ${formatNumber(total)}`}
      </strong>
      <span>{label}</span>
      <div className="progress-metric__track-row">
        <div
          aria-label={label}
          aria-valuemax={100}
          aria-valuemin={0}
          aria-valuenow={percent ?? undefined}
          className={`progress-bar${
            indeterminate ? ' progress-bar--indeterminate' : ''
          }`}
          role="progressbar"
        >
          <div
            className="progress-bar__fill"
            style={indeterminate ? undefined : { width: `${percent ?? 100}%` }}
          />
        </div>
        {percent !== null ? (
          <span className="progress-metric__percent">{percent}%</span>
        ) : null}
      </div>
    </div>
  )
}
