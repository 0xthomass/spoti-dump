import type {
  ActionResponse,
  BulkMergeIdentityConflictsResponse,
} from './types'
import { formatNumber } from '../lib/format'

export function apiPath(path: string) {
  return `/api${path}`
}

export async function apiRequest<T>(path: string, init?: RequestInit): Promise<T> {
  const headers = new Headers(init?.headers)
  if (init?.body && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json')
  }

  const response = await fetch(apiPath(path), {
    ...init,
    headers,
  })

  const payload = await response.json().catch(() => null)
  if (!response.ok) {
    const message =
      payload && typeof payload.error === 'string'
        ? payload.error
        : `Request failed (${response.status})`
    throw new Error(message)
  }

  // A 2xx response whose body was not parseable JSON must not be handed back
  // as a null "T" — callers rely on a typed payload. Fail loud instead.
  if (payload === null) {
    throw new Error(`Response was not valid JSON (${response.status})`)
  }

  return payload as T
}

export function actionMessage(payload: ActionResponse) {
  if (!payload.warnings.length) {
    return payload.message
  }
  return `${payload.message}\n${payload.warnings.map((warning) => `• ${warning}`).join('\n')}`
}

export function bulkMergeMessage(payload: BulkMergeIdentityConflictsResponse) {
  const lines = [
    payload.message,
    `Manual backup: ${payload.pre_merge_backup_path}`,
    `Resolved provider ID conflicts: ${formatNumber(payload.resolved_provider_conflicts)}`,
  ]
  if (payload.skipped_count > 0) {
    lines.push(`Skipped stale or capped rows: ${formatNumber(payload.skipped_count)}`)
  }
  lines.push(...payload.warnings.map((warning) => `• ${warning}`))
  return lines.join('\n')
}
