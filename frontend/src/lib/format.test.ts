import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  coverMonogram,
  cooldownRemainingMs,
  coverageLabel,
  formatBytes,
  formatDateTime,
  formatDuration,
  formatDurationDelta,
  formatNumber,
  formatOptionalScorePercent,
  formatScorePercent,
  identityConflictProviderLabel,
  identityConflictRecommendationLabel,
  identityGapProviderLabel,
  operationDisplayTitle,
  operationTitle,
  recommendationClassName,
  statusTone,
} from './format'

describe('formatNumber', () => {
  it('formats small values without grouping', () => {
    expect(formatNumber(0)).toBe('0')
    expect(formatNumber(42)).toBe('42')
  })

  it('groups large values using the runtime locale', () => {
    // Compare against the same Intl formatter the implementation uses so the
    // assertion stays stable across locales/CI runners.
    const expected = new Intl.NumberFormat().format(1234567)
    expect(formatNumber(1234567)).toBe(expected)
  })

  it('handles negative values', () => {
    expect(formatNumber(-5)).toBe(new Intl.NumberFormat().format(-5))
  })
})

describe('formatScorePercent / formatOptionalScorePercent', () => {
  it('rounds a 0..1 score to a whole percent', () => {
    expect(formatScorePercent(0)).toBe('0%')
    expect(formatScorePercent(0.5)).toBe('50%')
    expect(formatScorePercent(1)).toBe('100%')
    expect(formatScorePercent(0.756)).toBe('76%')
  })

  it('returns Unknown for a null optional score', () => {
    expect(formatOptionalScorePercent(null)).toBe('Unknown')
    expect(formatOptionalScorePercent(0.25)).toBe('25%')
  })
})

describe('formatDurationDelta', () => {
  it('handles null, singular, and plural seconds', () => {
    expect(formatDurationDelta(null)).toBe('Unknown')
    expect(formatDurationDelta(1)).toBe('1 sec')
    expect(formatDurationDelta(5)).toBe('5 sec')
    expect(formatDurationDelta(1234)).toBe(`${formatNumber(1234)} sec`)
  })
})

describe('formatDuration', () => {
  it('returns "now" for zero or negative input', () => {
    expect(formatDuration(0)).toBe('now')
    expect(formatDuration(-5000)).toBe('now')
  })

  it('rounds up sub-second remainders to whole seconds', () => {
    expect(formatDuration(1)).toBe('1s')
    expect(formatDuration(1000)).toBe('1s')
    expect(formatDuration(2500)).toBe('3s')
  })

  it('shows seconds only under five minutes', () => {
    expect(formatDuration(3000)).toBe('3s')
    expect(formatDuration(90000)).toBe('1m 30s')
  })

  it('drops seconds at or beyond five minutes', () => {
    expect(formatDuration(300000)).toBe('5m')
    expect(formatDuration(360000)).toBe('6m')
  })

  it('renders hours with a zero-minute component', () => {
    expect(formatDuration(3600000)).toBe('1h 0m')
    expect(formatDuration(3900000)).toBe('1h 5m')
  })
})

describe('formatBytes', () => {
  it('keeps raw byte counts below 1 KiB', () => {
    expect(formatBytes(0)).toBe('0 B')
    expect(formatBytes(512)).toBe('512 B')
    expect(formatBytes(1023)).toBe('1023 B')
  })

  it('uses one decimal below ten and none at or above ten', () => {
    expect(formatBytes(1024)).toBe('1.0 KB')
    expect(formatBytes(1536)).toBe('1.5 KB')
    expect(formatBytes(10 * 1024)).toBe('10 KB')
  })

  it('scales through MB and GB', () => {
    expect(formatBytes(1024 * 1024)).toBe('1.0 MB')
    expect(formatBytes(5 * 1024 * 1024 * 1024)).toBe('5.0 GB')
  })

  it('caps at the largest unit (TB)', () => {
    expect(formatBytes(1024 ** 5)).toBe('1024 TB')
  })
})

describe('formatDateTime', () => {
  it('delegates to a medium-date short-time Intl formatter', () => {
    const value = '2026-06-15T12:00:00Z'
    const expected = new Intl.DateTimeFormat(undefined, {
      dateStyle: 'medium',
      timeStyle: 'short',
    }).format(new Date(value))
    const actual = formatDateTime(value)
    expect(actual).toBe(expected)
    // A midday-UTC date lands on 2026 in every timezone.
    expect(actual).toContain('2026')
  })

  it('throws on an unparseable date (documents current behavior)', () => {
    // The helper has no invalid-date guard; Intl throws on Invalid Date.
    expect(() => formatDateTime('not-a-date')).toThrow()
  })
})

