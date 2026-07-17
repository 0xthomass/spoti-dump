import type {
  OperationKind,
  OperationSnapshot,
  ProviderConnectionState,
} from '../api/types'

export function parsePage(raw: string | null) {
  if (!raw) {
    return 1
  }
  const value = Number(raw)
  return Number.isFinite(value) && value > 0 ? value : 1
}

export function identityConflictQueryString({
  page,
  query,
  provider,
  recommendation,
  impact,
}: {
  page?: number
  query?: string
  provider?: string
  recommendation?: string
  impact?: string
}) {
  const params = new URLSearchParams()
  if (page) {
    params.set('page', String(page))
  }
  if (query) {
    params.set('q', query)
  }
  if (provider) {
    params.set('provider', provider)
  }
  if (recommendation) {
    params.set('recommendation', recommendation)
  }
  if (impact) {
    params.set('impact', impact)
  }
  return params.toString()
}

export function formatNumber(value: number) {
  return new Intl.NumberFormat().format(value)
}

export function formatScorePercent(value: number) {
  return `${Math.round(value * 100)}%`
}

export function formatOptionalScorePercent(value: number | null) {
  if (value === null) {
    return 'Unknown'
  }
  return formatScorePercent(value)
}

export function formatDurationDelta(value: number | null) {
  if (value === null) {
    return 'Unknown'
  }
  if (value === 1) {
    return '1 sec'
  }
  return `${formatNumber(value)} sec`
}

export function recommendationClassName(key: string) {
  if (key === 'likely_same_recording') {
    return 'status-chip--good'
  }
  if (key === 'likely_different_recording') {
    return 'status-chip--danger'
  }
  return 'status-chip--warning'
}

export function formatDateTime(value: string) {
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(new Date(value))
}

export function cooldownRemainingMs(cooldownUntil: string | null) {
  if (!cooldownUntil) {
    return 0
  }
  const until = new Date(cooldownUntil).getTime()
  if (!Number.isFinite(until)) {
    return 0
  }
  return Math.max(0, until - Date.now())
}

export function formatDuration(valueMs: number) {
  const totalSeconds = Math.ceil(Math.max(0, valueMs) / 1000)
  if (totalSeconds <= 0) {
    return 'now'
  }

  const hours = Math.floor(totalSeconds / 3600)
  const minutes = Math.floor((totalSeconds % 3600) / 60)
  const seconds = totalSeconds % 60
  const parts = []

  if (hours > 0) {
    parts.push(`${hours}h`)
  }
  if (minutes > 0 || hours > 0) {
    parts.push(`${minutes}m`)
  }
  if (hours === 0 && minutes < 5) {
    parts.push(`${seconds}s`)
  }

  return parts.join(' ')
}

export function providerCooldownCopy(provider: ProviderConnectionState) {
  const remainingMs = cooldownRemainingMs(provider.cooldown_until)
  if (!provider.cooldown_until || remainingMs <= 0) {
    return null
  }

  const reason = provider.cooldown_reason
    ? ` Provider response: ${provider.cooldown_reason}`
    : ''
  return `${provider.name} asked us to wait ${formatDuration(remainingMs)}. The app will avoid ${provider.name} API calls until ${formatDateTime(provider.cooldown_until)}.${reason}`
}

export function formatBytes(value: number) {
  if (value < 1024) {
    return `${value} B`
  }
  const units = ['KB', 'MB', 'GB', 'TB']
  let current = value / 1024
  let unitIndex = 0
  while (current >= 1024 && unitIndex < units.length - 1) {
    current /= 1024
    unitIndex += 1
  }
  return `${current.toFixed(current >= 10 ? 0 : 1)} ${units[unitIndex]}`
}

export function coverMonogram(title: string) {
  const parts = title
    .split(/[^a-zA-Z0-9]+/)
    .filter(Boolean)
    .slice(0, 2)
  return parts.map((part) => part[0].toUpperCase()).join('') || 'TR'
}

export function coverPalette(seed: string) {
  const palettes: [string, string][] = [
    ['#46c2ff', '#18202c'],
    ['#ff5f6d', '#240913'],
    ['#5be7c4', '#102620'],
    ['#ffb347', '#2e170a'],
    ['#c77dff', '#180828'],
    ['#73fbd3', '#101f1e'],
    ['#ffd166', '#33210c'],
    ['#ff6b6b', '#250f12'],
  ]
  let hash = 0
  for (const character of seed) {
    hash = (hash + character.charCodeAt(0)) % palettes.length
  }
  return palettes[hash]
}

export function statusTone(key: string) {
  if (key === 'error') {
    return 'danger'
  }
  if (key === 'unmatched' || key === 'missing') {
    return 'warning'
  }
  if (key === 'synced') {
    return 'good'
  }
  if (key === 'provider') {
    return 'provider'
  }
  return 'local'
}

export function coverageLabel(value: string) {
  if (value === 'missing-any-provider') {
    return 'Missing any ID'
  }
  if (value === 'missing-spotify') {
    return 'Missing Spotify ID'
  }
  if (value === 'missing-youtube-music') {
    return 'Missing YouTube ID'
  }
  if (value === 'spotify-only') {
    return 'Spotify only'
  }
  if (value === 'youtube-music-only') {
    return 'YouTube only'
  }
  if (value === 'multi-provider') {
    return 'Multi-provider'
  }
  if (value === 'canonical-only') {
    return 'Canonical only'
  }
  if (value === 'identity-conflicts') {
    return 'Identity conflicts'
  }
  if (value === 'unmatched') {
    return 'Unmatched'
  }
  return 'All coverage'
}

export function identityGapProviderLabel(value: string) {
  if (value === 'spotify') {
    return 'Spotify'
  }
  if (value === 'youtube-music') {
    return 'YouTube Music'
  }
  return 'All providers'
}

export function identityConflictProviderLabel(value: string) {
  if (value === 'spotify') {
    return 'Spotify candidates'
  }
  if (value === 'youtube-music') {
    return 'YouTube candidates'
  }
  return 'All providers'
}

export function identityConflictRecommendationLabel(value: string) {
  if (value === 'likely_same_recording') {
    return 'Likely same'
  }
  if (value === 'needs_manual_review') {
    return 'Manual review'
  }
  if (value === 'likely_different_recording') {
    return 'Likely different'
  }
  return 'All recommendations'
}

export function operationTitle(kind: OperationKind) {
  switch (kind) {
    case 'verify':
      return 'Checking connection'
    case 'pull':
      return 'Pulling'
    case 'push':
      return 'Pushing'
    case 'reset_push':
      return 'Resetting & pushing'
    case 'identity':
      return 'Resolving identities'
    case 'identity_all':
      return 'Resolving library identities'
  }
}

export function operationDisplayTitle(
  operation: Pick<OperationSnapshot, 'kind' | 'provider_name'>,
) {
  return operation.kind === 'identity_all'
    ? operationTitle(operation.kind)
    : `${operationTitle(operation.kind)} ${operation.provider_name}`
}
