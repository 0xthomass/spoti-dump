import type { ProviderBadge } from '../api/types'

export function ProviderChipRow({ providers }: { providers: ProviderBadge[] }) {
  if (providers.length === 0) {
    return <span className="status-chip status-chip--local">Local</span>
  }
  return (
    <div className="chip-row">
      {providers.map((provider) => {
        const tip = `${provider.label} · ${provider.source} · ${provider.provider_id}`
        return (
          <span
            aria-label={tip}
            className="status-chip status-chip--provider has-tip"
            data-tip={tip}
            key={`${provider.key}-${provider.provider_id}`}
            tabIndex={0}
          >
            {provider.label}
          </span>
        )
      })}
    </div>
  )
}
