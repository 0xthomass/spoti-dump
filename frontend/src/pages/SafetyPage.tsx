import { useState } from 'react'
import type {
  BackupItem,
  BackupsResponse,
  CreateBackupResponse,
  HealthResponse,
  RestoreBackupResponse,
} from '../api/types'
import { apiRequest } from '../api/client'
import { useConfirm, useRuntime } from '../context/runtime'
import { useApiQuery } from '../hooks/useApiQuery'
import { formatBytes, formatDateTime, formatNumber } from '../lib/format'
import { DashboardCard } from '../components/DashboardCard'
import { EmptyState } from '../components/EmptyState'
import { ErrorState } from '../components/ErrorState'
import { HeroStat } from '../components/HeroStat'
import { LoadingState } from '../components/LoadingState'
import { PageHero } from '../components/PageHero'

export function SafetyPage() {
  const { revision, refresh, notify } = useRuntime()
  const confirm = useConfirm()
  const backupsResource = useApiQuery<BackupsResponse>('/backups', revision)
  const healthResource = useApiQuery<HealthResponse>('/health', revision)
  const [creating, setCreating] = useState(false)
  const [restoring, setRestoring] = useState<string | null>(null)

  async function createBackup() {
    setCreating(true)
    try {
      const payload = await apiRequest<CreateBackupResponse>('/backups/manual', {
        method: 'POST',
      })
      notify(payload.message)
      refresh()
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Backup failed.')
    } finally {
      setCreating(false)
    }
  }

  async function restoreBackup(backup: BackupItem) {
    const accepted = await confirm({
      title: 'Restore this backup?',
      message: `This will replace the current canonical library database with ${backup.file_name}.`,
      details:
        'The app will first create a pre-restore manual backup of the current database. Provider accounts are not touched.',
      confirmLabel: 'Restore backup',
      tone: 'danger',
    })
    if (!accepted) {
      return
    }

    setRestoring(backup.path)
    try {
      const payload = await apiRequest<RestoreBackupResponse>('/backups/restore', {
        method: 'POST',
        body: JSON.stringify({
          backup_type: backup.backup_type,
          file_name: backup.file_name,
        }),
      })
      notify(payload.message)
      refresh()
    } catch (error) {
      notify(error instanceof Error ? error.message : 'Restore failed.')
    } finally {
      setRestoring(null)
    }
  }

  if (
    (backupsResource.loading && !backupsResource.data) ||
    (healthResource.loading && !healthResource.data)
  ) {
    return <LoadingState label="Loading safety data" />
  }

  if (
    backupsResource.error ||
    healthResource.error ||
    !backupsResource.data ||
    !healthResource.data
  ) {
    return (
      <ErrorState
        message={
          backupsResource.error ??
          healthResource.error ??
          'Failed to load safety data.'
        }
      />
    )
  }

  const backups = backupsResource.data
  const health = healthResource.data
  const manualCount = backups.backups.filter(
    (backup) => backup.backup_type === 'manual',
  ).length
  const automaticCount = backups.backups.filter(
    (backup) => backup.backup_type === 'automatic',
  ).length

  return (
    <section className="page-stack">
      <PageHero
        eyebrow="Safety"
        title="Source-of-truth protection."
        copy="Manual snapshots are never pruned by automatic backup retention."
      >
        <HeroStat label="Database" value={health.integrity_check} />
        <HeroStat label="Manual backups" value={formatNumber(manualCount)} />
      </PageHero>

      <div className="metric-grid">
        <DashboardCard label="Tracks" value={health.tracks}>
          Canonical track rows.
        </DashboardCard>
        <DashboardCard label="Saved Tracks" value={health.saved_tracks}>
          Source-of-truth saved library.
        </DashboardCard>
        <DashboardCard label="Playlists" value={health.playlists}>
          Canonical playlist records.
        </DashboardCard>
        <DashboardCard label="Automatic Backups" value={automaticCount}>
          Retained rolling snapshots.
        </DashboardCard>
      </div>

      <section className="panel">
        <div className="panel-head">
          <div>
            <span className="eyebrow">Manual Backup</span>
            <h2>Snapshot the canonical database</h2>
            <p>
              Creates a point-in-time copy of <code>{health.database_path}</code> under{' '}
              <code>{backups.manual_backup_dir}</code>.
            </p>
          </div>
          <button
            className="provider-action-button"
            disabled={creating}
            onClick={() => void createBackup()}
            type="button"
          >
            {creating ? 'Creating…' : 'Create Manual Backup'}
          </button>
        </div>
      </section>

      <section className="panel">
        <div className="panel-head">
          <div>
            <span className="eyebrow">Backup Inventory</span>
            <h2>Local snapshots</h2>
            <p>
              Automatic: <code>{backups.automatic_backup_dir}</code>
            </p>
          </div>
        </div>
        <div className="backup-list">
          {backups.backups.length === 0 ? (
            <EmptyState
              compact
              title="No backups found yet."
              copy="Create a manual backup or perform a write operation to create one."
            />
          ) : (
            backups.backups.map((backup) => (
              <div className="backup-row" key={backup.path}>
                <div>
                  <strong>{backup.file_name}</strong>
                  <p>{backup.path}</p>
                </div>
                <div className="backup-meta">
                  <span className="mini-chip">{backup.backup_type}</span>
                  <span className="mini-chip">{formatBytes(backup.size_bytes)}</span>
                  <span className="mini-chip">
                    {backup.modified_at ? formatDateTime(backup.modified_at) : 'Unknown date'}
                  </span>
                  <button
                    className="provider-action-button provider-action-button--danger provider-action-button--compact"
                    disabled={restoring !== null}
                    onClick={() => void restoreBackup(backup)}
                    type="button"
                  >
                    {restoring === backup.path ? 'Restoring…' : 'Restore'}
                  </button>
                </div>
              </div>
            ))
          )}
        </div>
      </section>
    </section>
  )
}
