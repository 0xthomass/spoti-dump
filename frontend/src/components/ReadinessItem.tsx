import { formatNumber } from '../lib/format'

export function ReadinessItem({
  label,
  ready,
  blocked,
}: {
  label: string
  ready: number
  blocked: number
}) {
  const total = ready + blocked
  const percent = total === 0 ? 100 : Math.round((ready / total) * 100)
  return (
    <div className="readiness-item">
      <span>{label}</span>
      <strong>
        {formatNumber(ready)} / {formatNumber(total)}
      </strong>
      <small>{blocked > 0 ? `${formatNumber(blocked)} missing IDs` : `${percent}% ready`}</small>
    </div>
  )
}
