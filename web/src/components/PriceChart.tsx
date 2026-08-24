import { useEffect, useMemo, useRef, useState } from 'react'
import { apiClient } from '../services/api'
import { calculateSMA, calculateEMA } from '../utils/sma'
import { toWeeklyBars } from '../utils/bars'

interface PriceHistoryPoint {
  date: string
  // OHLC is null for bars ingested before OHLC support existed — run the
  // backfill_ohlc binary to populate them. The chart falls back to a line.
  open: number | null
  high: number | null
  low: number | null
  close: number | null
  volume: number | null
}

const CANDLE_UP = '#4caf50'
const CANDLE_DOWN = '#f44336'

// User-set price levels share one blue: deeper than the price line so the two
// stay apart in line mode, and clear of the SMA palette (cyan at 100). The
// purchase dot and the breakthrough marker never appear on the same chart —
// Watchlist passes markers, Holdings passes purchasePrice.
const PURCHASE_COLOR = '#1565c0'
export const BREAKTHROUGH_COLOR = '#1565c0'
export const STOP_LOSS_COLOR = '#e91e63'
/** Purchase predates the chart range — pinned to the Y axis instead. */
const PURCHASE_AXIS_COLOR = '#ff9800'

interface PriceChartProps {
  symbol: string
  currency?: string   // native currency of the stock from Yahoo (e.g. 'USD', 'GBP'); omit or 'AUD' for domestic
  onLoading: (loading: boolean) => void
  currentPrice?: number | null   // live price to inject if newer than history
  currentVolume?: number | null  // live volume to use alongside injected price
  currentPriceDate?: string | null  // actual trading date for the live price (may differ from today)
  purchasePrice?: number | null  // avg cost per share — shown in Holdings chart header
  purchaseDate?: string | null   // earliest purchase date (YYYY-MM-DD) for dot placement
  /**
   * Every purchase lot still held, so a position built in several parcels is
   * marked at each one. Falls back to the single averaged dot when absent.
   */
  purchases?: Array<{ date: string; price: number }>
  markerPrice?: number | null    // price level to mark with a dot (e.g. breakthrough price, stop loss)
  markerLabel?: string           // label for the marker (e.g. "Breakthrough", "Stop Loss")
  markerMode?: 'breakthrough' | 'stoploss'
  markers?: Array<{ price: number; label: string; mode: 'breakthrough' | 'stoploss'; color: string }>
}

const CURRENCY_SYMBOL: Record<string, string> = {
  AUD: '$', USD: 'US$', GBP: '£', EUR: '€', JPY: '¥', CAD: 'CA$', HKD: 'HK$', SGD: 'S$', NZD: 'NZ$',
}

/**
 * Moving-average overlays, ordered by period so the buttons read left to right.
 * Identified by id rather than period because the period alone no longer says
 * which kind of average it is — 40 is exponential, the rest are simple.
 */
interface OverlayDef {
  id: string
  label: string
  period: number
  kind: 'sma' | 'ema'
  color: string
  /** EMA gets its own dash so the two kinds are distinguishable at a glance. */
  dash: string
}

const OVERLAYS: readonly OverlayDef[] = [
  { id: 'sma20',  label: 'SMA 20',  period: 20,  kind: 'sma', color: '#9c27b0', dash: '8 6' },
  { id: 'ema40',  label: 'EMA 40',  period: 40,  kind: 'ema', color: '#795548', dash: '3 4' },
  { id: 'sma50',  label: 'SMA 50',  period: 50,  kind: 'sma', color: '#ff9800', dash: '8 6' },
  { id: 'sma100', label: 'SMA 100', period: 100, kind: 'sma', color: '#00bcd4', dash: '8 6' },
  { id: 'sma150', label: 'SMA 150', period: 150, kind: 'sma', color: '#f44336', dash: '8 6' },
  { id: 'sma200', label: 'SMA 200', period: 200, kind: 'sma', color: '#4caf50', dash: '8 6' },
]

