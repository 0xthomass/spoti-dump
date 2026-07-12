import type { TrackIdentityConflict } from '../api/types'
import {
  formatDurationDelta,
  formatNumber,
  formatOptionalScorePercent,
  formatScorePercent,
  recommendationClassName,
} from '../lib/format'

export function ConflictEvidencePanel({
  conflict,
  compact,
}: {
  conflict: TrackIdentityConflict
  compact?: boolean
}) {
  const evidence = conflict.evidence
  const recommendationClass = recommendationClassName(evidence.recommendation.key)

  return (
    <div className={`conflict-evidence${compact ? ' conflict-evidence--compact' : ''}`}>
      <div className="conflict-evidence-head">
        <span className={`status-chip ${recommendationClass}`}>
          {evidence.recommendation.label}
        </span>
        <p>{evidence.recommendation.detail}</p>
      </div>
      <div className="conflict-evidence-grid">
        <EvidenceMetric
          label="Metadata score"
          value={formatScorePercent(evidence.metadata_similarity)}
        />
        <EvidenceMetric
          label="Provider confidence"
          value={formatOptionalScorePercent(evidence.provider_confidence)}
        />
        <EvidenceMetric
          label="Duration delta"
          value={formatDurationDelta(evidence.duration_delta_seconds)}
        />
        <EvidenceMetric
          label="Source impact"
          value={`${formatNumber(evidence.source_saved_tracks)} saved · ${formatNumber(
            evidence.source_playlist_entries,
          )} refs`}
        />
        <EvidenceMetric
          label="Candidate impact"
          value={`${formatNumber(evidence.candidate_saved_tracks)} saved · ${formatNumber(
            evidence.candidate_playlist_entries,
          )} refs`}
        />
      </div>
    </div>
  )
}

function EvidenceMetric({ label, value }: { label: string; value: string }) {
  return (
    <div className="evidence-metric">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  )
}
