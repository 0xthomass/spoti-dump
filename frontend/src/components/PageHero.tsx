import type { ReactNode } from 'react'

export function PageHero({
  eyebrow,
  title,
  copy,
  children,
}: {
  eyebrow: string
  title: string
  copy: string
  children?: ReactNode
}) {
  return (
    <section className="hero-panel">
      <div className="hero-copy">
        <span className="eyebrow">{eyebrow}</span>
        <h2>{title}</h2>
        <p>{copy}</p>
      </div>
      <div className="hero-aside">{children}</div>
    </section>
  )
}
