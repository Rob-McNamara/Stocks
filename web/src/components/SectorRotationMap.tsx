import { useState } from 'react'
import type { RotationPeriod, SectorRotationEntry, SectorRotationSet } from '../services/api'
import { labelColor, rgb, tileColor } from '../utils/heatColors'
import { formatSessionDate } from '../utils/sessionDate'
import SectorHoldings from './SectorHoldings'

/** Periods offered, as `[value, label]`. */
const PERIODS = [
  ['1d', '1D'],
  ['1w', '1W'],
  ['1m', '1M'],
  ['3m', '3M'],
] as const

/**
 * What a tile's number and colour measure. Against the benchmark comes first
 * and is the default: when the whole market moves, plain returns paint every
 * tile the same colour, and which sectors are leading — the point of the map —
 * disappears.
 */
const MODES = [
  ['relative', 'vs Benchmark'],
  ['return', 'vs Return'],
] as const

type Mode = (typeof MODES)[number][0]

/**
 * Percentage at which a tile's colour saturates, per period. Fixed rather than
 * scaled to the day's extremes, so a quiet day reads as quiet, and stated in
 * the legend. Longer periods move further, so they get a wider band.
 */
const SCALE_CAP: Record<RotationPeriod, number> = { '1d': 2, '1w': 4, '1m': 8, '3m': 12 }

/** The benchmark's own tile is black whatever it did, so it is never read as a sector. */
const BENCHMARK_FILL = '#111111'

const formatPct = (v: number | null) => (v == null ? '—' : `${v >= 0 ? '+' : ''}${v.toFixed(1)}%`)
const formatPoints = (v: number | null) => (v == null ? '—' : `${v >= 0 ? '+' : ''}${v.toFixed(1)} pp`)

/**
 * The configured sector sets as tiles: the benchmark first, in black, then
 * every sector strongest first for the chosen period. Each tile prints both
 * figures — its move against the benchmark and its own return — and is
 * coloured by whichever one is selected. Clicking a tile opens its fund's top
 * holdings below the tiles; clicking it again closes them.
 */