function buildPath(points: Array<{ x: number; y: number | null }>) {
  const filtered = points.filter((p) => p.y !== null) as Array<{ x: number; y: number }>
  if (filtered.length === 0) return ''
  // Single point: draw a tiny horizontal stub so the point is visible
  if (filtered.length === 1) return `M ${filtered[0].x - 4} ${filtered[0].y} L ${filtered[0].x + 4} ${filtered[0].y}`
  return filtered.map((p, i) => `${i === 0 ? 'M' : 'L'} ${p.x} ${p.y}`).join(' ')
}

export default function PriceChart({ symbol, currency: currencyProp = 'AUD', onLoading, currentPrice, currentVolume, currentPriceDate, purchasePrice, purchaseDate, purchases, markerPrice, markerLabel, markerMode = 'breakthrough', markers }: PriceChartProps) {
  const [history, setHistory] = useState<PriceHistoryPoint[]>([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [activeOverlays, setActiveOverlays] = useState<Set<string>>(new Set(['sma50', 'sma150']))
  const [timeframe, setTimeframe] = useState<'2y' | '12m' | '6m' | '3m' | '1m' | '1w'>('6m')
  const [chartType, setChartType] = useState<'line' | 'candle'>('line')
  const [barInterval, setBarInterval] = useState<'day' | 'week'>('day')
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

  // Resolve the currency for this symbol, then fetch its FX rate. When the
  // parent passes a non-AUD currency (from its symbolInfo cache) we trust it;
  // only when the prop says AUD — which can also mean "unknown" — do we
  // double-check against symbol_info.
  useEffect(() => {
    if (!symbol) return
    setShowInAud(false)
    setFxRate(null)

    const applyCurrency = (resolved: string) => {
      setDetectedCurrency(resolved)
      if (resolved !== 'AUD') {
        const today = new Date().toISOString().slice(0, 10)
        setFxLoading(true)
        apiClient.getFxRateForDate(resolved, today)
          .then((result) => { if (result) setFxRate(result.rate) })
          .finally(() => setFxLoading(false))
      }
    }

    const propCurrency = currencyProp.toUpperCase()
    if (propCurrency !== 'AUD') {
      applyCurrency(propCurrency)
      return
    }
    setDetectedCurrency('AUD')
    apiClient.getSymbolInfo().then((symbols) => {
      const info = symbols.find((s) => s.symbol === symbol)
      applyCurrency(info?.currency?.toUpperCase() ?? 'AUD')
    }).catch(() => applyCurrency('AUD'))
  }, [symbol, currencyProp])

  const toggleOverlay = (id: string) => {
    setActiveOverlays((prev) => {
      const next = new Set(prev)
      if (next.has(id)) {
        if (next.size > 1) next.delete(id) // keep at least one active
      } else {
        next.add(id)
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
      return [{ date: priceDate, open: null, high: null, low: null, close: currentPrice, volume: liveVol }]
    }
    const last = history[history.length - 1]
    if (priceDate === last.date) {
      // Replace stored close with live price; prefer live volume over stored.
      // Keep the stored session OHLC, but stretch high/low to cover the live
      // price — an intraday move past the last stored extreme would otherwise
      // render a candle whose close sits outside its own wick.
      return [...history.slice(0, -1), {
        date: priceDate,
        open: last.open,
        high: last.high !== null ? Math.max(last.high, currentPrice) : null,
        low: last.low !== null ? Math.min(last.low, currentPrice) : null,
        close: currentPrice,
        volume: liveVol ?? last.volume,
      }]
    }
    if (priceDate > last.date) {
      // A brand-new day with no stored bar yet: only the close is known.
      return [...history, { date: priceDate, open: null, high: null, low: null, close: currentPrice, volume: liveVol }]
    }
    return history
  }, [history, currentPrice, currentPriceDate, currentVolume])

  /**
   * The bars the chart actually draws. Weekly aggregation happens here, before
   * the moving averages and the timeframe trim, so the averages are computed on
   * the same bars that are plotted — a 50-period average over weekly bars spans
   * 50 weeks, which is the conventional behaviour when changing interval.
   */
  const seriesHistory = useMemo(
    () => (barInterval === 'week' ? toWeeklyBars(effectiveHistory) : effectiveHistory),
    [effectiveHistory, barInterval],
  )

  const trimmedHistory = useMemo(() => {
    if (seriesHistory.length === 0) return seriesHistory
    const cutoff = new Date()
    if (timeframe === '2y') cutoff.setFullYear(cutoff.getFullYear() - 2)
    else if (timeframe === '12m') cutoff.setFullYear(cutoff.getFullYear() - 1)
    else if (timeframe === '6m') cutoff.setMonth(cutoff.getMonth() - 6)
    else if (timeframe === '3m') cutoff.setMonth(cutoff.getMonth() - 3)
    else if (timeframe === '1m') cutoff.setMonth(cutoff.getMonth() - 1)
    else cutoff.setDate(cutoff.getDate() - 7)
    const cutoffStr = cutoff.toISOString().slice(0, 10)
    const filtered = seriesHistory.filter((item) => item.date >= cutoffStr)
    return filtered.length > 0 ? filtered : seriesHistory.slice(-5)
  }, [seriesHistory, timeframe])

  /**
   * Averages are computed on the plotted bars, so a period counts whatever the
   * interval is: SMA 150 spans 150 days in Day mode and 150 weeks in Week mode.
   * The legend states the unit, since "150" alone would be ambiguous.
   *
   * A consequence worth knowing: the longer averages have no line in Week mode.
   * 600 daily rows collapse to roughly 125 weekly bars, fewer than the 150 and
   * 200 those averages need, and only about two years of history is stored so
   * fetching more would not fill them either.
   */
  const allOverlays = useMemo(() => {
    const result: Record<string, (number | null)[]> = {}
    for (const o of OVERLAYS) {
      result[o.id] = o.kind === 'ema'
        ? calculateEMA(seriesHistory, o.period)
        : calculateSMA(seriesHistory, o.period)
    }
    return result
  }, [seriesHistory])

  // Multiplier converts native prices to AUD when toggled on
  const fxMultiplier = showInAud && fxRate ? fxRate : 1
  const displayCurrency = showInAud ? 'AUD' : detectedCurrency
  const currSym = CURRENCY_SYMBOL[displayCurrency] ?? displayCurrency

  const priceValues = trimmedHistory.map((item) => item.close).filter((v): v is number => v !== null)
  const latestPrice = priceValues.length > 0 ? priceValues[priceValues.length - 1] * fxMultiplier : null

  // Candles need a complete bar. Older history is close-only until the backfill
  // has run, so the toggle stays disabled rather than drawing an empty chart.
  const hasOhlc = useMemo(
    () => trimmedHistory.some((item) => item.open !== null && item.high !== null && item.low !== null),
    [trimmedHistory]
  )
  const showCandles = chartType === 'candle' && hasOhlc

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
    // In candle mode the extremes are the wicks, not the closes — without this
    // the highs and lows clip outside the plot area.
    if (showCandles) {
      for (const item of trimmedHistory) {
        if (item.high !== null) validValues.push(item.high * fxMultiplier)
        if (item.low !== null) validValues.push(item.low * fxMultiplier)
      }
    }
    // Include marker and purchase prices in the range so dots are always visible
    const markerValues: number[] = []
    if (markerPrice != null) markerValues.push(markerPrice * fxMultiplier)
    if (markers) markers.forEach((m) => markerValues.push(m.price * fxMultiplier))
    if (purchasePrice != null) {
      markerValues.push(purchasePrice * fxMultiplier)
    }
    // Every plotted lot has to fit, or a parcel bought well away from the
    // average is drawn outside the plot and silently clipped.
    if (purchases) purchases.forEach((p) => markerValues.push(p.price * fxMultiplier))
    const rawMin = Math.min(...validValues, ...markerValues)
    const rawMax = Math.max(...validValues, ...markerValues)
    const padding = (rawMax - rawMin) * 0.05 || 1
    const minValue = rawMin - padding
    const maxValue = rawMax + padding
    const priceRange = maxValue - minValue || 1

    const toY = (v: number) => top + plotHeight - ((v - minValue) / priceRange) * plotHeight

    const points = trimmedHistory.map((item, index) => ({
      x: left + (plotWidth * index) / Math.max(trimmedHistory.length - 1, 1),
      y: item.close !== null ? toY(item.close * fxMultiplier) : null,
    }))

    const overlayLines = OVERLAYS.map((overlay) => ({
      id: overlay.id,
      color: overlay.color,
      dash: overlay.dash,
      points: trimmedHistory.map((_, index) => {
        const globalIndex = seriesHistory.length - trimmedHistory.length + index
        const value = allOverlays[overlay.id][globalIndex]
        return {
          x: left + (plotWidth * index) / Math.max(trimmedHistory.length - 1, 1),
          y: value !== null ? toY(value * fxMultiplier) : null,
        }
      }),
    }))

    // Candle bodies span open→close, wicks span low→high. Colour is close vs
    // the bar's *own* open (candlestick convention), which is not the same as
    // the volume bars' close-vs-previous-close rule.
    const candleWidth = Math.max(1, Math.min(14, plotWidth / Math.max(trimmedHistory.length, 1) - 1))
    const candles = trimmedHistory.map((item, index) => {
      const x = left + (plotWidth * index) / Math.max(trimmedHistory.length - 1, 1)
      const { open, high, low, close } = item
      if (open === null || high === null || low === null || close === null) return null
      const openY = toY(open * fxMultiplier)
      const closeY = toY(close * fxMultiplier)
      const rising = close >= open
      // A doji (open === close) would otherwise be a zero-height invisible rect
      const bodyHeight = Math.max(1, Math.abs(closeY - openY))
      return {
        x: x - candleWidth / 2,
        centerX: x,
        width: candleWidth,
        bodyY: Math.min(openY, closeY),
        bodyHeight,
        highY: toY(high * fxMultiplier),
        lowY: toY(low * fxMultiplier),
        color: rising ? CANDLE_UP : CANDLE_DOWN,
      }
    }).filter((c): c is NonNullable<typeof c> => c !== null)

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
      points, overlayLines, candles, volumeBars, yLabels, xLabels,
      left, right, top, bottom, plotWidth, plotHeight,
      pricePlotHeight: plotHeight, axisY, labelY, toY, minValue, maxValue,
    }
  }, [trimmedHistory, seriesHistory.length, allOverlays, fxMultiplier, currSym, markerPrice, markers, purchasePrice, purchases, isInternational, showInAud, fxRate, showCandles])

  const allMarkers = useMemo(() => {
    const defs: Array<{ price: number; label: string; mode: 'breakthrough' | 'stoploss'; color: string }> = []
    if (markers) defs.push(...markers)
    if (markerPrice != null) defs.push({ price: markerPrice, label: markerLabel ?? 'Marker', mode: markerMode, color: markerMode === 'stoploss' ? STOP_LOSS_COLOR : BREAKTHROUGH_COLOR })
    return defs
  }, [markers, markerPrice, markerLabel, markerMode])

  const markerDots = useMemo(() => {
    if (trimmedHistory.length === 0) return []
    const latestClose = trimmedHistory[trimmedHistory.length - 1]?.close
    if (latestClose == null) return []
    const lastIdx = trimmedHistory.length - 1
    const todayX = chartData.left + chartData.plotWidth
    const tomorrowX = chartData.left + chartData.plotWidth + 15

    return allMarkers.map((m) => {
      const mp = m.price * fxMultiplier
      const y = chartData.toY(mp)
      const priceAbove = latestClose * fxMultiplier >= mp

      if (m.mode === 'stoploss') {
        const x = priceAbove ? tomorrowX : todayX
        return { x, y, label: m.label, color: m.color, price: mp }
      }

      // Breakthrough mode
      if (priceAbove) {
        for (let i = lastIdx; i >= 0; i--) {
          const close = trimmedHistory[i].close
          if (close !== null && close * fxMultiplier < mp) {
            const crossIdx = Math.min(i + 1, lastIdx)
            const x = chartData.left + (chartData.plotWidth * crossIdx) / Math.max(lastIdx, 1)
            return { x, y, label: m.label, color: m.color, price: mp }
          }
        }
        return { x: chartData.left, y, label: m.label, color: m.color, price: mp }
      } else {
        return { x: tomorrowX, y, label: m.label, color: m.color, price: mp }
      }
    }).filter((d): d is NonNullable<typeof d> => d !== null)
  }, [allMarkers, trimmedHistory, chartData, fxMultiplier])

  /**
   * One dot per purchase still held, or a single averaged dot when the caller
   * has not supplied the individual lots.
   *
   * A buy predating the visible window has no x position on this chart, so it
   * is pinned to the Y axis in a different colour rather than being drawn at
   * the left edge as though it happened then. Several such buys would stack on
   * the same spot, so they collapse to one axis marker at the average price.
   */
  const purchaseDots = useMemo(() => {
    if (trimmedHistory.length === 0) return []
    const firstDate = trimmedHistory[0].date
    const place = (price: number, date: string | null) => {
      const displayPrice = price * fxMultiplier
      const y = chartData.toY(displayPrice)
      if (date && date >= firstDate) {
        const idx = trimmedHistory.findIndex((h) => h.date >= date)
        const i = idx >= 0 ? idx : trimmedHistory.length - 1
        const x = chartData.left + (chartData.plotWidth * i) / Math.max(trimmedHistory.length - 1, 1)
        return { x, y, price: displayPrice, date, onAxis: false }
      }
      return { x: chartData.left, y, price: displayPrice, date, onAxis: true }
    }

    if (purchases && purchases.length > 0) {
      const visible = purchases.filter((p) => p.date >= firstDate).map((p) => place(p.price, p.date))
      const earlier = purchases.filter((p) => p.date < firstDate)
      if (earlier.length > 0) {
        const avg = earlier.reduce((sum, p) => sum + p.price, 0) / earlier.length
        visible.push(place(avg, null))
      }
      return visible
    }
    if (purchasePrice == null) return []
    return [place(purchasePrice, purchaseDate ?? null)]
  }, [purchases, purchasePrice, purchaseDate, trimmedHistory, chartData, fxMultiplier, isInternational, showInAud, fxRate])

  // The tooltip and the y-range logic below still reason about a single
  // representative purchase.
  const purchaseDot = purchaseDots[0] ?? null

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
    const globalIndex = seriesHistory.length - trimmedHistory.length + hoverIndex
    const x = chartData.points[hoverIndex]?.x ?? 0
    const priceY = chartData.points[hoverIndex]?.y ?? null
    const overlayValues = OVERLAYS.filter((o) => activeOverlays.has(o.id)).map((o) => {
      const raw = allOverlays[o.id][globalIndex] ?? null
      return {
        id: o.id,
        label: o.label,
        color: o.color,
        value: raw !== null ? raw * fxMultiplier : null,
        y: chartData.overlayLines.find((l) => l.id === o.id)?.points[hoverIndex]?.y ?? null,
      }
    })
    const displayPrice = item.close !== null ? item.close * fxMultiplier : null
    const ohlc = showCandles && item.open !== null && item.high !== null && item.low !== null
      ? { open: item.open * fxMultiplier, high: item.high * fxMultiplier, low: item.low * fxMultiplier }
      : null
    return { date: item.date, price: displayPrice, x, priceY, overlayValues, ohlc }
  }, [hoverIndex, trimmedHistory, seriesHistory.length, allOverlays, chartData, activeOverlays, fxMultiplier, showCandles])

  if (!symbol) return <p className="chart-message">Select a watchlist symbol to display the stock chart.</p>
  if (loading) return <p className="chart-message">Loading chart for {symbol}...</p>
  if (error) return <div className="alert alert-error">{error}</div>
  if (trimmedHistory.length === 0) return <p className="chart-message">No historical price data available for {symbol}.</p>

  const tooltipWidth = 170
  const tooltipX = hoverData
    ? hoverData.x + 10 + tooltipWidth > chartData.width - chartData.right
      ? hoverData.x - tooltipWidth - 10
      : hoverData.x + 10
    : 0
  const activeOverlayValues = hoverData?.overlayValues ?? []
  // Candle mode adds Open/High/Low rows above the SMA block, pushing every
  // following row down by the same amount.
  const ohlcRows = hoverData?.ohlc ? 3 : 0
  const tooltipHeight = 38 + (hoverData?.price !== null ? 18 : 0) + ohlcRows * 18 + activeOverlayValues.length * 18 + markerDots.length * 18 + (purchaseDot ? 18 : 0) + 4
  const tooltipRowY = (row: number) => chartData.top + 56 + ohlcRows * 18 + row * 18

  const activeOverlayDefs = OVERLAYS.filter((o) => activeOverlays.has(o.id))

  return (
    <div className="price-chart-card">
      <div className="chart-summary">
        <div>
          <span className="chart-symbol">{symbol}</span>
          <span className="chart-value">{latestPrice !== null ? `${currSym}${latestPrice.toFixed(2)}` : 'Price unavailable'}</span>
          {purchasePrice != null && latestPrice !== null && (() => {
            const displayPurchase = purchasePrice * fxMultiplier
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
            {(['1w', '1m', '3m', '6m', '12m', '2y'] as const).map((tf) => (
              <button key={tf} className={`sma-button ${timeframe === tf ? 'active' : ''}`} onClick={() => setTimeframe(tf)}>
                {tf === '1w' ? '1W' : tf === '1m' ? '1M' : tf === '3m' ? '3M' : tf === '6m' ? '6M' : tf === '12m' ? '12M' : '2Y'}
              </button>
            ))}
            <span style={{ margin: '0 4px', color: '#ccc' }}>|</span>
            <button
              className={`sma-button ${chartType === 'line' ? 'active' : ''}`}
              onClick={() => setChartType('line')}
              title="Show the closing price as a line"
            >
              Line
            </button>
            <button
              className={`sma-button ${showCandles ? 'active' : ''}`}
              onClick={() => setChartType('candle')}
              disabled={!hasOhlc}
              title={hasOhlc
                ? 'Show open/high/low/close candlesticks'
                : 'No OHLC data stored for this period — run the backfill_ohlc tool'}
            >
              Candles
            </button>
            <span style={{ margin: '0 4px', color: '#ccc' }}>|</span>
            <button
              className={`sma-button ${barInterval === 'day' ? 'active' : ''}`}
              onClick={() => setBarInterval('day')}
              title="One bar per trading day"
            >
              Day
            </button>
            <button
              className={`sma-button ${barInterval === 'week' ? 'active' : ''}`}
              onClick={() => setBarInterval('week')}
              title="One bar per week: first open, highest high, lowest low, last close"
            >
              Week
            </button>
            <span style={{ margin: '0 4px', color: '#ccc' }}>|</span>
            {OVERLAYS.map((o) => (
              <button
                key={o.id}
                className={`sma-button ${activeOverlays.has(o.id) ? 'active' : ''}`}
                style={activeOverlays.has(o.id) ? { borderColor: o.color, color: o.color } : {}}
                onClick={() => toggleOverlay(o.id)}
                title={activeOverlays.has(o.id) ? `Hide ${o.label}` : `Show ${o.label}`}
              >
                {o.label}
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
          {chartData.overlayLines
            .filter((line) => activeOverlays.has(line.id))
            .map((line) => (
              <path
                key={line.id}
                d={buildPath(line.points)}
                fill="none"
                stroke={line.color}
                strokeWidth="2"
                strokeDasharray={line.dash}
                opacity="0.9"
              />
            ))}

          {/* Price series — candlesticks or a close line */}
          {showCandles ? (
            chartData.candles.map((candle, index) => (
              <g key={index}>
                <line
                  className="candle-wick"
                  x1={candle.centerX} y1={candle.highY}
                  x2={candle.centerX} y2={candle.lowY}
                  stroke={candle.color} strokeWidth="1"
                />
                <rect
                  className="candle-body"
                  x={candle.x} y={candle.bodyY}
                  width={candle.width} height={candle.bodyHeight}
                  fill={candle.color}
                />
              </g>
            ))
          ) : (
            <path className="close-line" d={buildPath(chartData.points)} fill="none" stroke="#2f5ce4" strokeWidth="2" />
          )}

          {/* Marker dots (breakthrough price / stop loss) */}
          {markerDots.map((dot, i) => (
            <circle key={i} cx={dot.x} cy={dot.y} r="6" fill={dot.color} stroke="#fff" strokeWidth="2" />
          ))}

          {/* Purchase price dot */}
          {purchaseDots.map((dot, i) => (
            <circle
              key={`${dot.date ?? 'axis'}-${i}`}
              cx={dot.x} cy={dot.y} r="6"
              fill={dot.onAxis ? PURCHASE_AXIS_COLOR : PURCHASE_COLOR}
              stroke="#fff" strokeWidth="2"
            >
              <title>{dot.onAxis ? 'Purchased before this range' : `Purchased ${dot.date}`} at {currSym}{dot.price.toFixed(2)}</title>
            </circle>
          ))}

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
              {hoverData.overlayValues.map(({ id, color, y }) =>
                y !== null ? (
                  <circle key={id} cx={hoverData.x} cy={y} r="4" fill={color} stroke="#fff" strokeWidth="1.5" />
                ) : null
              )}
              <rect x={tooltipX} y={chartData.top + 4} width={tooltipWidth} height={tooltipHeight} rx="6" fill="#1e2a3a" opacity="0.92" />
              <text x={tooltipX + 10} y={chartData.top + 20} fontSize="11" fill="#aac" fontFamily="inherit">{hoverData.date}</text>
              {hoverData.price !== null && (
                <text x={tooltipX + 10} y={chartData.top + 38} fontSize="13" fill="#fff" fontFamily="inherit" fontWeight="600">
                  Price: {currSym}{hoverData.price.toFixed(2)}
                </text>
              )}
              {hoverData.ohlc && (
                <>
                  <text x={tooltipX + 10} y={chartData.top + 56} fontSize="12" fill="#aac" fontFamily="inherit">
                    Open: {currSym}{hoverData.ohlc.open.toFixed(2)}
                  </text>
                  <text x={tooltipX + 10} y={chartData.top + 74} fontSize="12" fill="#aac" fontFamily="inherit">
                    High: {currSym}{hoverData.ohlc.high.toFixed(2)}
                  </text>
                  <text x={tooltipX + 10} y={chartData.top + 92} fontSize="12" fill="#aac" fontFamily="inherit">
                    Low: {currSym}{hoverData.ohlc.low.toFixed(2)}
                  </text>
                </>
              )}
              {hoverData.overlayValues.map(({ id, label, color, value }, i) =>
                value !== null ? (
                  <text key={id} x={tooltipX + 10} y={tooltipRowY(i)} fontSize="12" fill={color} fontFamily="inherit">
                    {label}: {currSym}{value.toFixed(2)}
                  </text>
                ) : null
              )}
              {markerDots.map((dot, i) => (
                <text key={`m${i}`} x={tooltipX + 10} y={tooltipRowY(activeOverlayValues.length + i)} fontSize="12" fill={dot.color} fontFamily="inherit">
                  {dot.label}: {currSym}{dot.price.toFixed(2)}
                </text>
              ))}
              {purchaseDot && (
                <text x={tooltipX + 10} y={tooltipRowY(activeOverlayValues.length + markerDots.length)} fontSize="12" fill={purchaseDot.onAxis ? PURCHASE_AXIS_COLOR : PURCHASE_COLOR} fontFamily="inherit">
                  Purchase: {currSym}{purchaseDot.price.toFixed(2)}
                </text>
              )}
            </g>
          )}
        </svg>
      </div>
      <div className="chart-legend">
        {showCandles ? (
          <>
            <span className="legend-item"><span className="legend-swatch" style={{ background: CANDLE_UP }} /> Close ≥ Open</span>
            <span className="legend-item"><span className="legend-swatch" style={{ background: CANDLE_DOWN }} /> Close &lt; Open</span>
          </>
        ) : (
          <span className="legend-item"><span className="legend-swatch price-line" /> Closing Price</span>
        )}
        {activeOverlayDefs.map((o) => (
          <span key={o.id} className="legend-item">
            <span className="legend-swatch" style={{ background: o.color, opacity: 0.9 }} />
            {o.period}-{barInterval} {o.kind.toUpperCase()}
          </span>
        ))}
        <span className="legend-item"><span className="legend-swatch" style={{ background: '#4caf50' }} /> Volume Up</span>
        <span className="legend-item"><span className="legend-swatch" style={{ background: '#f44336' }} /> Volume Down</span>
      </div>
    </div>
  )
}
