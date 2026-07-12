import type { StatusPill } from '../api/types'
import { statusTone } from '../lib/format'

export function StatusChipRow({ pills }: { pills: StatusPill[] }) {
  return (
    <div className="chip-row">
      {pills.map((pill, index) => (
        <span
          className={`status-chip status-chip--${statusTone(pill.key)}`}
          key={`${pill.key}-${index}`}
          title={pill.title}
        >
          {pill.label}
        </span>
      ))}
    </div>
  )
}
