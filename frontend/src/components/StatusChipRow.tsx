import type { StatusPill } from '../api/types'
import { statusTone } from '../lib/format'

export function StatusChipRow({ pills }: { pills: StatusPill[] }) {
  return (
    <div className="chip-row">
      {pills.map((pill, index) => {
        const tip = pill.title
        return (
          <span
            aria-label={tip ? `${pill.label}: ${tip}` : undefined}
            className={`status-chip status-chip--${statusTone(pill.key)}${
              tip ? ' has-tip' : ''
            }`}
            data-tip={tip || undefined}
            key={`${pill.key}-${index}`}
            tabIndex={tip ? 0 : undefined}
          >
            {pill.label}
          </span>
        )
      })}
    </div>
  )
}
