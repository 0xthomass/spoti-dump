export function HeroStat({ label, value }: { label: string; value: string }) {
  return (
    <div className="hero-stat">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  )
}