export default function SectorRotationMap({ sets }: { sets: SectorRotationSet[] }) {
  const [setName, setSetName] = useState<string | null>(null)
  const [period, setPeriod] = useState<RotationPeriod>('1m')
  const [mode, setMode] = useState<Mode>('relative')
  const [openSymbol, setOpenSymbol] = useState<string | null>(null)

  // Held by name and looked up each render, so a set renamed or removed on
  // the Configuration screen falls back to the first rather than to nothing.
  const active = sets.find((s) => s.name === setName) ?? sets[0]
  if (!active) return <p className="empty-text">No sector sets configured.</p>

  const cap = SCALE_CAP[period]
  const unit = mode === 'relative' ? ' pp' : '%'
  const benchmarkLabel = active.benchmark.name || active.benchmark.symbol
  const periodLabel = PERIODS.find(([value]) => value === period)?.[1] ?? period
  const asOfText = active.as_of ? formatSessionDate(active.as_of) : null
  // Only a tile the active set still has stays open, so switching sets or
  // editing one never leaves a panel without its tile.
  const open =
    openSymbol && (openSymbol === active.benchmark.symbol || active.sectors.some((s) => s.symbol === openSymbol))
      ? openSymbol
      : null
  const toggle = (symbol: string) => setOpenSymbol(open === symbol ? null : symbol)
  const shown = (entry: SectorRotationEntry) => (mode === 'relative' ? entry.relative[period] : entry.returns[period])

  // Strongest first; a sector with nothing to show sinks to the end.
  const sectors = [...active.sectors].sort((a, b) => {
    const x = shown(a)
    const y = shown(b)
    if (x == null) return y == null ? 0 : 1
    if (y == null) return -1
    return y - x
  })

  const pill = <T extends string>(value: T, label: string, current: T, choose: (v: T) => void) => (
    <button
      key={value}
      type="button"
      className={`sma-button ${current === value ? 'active' : ''}`}
      onClick={() => choose(value)}
      aria-pressed={current === value}
    >
      {label}
    </button>
  )

  return (
    <div className="rotation-map">
      <div className="heatmap-controls">
        <div style={{ display: 'flex', gap: 16, flexWrap: 'wrap' }}>
          {sets.length > 1 && (
            <div className="sma-selector" role="group" aria-label="Sector set">
              {sets.map((s) => pill(s.name, s.name, active.name, setSetName))}
            </div>
          )}
          <div className="sma-selector" role="group" aria-label="Period">
            {PERIODS.map(([value, label]) => pill(value, label, period, setPeriod))}
          </div>
          <div className="sma-selector" role="group" aria-label="Measure tiles">
            {MODES.map(([value, label]) => pill(value, label, mode, setMode))}
          </div>
        </div>
        <div className="heatmap-legend" aria-hidden="true">
          <span>−{cap}{unit}</span>
          <span className="heatmap-ramp" />
          <span>+{cap}{unit}</span>
        </div>
      </div>

      <p className="rotation-asof">
        {asOfText ? <>As of <strong>{asOfText}</strong></> : 'No prices yet'}
        {' · '}price return, dividends not included
      </p>

      <div className="rotation-grid">
        <button
          type="button"
          className={`rotation-tile${open === active.benchmark.symbol ? ' selected' : ''}`}
          style={{ background: BENCHMARK_FILL, color: '#ffffff' }}
          aria-expanded={open === active.benchmark.symbol}
          onClick={() => toggle(active.benchmark.symbol)}
          title={[
            `${active.benchmark.symbol}${active.benchmark.long_name ? ` — ${active.benchmark.long_name}` : ''}`,
            `Benchmark return (${periodLabel}): ${formatPct(active.benchmark.returns[period])}`,
            'Click for its top holdings',
          ].join('\n')}
        >
          <span className="rotation-symbol">{active.benchmark.symbol}</span>
          <span className="rotation-name">{benchmarkLabel}</span>
          <span className="rotation-value">{formatPct(active.benchmark.returns[period])}</span>
          <span className="rotation-sub">Benchmark</span>
        </button>

        {sectors.map((entry) => {
          const value = shown(entry)
          const fill = tileColor(value, cap)
          // A sector with no close on the as-of day is blank in every period;
          // say so, rather than leave it looking like a flat result.
          const behind =
            asOfText && entry.last_date && active.as_of && entry.last_date < active.as_of
              ? `No price for ${asOfText} — the newest is ${formatSessionDate(entry.last_date)}`
              : null
          const sub = behind
            ? `No price for ${asOfText}`
            : entry.returns[period] == null
              ? 'Not enough history'
              : mode === 'relative'
                ? `${formatPct(entry.returns[period])} return`
                : `${formatPoints(entry.relative[period])} vs ${benchmarkLabel}`
          return (
            <button
              type="button"
              key={entry.symbol}
              className={`rotation-tile${open === entry.symbol ? ' selected' : ''}`}
              style={{ background: rgb(fill), color: labelColor(fill) }}
              aria-expanded={open === entry.symbol}
              onClick={() => toggle(entry.symbol)}
              title={[
                `${entry.symbol}${entry.long_name ? ` — ${entry.long_name}` : ''}`,
                `Sector: ${entry.sector}`,
                `Return (${periodLabel}): ${formatPct(entry.returns[period])}`,
                `vs ${benchmarkLabel}: ${formatPoints(entry.relative[period])}`,
                ...(behind ? [behind] : []),
                'Click for its top holdings',
              ].join('\n')}
            >
              <span className="rotation-symbol">{entry.symbol}</span>
              <span className="rotation-name">{entry.sector}</span>
              <span className="rotation-value">{mode === 'relative' ? formatPoints(value) : formatPct(value)}</span>
              <span className="rotation-sub">{sub}</span>
            </button>
          )
        })}
      </div>

      {open && (
        <SectorHoldings
          key={`${active.name}|${open}`}
          setName={active.name}
          symbol={open}
          period={period}
          periodLabel={periodLabel}
          mode={mode}
          cap={cap}
          onClose={() => setOpenSymbol(null)}
        />
      )}
    </div>
  )
}