describe('cooldownRemainingMs', () => {
  afterEach(() => {
    vi.useRealTimers()
  })

  it('returns 0 for a null cooldown', () => {
    expect(cooldownRemainingMs(null)).toBe(0)
  })

  it('returns the remaining milliseconds for a future cooldown', () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-01-01T00:00:00Z'))
    const future = new Date('2026-01-01T00:00:30Z').toISOString()
    expect(cooldownRemainingMs(future)).toBe(30_000)
  })

  it('clamps a past cooldown to 0', () => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-01-01T00:00:00Z'))
    const past = new Date('2025-12-31T23:59:00Z').toISOString()
    expect(cooldownRemainingMs(past)).toBe(0)
  })

  it('returns 0 for an unparseable cooldown timestamp', () => {
    expect(cooldownRemainingMs('not-a-date')).toBe(0)
  })
})

describe('coverMonogram', () => {
  it('takes the initials of the first two words', () => {
    expect(coverMonogram('Hello World')).toBe('HW')
    expect(coverMonogram('4 Non Blondes')).toBe('4N')
  })

  it('handles a single word', () => {
    expect(coverMonogram('hello')).toBe('H')
  })

  it('does not split on punctuation into extra initials beyond two', () => {
    // "Tyler, The Creator" tokenizes to three words; only the first two count.
    expect(coverMonogram('Tyler, The Creator')).toBe('TT')
  })

  it('falls back to TR for empty or punctuation-only titles', () => {
    expect(coverMonogram('')).toBe('TR')
    expect(coverMonogram('!!!')).toBe('TR')
  })
})

describe('statusTone', () => {
  it('maps known status keys to tones', () => {
    expect(statusTone('error')).toBe('danger')
    expect(statusTone('unmatched')).toBe('warning')
    expect(statusTone('missing')).toBe('warning')
    expect(statusTone('synced')).toBe('good')
    expect(statusTone('provider')).toBe('provider')
  })

  it('falls back to local for unknown keys', () => {
    expect(statusTone('local')).toBe('local')
    expect(statusTone('whatever')).toBe('local')
  })
})

describe('coverageLabel', () => {
  it('maps each known coverage key', () => {
    expect(coverageLabel('missing-any-provider')).toBe('Missing any ID')
    expect(coverageLabel('missing-spotify')).toBe('Missing Spotify ID')
    expect(coverageLabel('missing-youtube-music')).toBe('Missing YouTube ID')
    expect(coverageLabel('spotify-only')).toBe('Spotify only')
    expect(coverageLabel('youtube-music-only')).toBe('YouTube only')
    expect(coverageLabel('multi-provider')).toBe('Multi-provider')
    expect(coverageLabel('canonical-only')).toBe('Canonical only')
    expect(coverageLabel('identity-conflicts')).toBe('Identity conflicts')
    expect(coverageLabel('unmatched')).toBe('Unmatched')
  })

  it('falls back to "All coverage" for unknown keys', () => {
    expect(coverageLabel('')).toBe('All coverage')
    expect(coverageLabel('nonsense')).toBe('All coverage')
  })
})

describe('provider and recommendation label helpers', () => {
  it('identityGapProviderLabel', () => {
    expect(identityGapProviderLabel('spotify')).toBe('Spotify')
    expect(identityGapProviderLabel('youtube-music')).toBe('YouTube Music')
    expect(identityGapProviderLabel('all')).toBe('All providers')
  })

  it('identityConflictProviderLabel', () => {
    expect(identityConflictProviderLabel('spotify')).toBe('Spotify candidates')
    expect(identityConflictProviderLabel('youtube-music')).toBe('YouTube candidates')
    expect(identityConflictProviderLabel('all')).toBe('All providers')
  })

  it('identityConflictRecommendationLabel', () => {
    expect(identityConflictRecommendationLabel('likely_same_recording')).toBe('Likely same')
    expect(identityConflictRecommendationLabel('needs_manual_review')).toBe('Manual review')
    expect(identityConflictRecommendationLabel('likely_different_recording')).toBe(
      'Likely different',
    )
    expect(identityConflictRecommendationLabel('other')).toBe('All recommendations')
  })

  it('recommendationClassName', () => {
    expect(recommendationClassName('likely_same_recording')).toBe('status-chip--good')
    expect(recommendationClassName('likely_different_recording')).toBe('status-chip--danger')
    expect(recommendationClassName('needs_manual_review')).toBe('status-chip--warning')
  })
})

describe('operation titles', () => {
  it('operationTitle covers every kind', () => {
    expect(operationTitle('verify')).toBe('Checking connection')
    expect(operationTitle('pull')).toBe('Pulling')
    expect(operationTitle('push')).toBe('Pushing')
    expect(operationTitle('reset_push')).toBe('Resetting & pushing')
    expect(operationTitle('identity')).toBe('Resolving identities')
    expect(operationTitle('identity_all')).toBe('Resolving library identities')
  })

  it('operationDisplayTitle appends the provider name except for identity_all', () => {
    expect(
      operationDisplayTitle({ kind: 'pull', provider_name: 'Spotify' }),
    ).toBe('Pulling Spotify')
    expect(
      operationDisplayTitle({ kind: 'identity_all', provider_name: 'Spotify' }),
    ).toBe('Resolving library identities')
  })
})
