import type { CSSProperties } from 'react'
import { coverMonogram, coverPalette } from '../lib/format'

export function Artwork({
  image,
  seed,
  title,
  size,
}: {
  image: string | null
  seed: string
  title: string
  size: 'row' | 'playlist' | 'hero'
}) {
  const monogram = coverMonogram(title)
  const palette = coverPalette(seed)
  return (
    <div
      className={`artwork artwork--${size}`}
      style={
        {
          '--cover-a': palette[0],
          '--cover-b': palette[1],
        } as CSSProperties
      }
    >
      {image ? <img alt={`Artwork for ${title}`} loading="lazy" src={image} /> : null}
      {!image ? <span>{monogram}</span> : null}
    </div>
  )
}
