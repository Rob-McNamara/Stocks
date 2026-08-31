import { useMemo, useState } from 'react'
import type { PortfolioHolding } from '../services/api'
import { squarify, squarifyGrouped } from '../utils/treemap'

const WIDTH = 1100
const HEIGHT = 460

/** Strip reserved at the top of each sector for its name. */
const GROUP_HEADER = 20

/**
 * Holdings with no sector recorded still have to go somewhere. "Unassigned" is
 * kept distinct from the selectable "Others" sector — one means nobody has said,
 * the other means someone decided.
 */
const UNASSIGNED = 'Unassigned'

/**
 * Metrics a tile can be coloured by. Total return and today's move answer
 * different questions — "how is this position doing" versus "what moved today"
 * — and both are already on every holding, so both are offered.
 */
const METRICS = [
  ['pl', 'P/L'],
  ['today', 'Today'],
] as const

type Metric = (typeof METRICS)[number][0]

/**
 * Percentage at which the colour scale saturates, per metric.
 *
 * Scaling to the observed extremes would let one +2500% holding flatten every
 * other tile to near-neutral, so the band is fixed and the legend states it —
 * the ceiling is not a hidden assumption, and the map is comparable between
 * visits.
 *
 * The two metrics need very different bands. Lifetime returns run to hundreds
 * of percent; a day's move rarely leaves ±5%, and reading it against a ±50%
 * band renders the whole map blank.
 */
const SCALE_CAP: Record<Metric, number> = { pl: 50, today: 5 }

/**
 * Diverging scale endpoints.
 *
 * Green/red is kept because it is what this app already uses for profit and
 * loss everywhere else, and a heat map that inverted that convention would be
 * misread at a glance. Colour is never the only channel though: every tile with
 * room prints its own percentage, and the tooltip carries the exact figures —
 * so the map stays readable with red-green colour vision deficiency, which is
 * precisely the axis a green-to-red ramp destroys.
 */
const GAIN_END = { r: 27, g: 94, b: 32 } // #1b5e20
const LOSS_END = { r: 155, g: 26, b: 26 } // #9b1a1a
const NEUTRAL = { r: 236, g: 239, b: 241 } // #eceff1

function mix(from: typeof NEUTRAL, to: typeof NEUTRAL, t: number) {
  return {
    r: Math.round(from.r + (to.r - from.r) * t),
    g: Math.round(from.g + (to.g - from.g) * t),
    b: Math.round(from.b + (to.b - from.b) * t),
  }
}

function tileColor(pct: number | null, cap: number) {
  if (pct === null) return { r: 207, g: 216, b: 220 } // #cfd8dc — no price to judge
  const t = Math.min(Math.abs(pct) / cap, 1)
  return mix(NEUTRAL, pct >= 0 ? GAIN_END : LOSS_END, t)
}

const rgb = (c: typeof NEUTRAL) => `rgb(${c.r}, ${c.g}, ${c.b})`

/**
 * Label colour chosen from the tile's own luminance rather than a fixed value,
 * so text stays legible across the whole ramp — the ends are dark enough to
 * need white, the middle light enough to need near-black.
 */
function labelColor(c: typeof NEUTRAL): string {
  const channel = (v: number) => {
    const s = v / 255
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4
  }
  const luminance = 0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
  return luminance > 0.45 ? '#1c2430' : '#ffffff'
}

const formatAud = (value: number) =>
  `$${value.toLocaleString('en-AU', { minimumFractionDigits: 0, maximumFractionDigits: 0 })}`

/**
 * A long-held position can be up thousands of percent, where a decimal place is
 * noise and the extra characters are what push the label out of its tile.
 */
const formatPct = (pct: number | null | undefined) =>
  pct == null ? '—' : `${pct >= 0 ? '+' : ''}${pct.toFixed(Math.abs(pct) >= 100 ? 0 : 1)}%`

/**
 * Whether a label fits its tile.
 *
 * A fixed pixel threshold is not enough: it has to assume some typical label
 * width, and the outliers are exactly the tiles that break — a narrow tile
 * holding "+57150%" or a long symbol overflows a threshold tuned for "+12.3%".
 * 0.6em per character is a safe approximation of average advance width for the
 * digits and capitals these labels are made of.
 */
