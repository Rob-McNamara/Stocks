import { useMemo, useRef, useState } from 'react'
import type { PortfolioHistoryPoint } from '../services/api'

/**
 * Two categorical series, validated for colour-vision deficiency and contrast
 * against the card surface (worst adjacent pair ΔE 31 protan / 34 normal, both
 * ≥ 3:1 contrast). Assigned in fixed order — stocks is always blue whatever the
 * cash balance does.
 */
const STOCKS_COLOR = '#2f5ce4'
const CASH_COLOR = '#b0741c'

const WIDTH = 1100
const HEIGHT = 300
const LEFT = 78
const RIGHT = 18
const TOP = 16
const BOTTOM = 30

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']

function formatDate(iso: string): string {
  const [year, month, day] = iso.split('-')
  return `${day} ${MONTHS[parseInt(month, 10) - 1]} '${year.slice(2)}`
}

function formatAud(value: number): string {
  return `$${value.toLocaleString('en-AU', { minimumFractionDigits: 0, maximumFractionDigits: 0 })}`
}

export default function PortfolioHistoryChart({ series }: { series: PortfolioHistoryPoint[] }) {
  const [hoverIndex, setHoverIndex] = useState<number | null>(null)
  const svgRef = useRef<SVGSVGElement>(null)

  const chart = useMemo(() => {
    const plotWidth = WIDTH - LEFT - RIGHT
    const plotHeight = HEIGHT - TOP - BOTTOM
    const maxTotal = Math.max(...series.map((p) => p.total), 1)
    // Anchored to zero: a value chart truncated at its minimum exaggerates
    // every wobble into a cliff.
    const scale = (value: number) => TOP + plotHeight - (value / maxTotal) * plotHeight
    const xAt = (index: number) => LEFT + (plotWidth * index) / Math.max(series.length - 1, 1)

    // Stocks sit on the baseline, cash stacks on top, so the upper edge is the
    // portfolio total and the band heights are each component.
    const stocksArea = [
      ...series.map((p, i) => `${i === 0 ? 'M' : 'L'} ${xAt(i)} ${scale(p.stocks)}`),
      `L ${xAt(series.length - 1)} ${scale(0)}`,
      `L ${xAt(0)} ${scale(0)}`,
      'Z',
    ].join(' ')
    const totalArea = [
      ...series.map((p, i) => `${i === 0 ? 'M' : 'L'} ${xAt(i)} ${scale(p.total)}`),
      ...series
        .slice()
        .reverse()
        .map((p, i) => `L ${xAt(series.length - 1 - i)} ${scale(p.stocks)}`),
      'Z',
    ].join(' ')
    const totalLine = series.map((p, i) => `${i === 0 ? 'M' : 'L'} ${xAt(i)} ${scale(p.total)}`).join(' ')

    const yLabels = Array.from({ length: 5 }, (_, i) => {
      const value = (maxTotal * i) / 4
      return { y: scale(value), label: formatAud(value) }
    })

    // The outermost labels anchor inward: centred on the axis ends they would
    // overhang the viewBox and be clipped.
    const labelCount = Math.min(6, series.length)
    const xLabels = Array.from({ length: labelCount }, (_, i) => {
      const index = Math.round((i / Math.max(labelCount - 1, 1)) * (series.length - 1))
      const anchor = i === 0 ? 'start' : i === labelCount - 1 ? 'end' : 'middle'
      return { x: xAt(index), label: formatDate(series[index].date), anchor }
    })

    // Contribution days, marked on the baseline so a jump in the line is
    // visibly explained by money arriving rather than by a gain.
    const flows = series
      .map((p, i) => ({ x: xAt(i), flow: p.flow }))
      .filter((f) => Math.abs(f.flow) > 0.005)

    return { plotHeight, scale, xAt, stocksArea, totalArea, totalLine, yLabels, xLabels, flows }
  }, [series])

  if (series.length === 0) {
    return <p className="empty-text">No portfolio history yet — record a trade or a cash transaction to start the series.</p>
  }

  const handleMove = (event: React.MouseEvent<SVGSVGElement>) => {
    const svg = svgRef.current
    if (!svg) return
    const rect = svg.getBoundingClientRect()
    const x = ((event.clientX - rect.left) / rect.width) * WIDTH - LEFT
    const index = Math.round((x / (WIDTH - LEFT - RIGHT)) * (series.length - 1))
    setHoverIndex(Math.max(0, Math.min(series.length - 1, index)))
  }

  const hover = hoverIndex === null ? null : series[hoverIndex]
  const tooltipWidth = 190
  const tooltipHeight = hover && Math.abs(hover.flow) > 0.005 ? 96 : 78
  const hoverX = hoverIndex === null ? 0 : chart.xAt(hoverIndex)
  const tooltipX = hoverX + 12 + tooltipWidth > WIDTH - RIGHT ? hoverX - tooltipWidth - 12 : hoverX + 12

  return (
    <div>
      <div className="chart-frame">
        <svg
          ref={svgRef}
          viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
          className="chart-svg"
          style={{ cursor: 'crosshair' }}
          onMouseMove={handleMove}
          onMouseLeave={() => setHoverIndex(null)}
          role="img"
          aria-label="Portfolio value over time, split into shares and cash"
        >
          <rect x="0" y="0" width={WIDTH} height={HEIGHT} fill="#ffffff" rx="12" />

          {chart.yLabels.map(({ y, label }) => (
            <g key={label}>
              <line x1={LEFT} y1={y} x2={WIDTH - RIGHT} y2={y} stroke="#eef1f7" strokeWidth="1" />
              <text x={LEFT - 8} y={y + 4} textAnchor="end" fontSize="11" fill="#8a92a6" fontFamily="inherit">{label}</text>
            </g>
          ))}

          {/* Cash sits above shares; the 2px surface gap keeps the boundary
              readable where the two fills meet. */}
          <path d={chart.totalArea} fill={CASH_COLOR} opacity="0.85" />
          <path d={chart.stocksArea} fill={STOCKS_COLOR} opacity="0.85" stroke="#ffffff" strokeWidth="2" />
          <path d={chart.totalLine} fill="none" stroke={CASH_COLOR} strokeWidth="2" />

          {chart.flows.map(({ x, flow }, i) => (
            <circle
              key={i}
              cx={x}
              cy={TOP + chart.plotHeight}
              r="3"
              fill={flow > 0 ? '#2e7d32' : '#c62828'}
            >
              <title>{flow > 0 ? 'Money in' : 'Money out'}: {formatAud(Math.abs(flow))}</title>
            </circle>
          ))}

          {chart.xLabels.map(({ x, label, anchor }) => (
            <text key={label} x={x} y={HEIGHT - 8} textAnchor={anchor} fontSize="12" fill="#8a92a6" fontFamily="inherit">
              {label}
            </text>
          ))}

          {hover && (
            <g>
              <line x1={hoverX} y1={TOP} x2={hoverX} y2={TOP + chart.plotHeight} stroke="#b6bdcc" strokeWidth="1" strokeDasharray="4 3" />
              <circle cx={hoverX} cy={chart.scale(hover.total)} r="4" fill={CASH_COLOR} stroke="#fff" strokeWidth="2" />
              <circle cx={hoverX} cy={chart.scale(hover.stocks)} r="4" fill={STOCKS_COLOR} stroke="#fff" strokeWidth="2" />
              <rect x={tooltipX} y={TOP + 6} width={tooltipWidth} height={tooltipHeight} rx="6" fill="#1e2a3a" opacity="0.94" />
              <text x={tooltipX + 12} y={TOP + 26} fontSize="11" fill="#aab4c6" fontFamily="inherit">{formatDate(hover.date)}</text>
              <text x={tooltipX + 12} y={TOP + 46} fontSize="13" fill="#ffffff" fontWeight="600" fontFamily="inherit">
                Total: {formatAud(hover.total)}
              </text>
              <text x={tooltipX + 12} y={TOP + 64} fontSize="12" fill="#8fabff" fontFamily="inherit">
                Shares: {formatAud(hover.stocks)}
              </text>
              <text x={tooltipX + 12} y={TOP + 80} fontSize="12" fill="#e0a860" fontFamily="inherit">
                Cash: {formatAud(hover.cash)}
              </text>
              {Math.abs(hover.flow) > 0.005 && (
                <text x={tooltipX + 12} y={TOP + 96} fontSize="12" fill={hover.flow > 0 ? '#7bd88f' : '#ff8a80'} fontFamily="inherit">
                  {hover.flow > 0 ? 'Money in' : 'Money out'}: {formatAud(Math.abs(hover.flow))}
                </text>
              )}
            </g>
          )}
        </svg>
      </div>
      <div className="chart-legend">
        <span className="legend-item"><span className="legend-swatch" style={{ background: STOCKS_COLOR }} /> Shares</span>
        <span className="legend-item"><span className="legend-swatch" style={{ background: CASH_COLOR }} /> Cash</span>
        <span className="legend-item"><span className="legend-swatch" style={{ background: '#2e7d32' }} /> Money in</span>
        <span className="legend-item"><span className="legend-swatch" style={{ background: '#c62828' }} /> Money out</span>
      </div>
    </div>
  )
}
