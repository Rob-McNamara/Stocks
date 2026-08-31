import { loadAppConfig } from './appConfig'

/** Single `app_config` key holding the chart's opening state, as JSON. */
export const CHART_DEFAULTS_KEY = 'chart_defaults'

export type ChartTimeframe = '1w' | '1m' | '3m' | '6m' | '12m' | '2y'
export type ChartType = 'line' | 'candle'
export type BarInterval = 'day' | 'week'

export interface ChartDefaults {
  timeframe: ChartTimeframe
  chartType: ChartType
  barInterval: BarInterval
  /** Overlay ids switched on when a chart opens. */
  overlays: string[]
  /** Opening height of the chart frame in pixels; draggable from there. */
  height: number
}

/**
 * Below this the price panel has no room; above it the chart stops being
 * readable. The floor is the volume panel (100) plus its gap (40) plus the
 * smallest usable price panel (120) — go under it and the SVG grows taller
 * than its box, which the frame's `overflow: auto` would show as a scrollbar.
 * A stored value outside the range is clamped, not discarded.
 */
export const CHART_HEIGHT_RANGE = { min: 260, max: 900 } as const

export const TIMEFRAMES: ReadonlyArray<[ChartTimeframe, string]> = [
  ['1w', '1W'], ['1m', '1M'], ['3m', '3M'], ['6m', '6M'], ['12m', '12M'], ['2y', '2Y'],
]

/** What the chart opened with before any of this was configurable. */
export const FALLBACK_CHART_DEFAULTS: ChartDefaults = {
  timeframe: '6m',
  chartType: 'line',
  barInterval: 'day',
  overlays: ['sma50', 'sma150'],
  height: 400,
}

/**
 * Read stored defaults, falling back field by field.
 *
 * Every field is validated rather than trusted: a value written by an older
 * version, or an overlay id that no longer exists, must not leave the chart in
 * a state its own controls cannot represent — the buttons would all read
 * inactive and nothing would explain why.
 */
export function parseChartDefaults(raw: string | undefined, knownOverlayIds: readonly string[]): ChartDefaults {
  if (!raw) return FALLBACK_CHART_DEFAULTS
  let parsed: Partial<ChartDefaults>
  try {
    parsed = JSON.parse(raw)
  } catch {
    return FALLBACK_CHART_DEFAULTS
  }

  const oneOf = <T extends string>(value: unknown, allowed: readonly T[], fallback: T): T =>
    typeof value === 'string' && (allowed as readonly string[]).includes(value) ? (value as T) : fallback

  return {
    timeframe: oneOf(parsed.timeframe, TIMEFRAMES.map(([v]) => v), FALLBACK_CHART_DEFAULTS.timeframe),
    chartType: oneOf(parsed.chartType, ['line', 'candle'] as const, FALLBACK_CHART_DEFAULTS.chartType),
    barInterval: oneOf(parsed.barInterval, ['day', 'week'] as const, FALLBACK_CHART_DEFAULTS.barInterval),
    overlays: Array.isArray(parsed.overlays)
      ? parsed.overlays.filter((id): id is string => typeof id === 'string' && knownOverlayIds.includes(id))
      : FALLBACK_CHART_DEFAULTS.overlays,
    height:
      typeof parsed.height === 'number' && Number.isFinite(parsed.height)
        ? Math.min(CHART_HEIGHT_RANGE.max, Math.max(CHART_HEIGHT_RANGE.min, Math.round(parsed.height)))
        : FALLBACK_CHART_DEFAULTS.height,
  }
}

export function loadChartDefaults(knownOverlayIds: readonly string[]): Promise<ChartDefaults> {
  return loadAppConfig().then((config) => parseChartDefaults(config[CHART_DEFAULTS_KEY], knownOverlayIds))
}