const labelFits = (text: string, fontSize: number, tileWidth: number) =>
  text.length * fontSize * 0.6 <= tileWidth - 8

/**
 * Holdings as a treemap: area is the position's value in AUD, colour is its
 * return. `current_value` is already AUD-normalised by the API, so tiles are
 * comparable across foreign holdings without any conversion here.
 */
export default function HoldingsHeatMap({
  holdings,
  onSelectSymbol,
}: {
  holdings: PortfolioHolding[]
  onSelectSymbol?: (symbol: string) => void
}) {
  const [metric, setMetric] = useState<Metric>('pl')
  const [grouped, setGrouped] = useState(true)

  const cap = SCALE_CAP[metric]
  // `pl_pct` already follows the baseline wherever one is set, so there is no
  // second measure to choose between.
  const metricOf = (holding: PortfolioHolding): number | null =>
    metric === 'today' ? holding.change_percent : holding.pl_pct

  const layout = useMemo(() => {
    const bySymbol = new Map(holdings.map((h) => [h.symbol, h]))
    const withHolding = (tile: { key: string; x: number; y: number; width: number; height: number }) => ({
      tile,
      holding: bySymbol.get(tile.key)!,
    })
    const inputs = holdings.map((h) => ({ key: h.symbol, value: h.current_value }))

    if (!grouped) {
      return { groups: [], tiles: squarify(inputs, WIDTH, HEIGHT).map(withHolding) }
    }

    // Insertion order decides nothing — the layout ranks groups by value — so
    // a plain Map keyed by sector name is enough.
    const bySector = new Map<string, typeof inputs>()
    for (const holding of holdings) {
      const sector = holding.sector?.trim() || UNASSIGNED
      const items = bySector.get(sector) ?? []
      items.push({ key: holding.symbol, value: holding.current_value })
      bySector.set(sector, items)
    }
    const groups = squarifyGrouped(
      [...bySector].map(([key, items]) => ({ key, items })),
      WIDTH,
      HEIGHT,
      GROUP_HEADER,
    )
    return { groups, tiles: groups.flatMap((g) => g.tiles).map(withHolding) }
  }, [holdings, grouped])

  const tiles = layout.tiles

  const totalValue = useMemo(
    () => holdings.reduce((sum, h) => sum + Math.max(h.current_value, 0), 0),
    [holdings],
  )

  if (tiles.length === 0) {
    return <p className="empty-text">No priced holdings to map.</p>
  }

  return (
    <div>
      <div className="heatmap-controls">
        {/* Same pill row the price chart uses for its timeframes, so the two
            charts are driven the same way. */}
        <div className="sma-selector" role="group" aria-label="Colour tiles by">
          {METRICS.map(([value, label]) => (
            <button
              key={value}
              type="button"
              className={`sma-button ${metric === value ? 'active' : ''}`}
              onClick={() => setMetric(value)}
              aria-pressed={metric === value}
            >
              {label}
            </button>
          ))}
        </div>
        <div className="sma-selector" role="group" aria-label="Group tiles by">
          <button
            type="button"
            className={`sma-button ${grouped ? 'active' : ''}`}
            onClick={() => setGrouped(true)}
            aria-pressed={grouped}
          >
            By Sector
          </button>
          <button
            type="button"
            className={`sma-button ${grouped ? '' : 'active'}`}
            onClick={() => setGrouped(false)}
            aria-pressed={!grouped}
          >
            Flat
          </button>
        </div>
        <div className="heatmap-legend" aria-hidden="true">
          <span>−{cap}%</span>
          <span className="heatmap-ramp" />
          <span>+{cap}%</span>
        </div>
      </div>

      <div className="chart-frame">
        <svg
          viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
          className="chart-svg"
          role="img"
          aria-label={`Holdings sized by value and coloured by ${metric === 'pl' ? 'profit and loss' : "today's move"}`}
        >
          <rect x="0" y="0" width={WIDTH} height={HEIGHT} fill="#ffffff" rx="12" />

          {/* Sector frames sit under the tiles: the band is the reserved header
              strip, and the outline separates neighbouring sectors more firmly
              than the white gap between individual tiles. */}
          {layout.groups.map((group) => (
            <rect
              key={`frame-${group.key}`}
              x={group.x}
              y={group.y}
              width={group.width}
              height={group.height}
              fill="#f4f6fa"
              stroke="#ffffff"
              strokeWidth="3"
            />
          ))}

          {tiles.map(({ tile, holding }) => {
            const pct = metricOf(holding)
            const fill = tileColor(pct, cap)
            const text = labelColor(fill)
            // A label that does not fit is dropped rather than allowed to spill
            // over its neighbours; the tooltip still carries everything, so the
            // tile stays useful without the text.
            const symbolSize = Math.min(18, Math.max(10, tile.width / 6))
            const pctSize = Math.min(14, Math.max(9, tile.width / 8))
            const pctText = formatPct(pct)
            const showSymbol =
              tile.height >= 26 && labelFits(holding.symbol, symbolSize, tile.width)
            const showPct =
              showSymbol && tile.height >= 40 && labelFits(pctText, pctSize, tile.width)
            const weight = totalValue > 0 ? (holding.current_value / totalValue) * 100 : 0
            return (
              <g
                key={tile.key}
                onClick={() => onSelectSymbol?.(holding.symbol)}
                style={{ cursor: onSelectSymbol ? 'pointer' : 'default' }}
              >
                <rect
                  x={tile.x}
                  y={tile.y}
                  width={tile.width}
                  height={tile.height}
                  fill={rgb(fill)}
                  stroke="#ffffff"
                  strokeWidth="2"
                />
                <title>
                  {holding.symbol}
                  {holding.long_name ? ` — ${holding.long_name}` : ''}
                  {/* Carried per tile because a narrow sector frame cannot
                      always show its own name — "Consumer Discretionary" does
                      not fit a one-holding group. */}
                  {'\n'}Sector: {holding.sector?.trim() || UNASSIGNED}
                  {'\n'}Value: {formatAud(holding.current_value)} ({weight.toFixed(1)}% of holdings)
                  {'\n'}P/L{holding.basis_date ? ` since ${holding.basis_date}` : ''}: {formatPct(holding.pl_pct)} ({formatAud(holding.pl)})
                  {'\n'}Today: {formatPct(holding.change_percent)}
                </title>
                {showSymbol && (
                  <text
                    x={tile.x + tile.width / 2}
                    y={tile.y + tile.height / 2 + (showPct ? -4 : 4)}
                    textAnchor="middle"
                    fontSize={symbolSize}
                    fontWeight="600"
                    fill={text}
                    fontFamily="inherit"
                    pointerEvents="none"
                  >
                    {holding.symbol}
                  </text>
                )}
                {showPct && (
                  <text
                    x={tile.x + tile.width / 2}
                    y={tile.y + tile.height / 2 + 16}
                    textAnchor="middle"
                    fontSize={pctSize}
                    fill={text}
                    fontFamily="inherit"
                    pointerEvents="none"
                  >
                    {pctText}
                  </text>
                )}
              </g>
            )
          })}

          {layout.groups.map((group) => {
            const weight = totalValue > 0 ? (group.value / totalValue) * 100 : 0
            const label = `${group.key} ${weight.toFixed(0)}%`
            // The header is only reserved on groups tall enough to spare it,
            // and the name still has to fit the width.
            const hasHeader = group.tiles.every((t) => t.y >= group.y + GROUP_HEADER - 0.001)
            if (!hasHeader || !labelFits(label, 12, group.width)) return null
            return (
              <text
                key={`label-${group.key}`}
                x={group.x + 6}
                y={group.y + 14}
                fontSize="12"
                fontWeight="600"
                fill="#5b6478"
                fontFamily="inherit"
                pointerEvents="none"
              >
                {label}
              </text>
            )
          })}
        </svg>
      </div>
    </div>
  )
}
