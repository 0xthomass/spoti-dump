export function Pagination({
  page,
  totalPages,
  onPageChange,
  compact,
}: {
  page: number
  totalPages: number
  onPageChange: (page: number) => void
  compact?: boolean
}) {
  if (!totalPages || totalPages <= 1) {
    return null
  }
  return (
    <div className={`pagination${compact ? ' pagination--compact' : ''}`}>
      <button disabled={page <= 1} onClick={() => onPageChange(page - 1)} type="button">
        Previous
      </button>
      <span>
        Page {page} of {totalPages}
      </span>
      <button
        disabled={page >= totalPages}
        onClick={() => onPageChange(page + 1)}
        type="button"
      >
        Next
      </button>
    </div>
  )
}
