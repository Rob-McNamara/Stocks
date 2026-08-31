import { loadAppConfig } from './appConfig'

/** `app_config` key holding the content-width preference. */
export const LAYOUT_WIDTH_KEY = 'layout_width'

export type LayoutWidth = 'comfortable' | 'wide' | 'full'

export const LAYOUT_WIDTHS: ReadonlyArray<[LayoutWidth, string, string]> = [
  ['comfortable', 'Comfortable', 'The original fixed width — easiest to read on a wide monitor'],
  ['wide', 'Wide', 'Caps content at 1600px — more columns visible without going edge to edge'],
  ['full', 'Full width', 'Uses the whole window'],
]

/** What the app used before this was configurable. */
export const FALLBACK_LAYOUT_WIDTH: LayoutWidth = 'comfortable'

export function parseLayoutWidth(raw: string | undefined): LayoutWidth {
  return LAYOUT_WIDTHS.some(([value]) => value === raw) ? (raw as LayoutWidth) : FALLBACK_LAYOUT_WIDTH
}

/**
 * Applied as a data attribute on the root element rather than an inline style,
 * so the widths live in CSS beside the layout they affect and a change takes
 * effect immediately — no reload, and no width arithmetic in TypeScript.
 */
export function applyLayoutWidth(width: LayoutWidth): void {
  document.documentElement.dataset.layoutWidth = width
}

export async function initLayoutWidth(): Promise<LayoutWidth> {
  const config = await loadAppConfig()
  const width = parseLayoutWidth(config[LAYOUT_WIDTH_KEY])
  applyLayoutWidth(width)
  return width
}
