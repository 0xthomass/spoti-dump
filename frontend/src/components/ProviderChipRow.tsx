import type { ProviderBadge } from '../api/types'

export function ProviderChipRow({ providers }: { providers: ProviderBadge[] }) {
  if (providers.length === 0) {
    return <span className="status-chip status-chip--local">Local</span>
  }
  return (
    <div className="chip-row">
      {providers.map((provider) => (
        <span
          className="status-chip status-chip--provider"
          key={`${provider.key}-${provider.provider_id}`}
          title={`${provider.label} · ${provider.source} · ${provider.provider_id}`}
        >
          {provider.label}
        </span>
      ))}
    </div>
  )
}
