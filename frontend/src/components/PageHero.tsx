import type { ReactNode } from 'react'

export function PageHero({
  title,
  subtitle,
  children,
}: {
  title: string
  subtitle?: string
  children?: ReactNode
}) {
  return (
    <section className="hero-panel">
      <div className="hero-copy">
        <h2>{title}</h2>
        {subtitle ? <p>{subtitle}</p> : null}
      </div>
      {children ? <div className="hero-aside">{children}</div> : null}
    </section>
  )
}
