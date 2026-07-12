import type { ReactNode } from 'react'

export function IconButton({
  children,
  label,
  onClick,
  destructive,
}: {
  children: ReactNode
  label: string
  onClick: () => void
  destructive?: boolean
}) {
  return (
    <button
      aria-label={label}
      className={`icon-button${destructive ? ' icon-button--destructive' : ''}`}
      onClick={onClick}
      type="button"
      title={label}
    >
      {children}
    </button>
  )
}
