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
  currency?: string   // native currency of the stock from Yahoo (e.g. 'USD', 'GBP'); omit or 'AUD' for domestic
  onLoading: (loading: boolean) => void
  currentPrice?: number | null   // live price to inject if newer than history
  currentVolume?: number | null  // live volume to use alongside injected price
  currentPriceDate?: string | null  // actual trading date for the live price (may differ from today)
  purchasePrice?: number | null  // avg cost per share (AUD) — shown in Holdings chart header
}

const CURRENCY_SYMBOL: Record<string, string> = {
  AUD: '$', USD: 'US$', GBP: '£', EUR: '€', JPY: '¥', CAD: 'CA$', HKD: 'HK$', SGD: 'S$', NZD: 'NZ$',
}

const SMA_PERIODS = [20, 50, 100, 150, 200] as const
type SmaPeriod = typeof SMA_PERIODS[number]

const SMA_COLORS: Record<SmaPeriod, string> = {
  20:  '#9c27b0',
  50:  '#ff9800',
  100: '#00bcd4',
  150: '#f44336',
  200: '#4caf50',
}

function buildPath(points: Array<{ x: number; y: number | null }>) {
  const filtered = points.filter((p) => p.y !== null) as Array<{ x: number; y: number }>
  if (filtered.length === 0) return ''
  // Single point: draw a tiny horizontal stub so the point is visible
  if (filtered.length === 1) return `M ${filtered[0].x - 4} ${filtered[0].y} L ${filtered[0].x + 4} ${filtered[0].y}`
  return filtered.map((p, i) => `${i === 0 ? 'M' : 'L'} ${p.x} ${p.y}`).join(' ')
}

