/**
 * The colour scale the heat maps share, so a +3% tile reads the same on the
 * holdings map and the sector rotation map.
 *
 * Green/red is kept because it is what this app already uses for profit and
 * loss everywhere else, and a heat map that inverted that convention would be
 * misread at a glance. Colour is never the only channel though: every tile with
 * room prints its own percentage, and the tooltip carries the exact figures —
 * so the maps stay readable with red-green colour vision deficiency, which is
 * precisely the axis a green-to-red ramp destroys.
 */
export type Rgb = { r: number; g: number; b: number }

export const GAIN_END: Rgb = { r: 27, g: 94, b: 32 } // #1b5e20
export const LOSS_END: Rgb = { r: 155, g: 26, b: 26 } // #9b1a1a
export const NEUTRAL: Rgb = { r: 236, g: 239, b: 241 } // #eceff1

/** Fill for a tile with no price to judge it by. */
export const NO_DATA: Rgb = { r: 207, g: 216, b: 220 } // #cfd8dc

function mix(from: Rgb, to: Rgb, t: number): Rgb {
  return {
    r: Math.round(from.r + (to.r - from.r) * t),
    g: Math.round(from.g + (to.g - from.g) * t),
    b: Math.round(from.b + (to.b - from.b) * t),
  }
}

/** Diverging fill for a percentage, fully saturated at ±`cap`. */
export function tileColor(pct: number | null, cap: number): Rgb {
  if (pct === null) return NO_DATA
  const t = Math.min(Math.abs(pct) / cap, 1)
  return mix(NEUTRAL, pct >= 0 ? GAIN_END : LOSS_END, t)
}

export const rgb = (c: Rgb) => `rgb(${c.r}, ${c.g}, ${c.b})`

/**
 * Label colour chosen from the tile's own luminance rather than a fixed value,
 * so text stays legible across the whole ramp — the ends are dark enough to
 * need white, the middle light enough to need near-black.
 */
export function labelColor(c: Rgb): string {
  const channel = (v: number) => {
    const s = v / 255
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4
  }
  const luminance = 0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
  return luminance > 0.45 ? '#1c2430' : '#ffffff'
}
