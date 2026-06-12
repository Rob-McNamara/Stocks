import { useEffect, useMemo, useRef, useState } from 'react'
import { apiClient } from '../services/api'
import { calculateSMA } from '../utils/sma'

interface PriceHistoryPoint {
  date: string
  close: number | null
  volume: number | null
}

interface PriceChartProps {
  symbol: string
  onLoading: (loading: boolean) => void
}

function buildPath(points: Array<{ x: number; y: number | null }>) {
  const filtered = points.filter((point) => point.y !== null) as Array<{ x: number; y: number }>
  if (filtered.length === 0) return ''
  return filtered.map((point, index) => `${index === 0 ? 'M' : 'L'} ${point.x} ${point.y}`).join(' ')
}

export default function PriceChart({ symbol, onLoading }: PriceChartProps) {
  const [history, setHistory] = useState<PriceHistoryPoint[]>([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [smaPeriod, setSmaPeriod] = useState<20 | 50 | 100 | 150 | 200>(150)
  const [timeframe, setTimeframe] = useState<'12m' | '6m' | '3m' | '1m' | '1w'>('6m')
  const [hoverIndex, setHoverIndex] = useState<number | null>(null)
  const svgRef = useRef<SVGSVGElement>(null)

  useEffect(() => {
    if (!symbol) { setHistory([]); return }
    const loadHistory = async () => {
      try {
        setLoading(true)
        setError(null)
        onLoading(true)
        const data = await apiClient.getPriceHistory(symbol, 600)
        setHistory(data)
      } catch (err) {
        setError(err instanceof Error ? err.message : 'Failed to load price history')
      } finally {
        setLoading(false)
        onLoading(false)
      }
    }
    loadHistory()
  }, [symbol])

  const trimmedHistory = useMemo(() => {
    if (history.length === 0) return history
    const cutoff = new Date()
    if (timeframe === '12m') cutoff.setFullYear(cutoff.getFullYear() - 1)
    else if (timeframe === '6m') cutoff.setMonth(cutoff.getMonth() - 6)
    else if (timeframe === '3m') cutoff.setMonth(cutoff.getMonth() - 3)
    else if (timeframe === '1m') cutoff.setMonth(cutoff.getMonth() - 1)
    else cutoff.setDate(cutoff.getDate() - 7)
    const cutoffStr = cutoff.toISOString().slice(0, 10)
    const filtered = history.filter((item) => item.date >= cutoffStr)
    return filtered.length > 0 ? filtered : history.slice(-5)
  }, [history, timeframe])

  const sma = useMemo(() => calculateSMA(history, smaPeriod), [history, smaPeriod])

  const priceValues = trimmedHistory.map((item) => item.close).filter((v): v is number => v !== null)
  const latestPrice = priceValues[priceValues.length - 1]
  const latestSma = sma[history.length - 1]

  const chartData = useMemo(() => {
    const width = 1040
    const height = 260
    const left = 72   // wider left margin for Y-axis labels
    const right = 20
    const top = 20
    const bottom = 20
    const plotWidth = width - left - right
    const plotHeight = height - top - bottom

    const closeValues = trimmedHistory.map((item) => item.close)
    const validValues = closeValues.filter((val): val is number => val !== null)
    const rawMin = Math.min(...validValues)
    const rawMax = Math.max(...validValues)
    const padding = (rawMax - rawMin) * 0.05 || 1
    const minValue = rawMin - padding
    const maxValue = rawMax + padding
    const priceRange = maxValue - minValue || 1

    const toY = (v: number) => top + plotHeight - ((v - minValue) / priceRange) * plotHeight

    const points = trimmedHistory.map((item, index) => ({
      x: left + (plotWidth * index) / Math.max(trimmedHistory.length - 1, 1),
      y: item.close !== null ? toY(item.close) : null,
    }))

    const smaPoints = trimmedHistory.map((_, index) => {
      const globalIndex = history.length - trimmedHistory.length + index
      const value = sma[globalIndex]
      return {
        x: left + (plotWidth * index) / Math.max(trimmedHistory.length - 1, 1),
        y: value !== null ? toY(value) : null,
      }
    })

    const volumeValues = trimmedHistory.map((item) => item.volume ?? 0)
    const maxVolume = Math.max(...volumeValues, 1)
    const volumeHeight = 100
    const volumeTop = height + 40
    const volumePlotHeight = volumeHeight - 20

    const volumeBars = trimmedHistory.map((item, index) => {
      const x = left + (plotWidth * index) / Math.max(trimmedHistory.length - 1, 1)
      const barWidth = Math.max(4, plotWidth / trimmedHistory.length - 2)
      const volume = item.volume ?? 0
      const barHeight = (volume / maxVolume) * volumePlotHeight
      const y = volumeTop + volumePlotHeight - barHeight
      let color = '#8fb9ff'
      if (index > 0) {
        const prev = trimmedHistory[index - 1].close
        const curr = item.close
        if (prev !== null && curr !== null) color = curr >= prev ? '#4caf50' : '#f44336'
      }
      return { x: x - barWidth / 2, y, width: barWidth, height: barHeight, color }
    })

    // Y-axis gridlines and labels (5 levels)
    const yLabelCount = 5
    const yLabels = Array.from({ length: yLabelCount }, (_, i) => {
      const value = minValue + (priceRange * i) / (yLabelCount - 1)
      return { y: toY(value), label: `$${value.toFixed(2)}` }
    })

    const axisY = top + plotHeight
    const labelY = axisY + 18
    const labelCount = trimmedHistory.length <= 7 ? trimmedHistory.length : trimmedHistory.length <= 30 ? 4 : 6
    const xLabels: Array<{ x: number; label: string }> = []
    if (trimmedHistory.length > 0) {
      const indices = Array.from({ length: labelCount }, (_, i) =>
        Math.round((i / (labelCount - 1)) * (trimmedHistory.length - 1))
      )
      for (const idx of indices) {
        const item = trimmedHistory[idx]
        if (!item) continue
        const [year, month, day] = item.date.split('-')
        const months = ['Jan','Feb','Mar','Apr','May','Jun','Jul','Aug','Sep','Oct','Nov','Dec']
        const label = `${day} ${months[parseInt(month, 10) - 1]} '${year.slice(2)}`
        xLabels.push({ x: left + (plotWidth * idx) / Math.max(trimmedHistory.length - 1, 1), label })
      }
    }

    return {
      width: 1100, height: volumeTop + volumeHeight,
      points, smaPoints, volumeBars, yLabels, xLabels,
      left, right, top, bottom, plotWidth, plotHeight,
      pricePlotHeight: plotHeight, axisY, labelY,
    }
  }, [trimmedHistory, history.length, sma])

  // Convert SVG mouse position to nearest data index
  const handleMouseMove = (e: React.MouseEvent<SVGSVGElement>) => {
    const svg = svgRef.current
    if (!svg || trimmedHistory.length === 0) return
    const rect = svg.getBoundingClientRect()
    const svgX = ((e.clientX - rect.left) / rect.width) * chartData.width
    const plotX = svgX - chartData.left
    const idx = Math.round((plotX / chartData.plotWidth) * (trimmedHistory.length - 1))
    setHoverIndex(Math.max(0, Math.min(trimmedHistory.length - 1, idx)))
  }

  // Tooltip data for hovered point
  const hoverData = useMemo(() => {
    if (hoverIndex === null) return null
    const item = trimmedHistory[hoverIndex]
    if (!item) return null
    const globalIndex = history.length - trimmedHistory.length + hoverIndex
    const smaValue = sma[globalIndex] ?? null
    const x = chartData.points[hoverIndex]?.x ?? 0
    const priceY = chartData.points[hoverIndex]?.y ?? null
    const smaY = chartData.smaPoints[hoverIndex]?.y ?? null
    return { date: item.date, price: item.close, smaValue, x, priceY, smaY }
  }, [hoverIndex, trimmedHistory, history.length, sma, chartData])

  if (!symbol) return <p className="chart-message">Select a watchlist symbol to display the Simple Moving Average chart.</p>
  if (loading) return <p className="chart-message">Loading chart for {symbol}...</p>
  if (error) return <div className="alert alert-error">{error}</div>
  if (trimmedHistory.length === 0) return <p className="chart-message">No historical price data available for {symbol}.</p>

  // Tooltip box position — keep it within the chart
  const tooltipWidth = 160
  const tooltipX = hoverData
    ? hoverData.x + 10 + tooltipWidth > chartData.width - chartData.right
      ? hoverData.x - tooltipWidth - 10
      : hoverData.x + 10
    : 0

  return (
    <div className="price-chart-card">
      <div className="chart-summary">
        <div>
          <span className="chart-symbol">{symbol}</span>
          <span className="chart-value">{latestPrice ? `$${latestPrice.toFixed(2)}` : 'Price unavailable'}</span>
        </div>
        <div>
          <span className="chart-detail">{smaPeriod}-day SMA: {latestSma ? `$${latestSma.toFixed(2)}` : 'N/A'}</span>
          <span className="chart-detail">Last: {trimmedHistory[trimmedHistory.length - 1]?.date}</span>
          <div className="sma-selector">
            {(['1w', '1m', '3m', '6m', '12m'] as const).map((tf) => (
              <button key={tf} className={`sma-button ${timeframe === tf ? 'active' : ''}`} onClick={() => setTimeframe(tf)}>
                {tf === '1w' ? '1W' : tf === '1m' ? '1M' : tf === '3m' ? '3M' : tf === '6m' ? '6M' : '12M'}
              </button>
            ))}
            <span style={{ margin: '0 4px', color: '#ccc' }}>|</span>
            {([20, 50, 100, 150, 200] as const).map((p) => (
              <button key={p} className={`sma-button ${smaPeriod === p ? 'active' : ''}`} onClick={() => setSmaPeriod(p)}>
                SMA {p}
              </button>
            ))}
          </div>
        </div>
      </div>
      <div className="chart-frame">
        <svg
          ref={svgRef}
          viewBox={`0 0 ${chartData.width} ${chartData.height}`}
          className="chart-svg"
          onMouseMove={handleMouseMove}
          onMouseLeave={() => setHoverIndex(null)}
          style={{ cursor: 'crosshair' }}
        >
          <rect x="0" y="0" width={chartData.width} height={chartData.height} fill="#ffffff" rx="18" />

          {/* Y-axis gridlines and labels */}
          {chartData.yLabels.map(({ y, label }) => (
            <g key={label}>
              <line x1={chartData.left} y1={y} x2={chartData.width - chartData.right} y2={y} stroke="#e8edf5" strokeWidth="1" strokeDasharray="4 3" />
              <text x={chartData.left - 6} y={y + 4} textAnchor="end" fontSize="11" fill="#999" fontFamily="inherit">{label}</text>
            </g>
          ))}

          {/* Axes */}
          <line x1={chartData.left} y1={chartData.top} x2={chartData.left} y2={chartData.top + chartData.pricePlotHeight} stroke="#e1e7f1" strokeWidth="1" />
          <line x1={chartData.left} y1={chartData.top + chartData.pricePlotHeight} x2={chartData.width - chartData.right} y2={chartData.top + chartData.pricePlotHeight} stroke="#e1e7f1" strokeWidth="1" />

          {/* X-axis labels */}
          {chartData.xLabels.map(({ x, label }) => (
            <g key={label}>
              <line x1={x} y1={chartData.axisY} x2={x} y2={chartData.axisY + 5} stroke="#aaa" strokeWidth="1" />
              <text x={x} y={chartData.labelY} textAnchor="middle" fontSize="13" fill="#888" fontFamily="inherit">{label}</text>
            </g>
          ))}

          {/* Price and SMA lines */}
          <path d={buildPath(chartData.points)} fill="none" stroke="#2f5ce4" strokeWidth="2" />
          <path d={buildPath(chartData.smaPoints)} fill="none" stroke="#ff9800" strokeWidth="2" strokeDasharray="8 6" />

          {/* Volume bars */}
          {chartData.volumeBars.map((bar, index) => (
            <rect key={index} x={bar.x} y={bar.y} width={bar.width} height={bar.height} fill={bar.color} opacity="0.85" />
          ))}

          {/* Crosshair and tooltip */}
          {hoverData && (
            <g>
              {/* Vertical crosshair */}
              <line
                x1={hoverData.x} y1={chartData.top}
                x2={hoverData.x} y2={chartData.top + chartData.pricePlotHeight}
                stroke="#aaa" strokeWidth="1" strokeDasharray="4 3"
              />
              {/* Dot on price line */}
              {hoverData.priceY !== null && (
                <circle cx={hoverData.x} cy={hoverData.priceY} r="4" fill="#2f5ce4" stroke="#fff" strokeWidth="1.5" />
              )}
              {/* Dot on SMA line */}
              {hoverData.smaY !== null && (
                <circle cx={hoverData.x} cy={hoverData.smaY} r="4" fill="#ff9800" stroke="#fff" strokeWidth="1.5" />
              )}
              {/* Tooltip box */}
              <rect x={tooltipX} y={chartData.top + 4} width={tooltipWidth} height={hoverData.smaValue !== null ? 68 : 50} rx="6" fill="#1e2a3a" opacity="0.92" />
              <text x={tooltipX + 10} y={chartData.top + 20} fontSize="11" fill="#aac" fontFamily="inherit">{hoverData.date}</text>
              {hoverData.price !== null && (
                <text x={tooltipX + 10} y={chartData.top + 38} fontSize="13" fill="#fff" fontFamily="inherit" fontWeight="600">
                  Price: ${hoverData.price.toFixed(2)}
                </text>
              )}
              {hoverData.smaValue !== null && (
                <text x={tooltipX + 10} y={chartData.top + 58} fontSize="12" fill="#ff9800" fontFamily="inherit">
                  SMA {smaPeriod}: ${hoverData.smaValue.toFixed(2)}
                </text>
              )}
            </g>
          )}
        </svg>
      </div>
      <div className="chart-legend">
        <span className="legend-item"><span className="legend-swatch price-line" /> Closing Price</span>
        <span className="legend-item"><span className="legend-swatch sma-line" /> {smaPeriod}-day SMA</span>
        <span className="legend-item"><span className="legend-swatch" style={{ background: '#4caf50' }} /> Volume Up</span>
        <span className="legend-item"><span className="legend-swatch" style={{ background: '#f44336' }} /> Volume Down</span>
      </div>
    </div>
  )
}