export default function PriceChart({ symbol, currency: currencyProp = 'AUD', onLoading, currentPrice, currentVolume, currentPriceDate, purchasePrice }: PriceChartProps) {
  const [history, setHistory] = useState<PriceHistoryPoint[]>([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [activePeriods, setActivePeriods] = useState<Set<SmaPeriod>>(new Set([50]))
  const [timeframe, setTimeframe] = useState<'12m' | '6m' | '3m' | '1m' | '1w'>('6m')
  const [hoverIndex, setHoverIndex] = useState<number | null>(null)
  const [showInAud, setShowInAud] = useState(false)
  const [fxRate, setFxRate] = useState<number | null>(null)
  const [fxLoading, setFxLoading] = useState(false)
  // detectedCurrency is resolved from symbol info — more reliable than the prop when
  // the parent's symbolInfo cache hasn't been populated yet for this symbol.
  const [detectedCurrency, setDetectedCurrency] = useState<string>('AUD')
  const svgRef = useRef<SVGSVGElement>(null)

  const isInternational = detectedCurrency !== 'AUD'

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

  // Resolve the true currency for this symbol, then fetch its FX rate.
  // We fetch symbol info ourselves so this works even when the parent's cache is stale.
  useEffect(() => {
    if (!symbol) return
    setShowInAud(false)
    setFxRate(null)
    setDetectedCurrency('AUD')

    // Use the prop as an immediate hint if it looks reliable
    const hint = currencyProp !== 'AUD' ? currencyProp : null

    apiClient.getSymbolInfo().then((symbols) => {
      const info = symbols.find((s) => s.symbol === symbol)
      const resolved = (info?.currency?.toUpperCase() ?? hint ?? 'AUD')
      setDetectedCurrency(resolved)

      if (resolved !== 'AUD') {
        const today = new Date().toISOString().slice(0, 10)
        setFxLoading(true)
        apiClient.getFxRateForDate(resolved, today)
          .then((result) => { if (result) setFxRate(result.rate) })
          .finally(() => setFxLoading(false))
      }
    }).catch(() => {
      // If symbol info fetch fails, fall back to the prop
      const fallback = currencyProp.toUpperCase()
      setDetectedCurrency(fallback)
      if (fallback !== 'AUD') {
        const today = new Date().toISOString().slice(0, 10)
        setFxLoading(true)
        apiClient.getFxRateForDate(fallback, today)
          .then((result) => { if (result) setFxRate(result.rate) })
          .finally(() => setFxLoading(false))
      }
    })
  }, [symbol, currencyProp])

  const togglePeriod = (period: SmaPeriod) => {
    setActivePeriods((prev) => {
      const next = new Set(prev)
      if (next.has(period)) {
        if (next.size > 1) next.delete(period) // keep at least one active
      } else {
        next.add(period)
      }
      return next
    })
  }

  // Always use the live current price for today's data point — it's more up-to-date than
  // whatever daily close Yahoo or the daemon stored (which can be a stale intraday snapshot).
  const effectiveHistory = useMemo(() => {
    if (!currentPrice) return history
    // Use the actual trading date from the price response; fall back to today
    const priceDate = currentPriceDate ?? new Date().toISOString().slice(0, 10)
    const liveVol = currentVolume ?? null
    if (history.length === 0) {
      return [{ date: priceDate, close: currentPrice, volume: liveVol }]
    }
    const last = history[history.length - 1]
    if (priceDate === last.date) {
      // Replace stored close with live price; prefer live volume over stored
      return [...history.slice(0, -1), { date: priceDate, close: currentPrice, volume: liveVol ?? last.volume }]
    }
    if (priceDate > last.date) {
      return [...history, { date: priceDate, close: currentPrice, volume: liveVol }]
    }
    return history
  }, [history, currentPrice, currentPriceDate, currentVolume])

  const trimmedHistory = useMemo(() => {
    if (effectiveHistory.length === 0) return effectiveHistory
    const cutoff = new Date()
    if (timeframe === '12m') cutoff.setFullYear(cutoff.getFullYear() - 1)
    else if (timeframe === '6m') cutoff.setMonth(cutoff.getMonth() - 6)
    else if (timeframe === '3m') cutoff.setMonth(cutoff.getMonth() - 3)
    else if (timeframe === '1m') cutoff.setMonth(cutoff.getMonth() - 1)
    else cutoff.setDate(cutoff.getDate() - 7)
    const cutoffStr = cutoff.toISOString().slice(0, 10)
    const filtered = effectiveHistory.filter((item) => item.date >= cutoffStr)
    return filtered.length > 0 ? filtered : effectiveHistory.slice(-5)
  }, [effectiveHistory, timeframe])

  // Compute all SMA series upfront (cheap — reused across renders)
  const allSmas = useMemo(() => {
    const result = {} as Record<SmaPeriod, (number | null)[]>
    for (const p of SMA_PERIODS) {
      result[p] = calculateSMA(effectiveHistory, p)
    }
    return result
  }, [effectiveHistory])

  // Multiplier converts native prices to AUD when toggled on
  const fxMultiplier = showInAud && fxRate ? fxRate : 1
  const displayCurrency = showInAud ? 'AUD' : detectedCurrency
  const currSym = CURRENCY_SYMBOL[displayCurrency] ?? displayCurrency

  const priceValues = trimmedHistory.map((item) => item.close).filter((v): v is number => v !== null)
  const latestPrice = priceValues.length > 0 ? priceValues[priceValues.length - 1] * fxMultiplier : null

  const chartData = useMemo(() => {
    const width = 1040
    const height = 260
    const left = 72
    const right = 20
    const top = 20
    const bottom = 20
    const plotWidth = width - left - right
    const plotHeight = height - top - bottom

    const closeValues = trimmedHistory.map((item) => item.close)
    const validValues = closeValues.filter((val): val is number => val !== null).map((v) => v * fxMultiplier)
    const rawMin = Math.min(...validValues)
    const rawMax = Math.max(...validValues)
    const padding = (rawMax - rawMin) * 0.05 || 1
    const minValue = rawMin - padding
    const maxValue = rawMax + padding
    const priceRange = maxValue - minValue || 1

    const toY = (v: number) => top + plotHeight - ((v - minValue) / priceRange) * plotHeight

    const points = trimmedHistory.map((item, index) => ({
      x: left + (plotWidth * index) / Math.max(trimmedHistory.length - 1, 1),
      y: item.close !== null ? toY(item.close * fxMultiplier) : null,
    }))

    const smaLines = SMA_PERIODS.map((period) => ({
      period,
      color: SMA_COLORS[period],
      points: trimmedHistory.map((_, index) => {
        const globalIndex = effectiveHistory.length - trimmedHistory.length + index
        const value = allSmas[period][globalIndex]
        return {
          x: left + (plotWidth * index) / Math.max(trimmedHistory.length - 1, 1),
          y: value !== null ? toY(value * fxMultiplier) : null,
        }
      }),
    }))

    const volumeValues = trimmedHistory.map((item) => item.volume ?? 0)
    const maxVolume = Math.max(...volumeValues, 1)
    const volumeHeight = 100
    const volumeTop = height + 40
    const volumePlotHeight = volumeHeight - 20

    const volumeBars = trimmedHistory.map((item, index) => {
      const x = left + (plotWidth * index) / Math.max(trimmedHistory.length - 1, 1)
      const barWidth = Math.min(20, Math.max(4, plotWidth / trimmedHistory.length - 2))
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

    const yLabelCount = 5
    const yLabels = Array.from({ length: yLabelCount }, (_, i) => {
      const value = minValue + (priceRange * i) / (yLabelCount - 1)
      return { y: toY(value), label: `${currSym}${value.toFixed(2)}` }
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
      points, smaLines, volumeBars, yLabels, xLabels,
      left, right, top, bottom, plotWidth, plotHeight,
      pricePlotHeight: plotHeight, axisY, labelY,
    }
  }, [trimmedHistory, effectiveHistory.length, allSmas, fxMultiplier, currSym])

  const handleMouseMove = (e: React.MouseEvent<SVGSVGElement>) => {
    const svg = svgRef.current
    if (!svg || trimmedHistory.length === 0) return
    const rect = svg.getBoundingClientRect()
    const svgX = ((e.clientX - rect.left) / rect.width) * chartData.width
    const plotX = svgX - chartData.left
    const idx = Math.round((plotX / chartData.plotWidth) * (trimmedHistory.length - 1))
    setHoverIndex(Math.max(0, Math.min(trimmedHistory.length - 1, idx)))
  }

  const hoverData = useMemo(() => {
    if (hoverIndex === null) return null
    const item = trimmedHistory[hoverIndex]
    if (!item) return null
    const globalIndex = effectiveHistory.length - trimmedHistory.length + hoverIndex
    const x = chartData.points[hoverIndex]?.x ?? 0
    const priceY = chartData.points[hoverIndex]?.y ?? null
    const smaValues = SMA_PERIODS.filter((p) => activePeriods.has(p)).map((p) => {
      const raw = allSmas[p][globalIndex] ?? null
      return {
        period: p,
        color: SMA_COLORS[p],
        value: raw !== null ? raw * fxMultiplier : null,
        y: chartData.smaLines.find((l) => l.period === p)?.points[hoverIndex]?.y ?? null,
      }
    })
    const displayPrice = item.close !== null ? item.close * fxMultiplier : null
    return { date: item.date, price: displayPrice, x, priceY, smaValues }
  }, [hoverIndex, trimmedHistory, effectiveHistory.length, allSmas, chartData, activePeriods, fxMultiplier])

  if (!symbol) return <p className="chart-message">Select a watchlist symbol to display the Simple Moving Average chart.</p>
  if (loading) return <p className="chart-message">Loading chart for {symbol}...</p>
  if (error) return <div className="alert alert-error">{error}</div>
  if (trimmedHistory.length === 0) return <p className="chart-message">No historical price data available for {symbol}.</p>

  const tooltipWidth = 170
  const tooltipX = hoverData
    ? hoverData.x + 10 + tooltipWidth > chartData.width - chartData.right
      ? hoverData.x - tooltipWidth - 10
      : hoverData.x + 10
    : 0
  const activeSmaValues = hoverData?.smaValues ?? []
  const tooltipHeight = 38 + (hoverData?.price !== null ? 18 : 0) + activeSmaValues.length * 18 + 4

  const activePeriodsArray = Array.from(activePeriods).sort((a, b) => a - b)

  return (
    <div className="price-chart-card">
      <div className="chart-summary">
        <div>
          <span className="chart-symbol">{symbol}</span>
          <span className="chart-value">{latestPrice !== null ? `${currSym}${latestPrice.toFixed(2)}` : 'Price unavailable'}</span>
          {purchasePrice != null && latestPrice !== null && (() => {
            // purchasePrice is always in AUD; convert to native currency when chart is in native mode
            const displayPurchase = (isInternational && !showInAud && fxRate)
              ? purchasePrice / fxRate
              : purchasePrice
            const pl = ((latestPrice - displayPurchase) / displayPurchase) * 100
            return (
              <>
                <span style={{ fontSize: 12, marginLeft: 10, color: '#888' }}>avg cost {currSym}{displayPurchase.toFixed(2)}</span>
                <span style={{ fontSize: 12, marginLeft: 6, fontWeight: 600, color: pl >= 0 ? '#2e7d32' : '#c62828' }}>
                  {pl >= 0 ? '+' : ''}{pl.toFixed(1)}%
                </span>
              </>
            )
          })()}
          <span style={{ fontSize: 12, marginLeft: 8, color: '#888' }}>
            {isInternational
              ? showInAud
                ? `AUD${fxRate ? ` (1 ${detectedCurrency} = ${fxRate.toFixed(4)} AUD)` : ''}`
                : detectedCurrency
              : 'AUD'}
          </span>
        </div>
        <div>
          <span className="chart-detail">Last: {trimmedHistory[trimmedHistory.length - 1]?.date}</span>
          <div className="sma-selector">
            {(['1w', '1m', '3m', '6m', '12m'] as const).map((tf) => (
              <button key={tf} className={`sma-button ${timeframe === tf ? 'active' : ''}`} onClick={() => setTimeframe(tf)}>
                {tf === '1w' ? '1W' : tf === '1m' ? '1M' : tf === '3m' ? '3M' : tf === '6m' ? '6M' : '12M'}
              </button>
            ))}
            <span style={{ margin: '0 4px', color: '#ccc' }}>|</span>
            {SMA_PERIODS.map((p) => (
              <button
                key={p}
                className={`sma-button ${activePeriods.has(p) ? 'active' : ''}`}
                style={activePeriods.has(p) ? { borderColor: SMA_COLORS[p], color: SMA_COLORS[p] } : {}}
                onClick={() => togglePeriod(p)}
                title={activePeriods.has(p) ? `Hide SMA ${p}` : `Show SMA ${p}`}
              >
                SMA {p}
              </button>
            ))}
            {isInternational && (
              <>
                <span style={{ margin: '0 4px', color: '#ccc' }}>|</span>
                <button
                  className={`sma-button ${showInAud ? 'active' : ''}`}
                  onClick={() => setShowInAud((v) => !v)}
                  disabled={fxLoading || (!fxRate && !showInAud)}
                  title={
                    fxLoading ? 'Fetching exchange rate…'
                    : !fxRate ? 'Exchange rate unavailable'
                    : showInAud ? `Switch to ${detectedCurrency}`
                    : 'Switch to AUD'
                  }
                >
                  {fxLoading ? '…' : showInAud ? 'AUD' : detectedCurrency}
                </button>
              </>
            )}
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

          {chartData.yLabels.map(({ y, label }) => (
            <g key={label}>
              <line x1={chartData.left} y1={y} x2={chartData.width - chartData.right} y2={y} stroke="#e8edf5" strokeWidth="1" strokeDasharray="4 3" />
              <text x={chartData.left - 6} y={y + 4} textAnchor="end" fontSize="11" fill="#999" fontFamily="inherit">{label}</text>
            </g>
          ))}

          <line x1={chartData.left} y1={chartData.top} x2={chartData.left} y2={chartData.top + chartData.pricePlotHeight} stroke="#e1e7f1" strokeWidth="1" />
          <line x1={chartData.left} y1={chartData.top + chartData.pricePlotHeight} x2={chartData.width - chartData.right} y2={chartData.top + chartData.pricePlotHeight} stroke="#e1e7f1" strokeWidth="1" />

          {chartData.xLabels.map(({ x, label }) => (
            <g key={label}>
              <line x1={x} y1={chartData.axisY} x2={x} y2={chartData.axisY + 5} stroke="#aaa" strokeWidth="1" />
              <text x={x} y={chartData.labelY} textAnchor="middle" fontSize="13" fill="#888" fontFamily="inherit">{label}</text>
            </g>
          ))}

          {/* SMA lines — rendered behind price line */}
          {chartData.smaLines
            .filter((line) => activePeriods.has(line.period as SmaPeriod))
            .map((line) => (
              <path
                key={line.period}
                d={buildPath(line.points)}
                fill="none"
                stroke={line.color}
                strokeWidth="2"
                strokeDasharray="8 6"
                opacity="0.9"
              />
            ))}

          {/* Price line */}
          <path d={buildPath(chartData.points)} fill="none" stroke="#2f5ce4" strokeWidth="2" />

          {/* Volume bars */}
          {chartData.volumeBars.map((bar, index) => (
            <rect key={index} x={bar.x} y={bar.y} width={bar.width} height={bar.height} fill={bar.color} opacity="0.85" />
          ))}

          {/* Crosshair and tooltip */}
          {hoverData && (
            <g>
              <line
                x1={hoverData.x} y1={chartData.top}
                x2={hoverData.x} y2={chartData.top + chartData.pricePlotHeight}
                stroke="#aaa" strokeWidth="1" strokeDasharray="4 3"
              />
              {hoverData.priceY !== null && (
                <circle cx={hoverData.x} cy={hoverData.priceY} r="4" fill="#2f5ce4" stroke="#fff" strokeWidth="1.5" />
              )}
              {hoverData.smaValues.map(({ period, color, y }) =>
                y !== null ? (
                  <circle key={period} cx={hoverData.x} cy={y} r="4" fill={color} stroke="#fff" strokeWidth="1.5" />
                ) : null
              )}
              <rect x={tooltipX} y={chartData.top + 4} width={tooltipWidth} height={tooltipHeight} rx="6" fill="#1e2a3a" opacity="0.92" />
              <text x={tooltipX + 10} y={chartData.top + 20} fontSize="11" fill="#aac" fontFamily="inherit">{hoverData.date}</text>
              {hoverData.price !== null && (
                <text x={tooltipX + 10} y={chartData.top + 38} fontSize="13" fill="#fff" fontFamily="inherit" fontWeight="600">
                  Price: {currSym}{hoverData.price.toFixed(2)}
                </text>
              )}
              {hoverData.smaValues.map(({ period, color, value }, i) =>
                value !== null ? (
                  <text key={period} x={tooltipX + 10} y={chartData.top + 56 + i * 18} fontSize="12" fill={color} fontFamily="inherit">
                    SMA {period}: {currSym}{value.toFixed(2)}
                  </text>
                ) : null
              )}
            </g>
          )}
        </svg>
      </div>
      <div className="chart-legend">
        <span className="legend-item"><span className="legend-swatch price-line" /> Closing Price</span>
        {activePeriodsArray.map((p) => (
          <span key={p} className="legend-item">
            <span className="legend-swatch" style={{ background: SMA_COLORS[p as SmaPeriod], opacity: 0.9 }} />
            {p}-day SMA
          </span>
        ))}
        <span className="legend-item"><span className="legend-swatch" style={{ background: '#4caf50' }} /> Volume Up</span>
        <span className="legend-item"><span className="legend-swatch" style={{ background: '#f44336' }} /> Volume Down</span>
      </div>
    </div>
  )
}
