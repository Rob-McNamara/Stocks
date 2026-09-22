import { useCallback, useEffect, useId, useMemo, useRef, useState } from 'react'
import { apiClient, type ChartDrawing } from '../services/api'
import { calculateSMA, calculateEMA } from '../utils/sma'
import { toWeeklyBars } from '../utils/bars'
import { loadChartDefaults, FALLBACK_CHART_DEFAULTS, CHART_HEIGHT_RANGE, type ChartTimeframe } from '../utils/chartDefaults'

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
/** User-drawn support/resistance levels. */
/** Recessive grey: an annotation, deliberately outside the series hue space. */
export const DRAWING_COLOR = '#5f6368'
/** Volume panel geometry, in CSS pixels — fixed, so height changes go to price. */
/**
 * How far the price scale may be zoomed. Below 1 the data shrinks toward the
 * middle of the panel; above it the view magnifies. The ceiling stops a drag
 * running away into a range no tick label could distinguish.
 */
const Y_ZOOM_RANGE = { min: 0.25, max: 20 } as const

const VOLUME_PANEL_HEIGHT = 100
const VOLUME_PANEL_GAP = 40
/**
 * Tooltip text sits on a near-black panel, so it wears an ink colour and a
 * coloured swatch carries the series identity. Painting the text itself in the
 * series colour put brown (#795548) and purple (#9c27b0) on #1e2a3a, which is
 * close to unreadable — the darker a series reads on the white chart, the worse
 * it reads in the tooltip.
 */
const TOOLTIP_INK = '#e8ecf3'

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

/**
 * Overlay colours, validated rather than chosen by eye.
 *
 * Constrained by what the chart already paints: the price line (#2f5ce4), the
 * candles (#4caf50 up, #f44336 down) and the purchase markers own blue, green,
 * red and bright orange, so no overlay may use them. The previous palette
 * matched three of those *exactly* — SMA 150 was the up-candle green and SMA
 * 200 the down-candle red, so in candle mode those lines vanished into the
 * bars — and its brown fell below the chroma floor, reading as grey.
 *
 * Checked with the data-viz validator on a white surface: the full seven pass
 * the lightness band, chroma floor, adjacent-pair CVD separation and contrast;
 * the three in heaviest use (EMA 40, SMA 50, SMA 150) also pass as their own
 * set, since they are typically shown together.
 *
 * EMA 200's green is a compromise, and worth stating plainly. With thirteen
 * chromatic colours already on the chart the space is full: a sweep of 576
 * candidates found nothing separating cleanly from all of them, and every
 * best-scoring option was a blue that collided with the price line in *normal*
 * vision — the same way SMA 150 and SMA 200 once vanished into the candles.
 * This green protects against the always-painted marks instead (ΔE 21 from the
 * up-candle, 26 from SMA 200, which it tracks closely). What it costs is
 * separation from SMA 150 under protanopia, where the two read alike; the
 * differing dash and the legend label are what distinguish them there.
 *
 * A seventh line was the last one this palette could take. Another needs a
 * slot freed first — SMA 100 is the least-used candidate.
 */
export const OVERLAYS: readonly OverlayDef[] = [
  { id: 'sma20',  label: 'SMA 20',  period: 20,  kind: 'sma', color: '#827717', dash: '8 6' },
  { id: 'ema40',  label: 'EMA 40',  period: 40,  kind: 'ema', color: '#0097a7', dash: '3 4' },
  { id: 'sma50',  label: 'SMA 50',  period: 50,  kind: 'sma', color: '#7b1fa2', dash: '8 6' },
  { id: 'sma100', label: 'SMA 100', period: 100, kind: 'sma', color: '#c2185b', dash: '8 6' },
  { id: 'sma150', label: 'SMA 150', period: 150, kind: 'sma', color: '#a15c00', dash: '8 6' },
  { id: 'sma200', label: 'SMA 200', period: 200, kind: 'sma', color: '#3949ab', dash: '8 6' },
  { id: 'ema200', label: 'EMA 200', period: 200, kind: 'ema', color: '#33691e', dash: '3 4' },
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
  const [activeOverlays, setActiveOverlays] = useState<Set<string>>(new Set(FALLBACK_CHART_DEFAULTS.overlays))
  const [drawings, setDrawings] = useState<ChartDrawing[]>([])
  const [drawMode, setDrawMode] = useState<null | 'level' | 'trend'>(null)
  /** First anchor of a trendline, waiting for its second click. */
  const [pendingAnchor, setPendingAnchor] = useState<{ date: string; price: number } | null>(null)
  const [drawError, setDrawError] = useState<string | null>(null)
  const [timeframe, setTimeframe] = useState<ChartTimeframe>(FALLBACK_CHART_DEFAULTS.timeframe)
  const [chartType, setChartType] = useState<'line' | 'candle'>(FALLBACK_CHART_DEFAULTS.chartType)
  const [barInterval, setBarInterval] = useState<'day' | 'week'>(FALLBACK_CHART_DEFAULTS.barInterval)
  const [hoverIndex, setHoverIndex] = useState<number | null>(null)
  const [showInAud, setShowInAud] = useState(false)
  const [fxRate, setFxRate] = useState<number | null>(null)
  const [fxLoading, setFxLoading] = useState(false)
  // detectedCurrency is resolved from symbol info — more reliable than the prop when
  // the parent's symbolInfo cache hasn't been populated yet for this symbol.
  const [detectedCurrency, setDetectedCurrency] = useState<string>('AUD')
  /**
   * Callback ref rather than an effect: the frame is rendered only once the
   * history has loaded, so an effect with an empty dependency list runs while
   * the ref is still null and never observes anything.
   */
  const resizeObserver = useRef<ResizeObserver | null>(null)
  const frameRef = useCallback((node: HTMLDivElement | null) => {
    resizeObserver.current?.disconnect()
    resizeObserver.current = null
    if (!node || typeof ResizeObserver === 'undefined') return
    const observer = new ResizeObserver(([entry]) => {
      const { width, height } = entry.contentRect
      // A collapsed frame (hidden tab, mid-layout) would otherwise produce NaN
      // geometry from a zero-width plot.
      if (width > 0 && height > 0) {
        setFrame({ width: Math.round(width), height: Math.round(height) })
      }
    })
    observer.observe(node)
    resizeObserver.current = observer
  }, [])
  /**
   * Rendered size of the chart frame, in CSS pixels.
   *
   * The chart used to draw into a fixed 1100x400 viewBox and let the browser
   * scale it, so on a wide screen every label, tick and candle grew with the
   * container and the height was dictated by the width. Measuring instead means
   * the viewBox matches the rendered size 1:1 — SVG units become CSS pixels, so
   * an 11px label is 11px at any width, and height is free to be set
   * independently.
   *
   * The defaults match the old fixed viewBox, so a frame that has not been
   * measured yet (or a test without ResizeObserver) renders exactly as before.
   */
  const [frame, setFrame] = useState({ width: 1100, height: FALLBACK_CHART_DEFAULTS.height })
  /** Opening height of the frame; the user can drag it taller from there. */
  const [chartFrameHeight, setChartFrameHeight] = useState(FALLBACK_CHART_DEFAULTS.height)
  /** The configured opening height, so a double-click on the grip can restore it. */
  const [chartDefaultHeight, setChartDefaultHeight] = useState(FALLBACK_CHART_DEFAULTS.height)
  const svgRef = useRef<SVGSVGElement>(null)
  // Unique per chart: several can share a page (Holdings list, Analysis).
  const clipId = useId().replace(/:/g, '')

  /**
   * Vertical zoom on the price scale. 1 is auto-fit — the data span plus its
   * padding, exactly as before this existed. Above 1 the same data occupies
   * more of the panel, magnifying small moves; below 1 it is compressed.
   *
   * `yCenter` is the price the view is centred on, in display currency. Null
   * follows the data's own midpoint, so a zoom stays put as bars arrive rather
   * than drifting.
   */
  const [yZoom, setYZoom] = useState(1)
  const [yCenter, setYCenter] = useState<number | null>(null)
  const yZoomed = yZoom !== 1 || yCenter !== null

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

  /**
   * History is fetched once per symbol, but the live price keeps polling. When
   * a session's daily bar lands after the chart was drawn, the loaded history
   * ends a day short and the live price is appended close-only below — a day
   * that renders no candle and no OHLC in the tooltip. Reload the bars as soon
   * as the live price reports a trading date newer than the newest one held.
   *
   * One attempt per (symbol, trading date): the daily bar is stored before the
   * quote cache advances, so the bar is there by the time this runs, and the
   * guard keeps a backend that is still catching up from being polled.
   */
  const historyRefetchedFor = useRef<string | null>(null)
  useEffect(() => { historyRefetchedFor.current = null }, [symbol])
  useEffect(() => {
    if (!symbol || !currentPriceDate || history.length === 0) return
    const newest = history[history.length - 1].date
    if (currentPriceDate <= newest) return
    if (historyRefetchedFor.current === currentPriceDate) return
    historyRefetchedFor.current = currentPriceDate

    let cancelled = false
    apiClient.getPriceHistory(symbol, 600)
      .then((data) => {
        // Only adopt bars that actually advance the chart, so a backend still
        // serving the old tail cannot retrigger this effect in a loop.
        if (!cancelled && data.length > 0 && data[data.length - 1].date > newest) setHistory(data)
      })
      .catch(() => { /* keep the bars we have — the live close-only point still draws */ })
    return () => { cancelled = true }
  }, [symbol, currentPriceDate, history])

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

  /**
   * Apply the configured opening state, once.
   *
   * The config arrives after the first render, so this must not overwrite a
   * control the user has already touched — changing the timeframe and watching
   * it snap back a moment later would look like the chart ignoring the click.
   * Applying only on the first load also means it does not re-seed when the
   * symbol changes, so a chosen timeframe survives switching stocks.
   */
  const defaultsApplied = useRef(false)
  /**
   * Set the moment any toolbar control is used. The config resolves after the
   * first render, so without this a click made while the request is in flight
   * would be silently undone when it lands.
   */
  const userAdjusted = useRef(false)
  useEffect(() => {
    if (defaultsApplied.current) return
    let cancelled = false
    loadChartDefaults(OVERLAYS.map((o) => o.id)).then((d) => {
      if (cancelled || defaultsApplied.current || userAdjusted.current) return
      defaultsApplied.current = true
      setTimeframe(d.timeframe)
      setChartType(d.chartType)
      setBarInterval(d.barInterval)
      setActiveOverlays(new Set(d.overlays))
      setChartFrameHeight(d.height)
      setChartDefaultHeight(d.height)
    })
    return () => { cancelled = true }
  }, [])

  // Levels belong to the symbol, so they follow it onto whichever screen is
  // charting it. A failure just means no levels — never a broken chart.
  useEffect(() => {
    let cancelled = false
    setPendingAnchor(null)
    apiClient.getChartDrawings?.(symbol)
      .then((rows) => { if (!cancelled) setDrawings(rows) })
      .catch(() => { if (!cancelled) setDrawings([]) })
    return () => { cancelled = true }
  }, [symbol])


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
    // 60px of the frame is the right margin the stop-loss and level markers
    // sit in, outside the plot; the rest is the plot box.
    const width = Math.max(320, frame.width - 60)
    // The volume panel and its gap are fixed, so the price panel takes what is
    // left — a taller frame grows the price panel, not the volume bars.
    const height = Math.max(120, frame.height - VOLUME_PANEL_HEIGHT - VOLUME_PANEL_GAP)
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
    // Auto-fit is the span plus padding; zoom scales that span about a centre,
    // so at yZoom 1 with no centre the domain is exactly what it always was.
    const fitMin = rawMin - padding
    const fitMax = rawMax + padding
    const centre = yCenter ?? (fitMin + fitMax) / 2
    const halfSpan = ((fitMax - fitMin) || 2) / 2 / yZoom
    const minValue = centre - halfSpan
    const maxValue = centre + halfSpan
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
    const volumeHeight = VOLUME_PANEL_HEIGHT
    const volumeTop = height + VOLUME_PANEL_GAP
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
    // Two decimals is right for a whole-span view and useless zoomed in: five
    // ticks across a 3-cent window would all render as the same number. Extra
    // precision is added only once a cent stops separating adjacent ticks, so
    // an unzoomed chart reads exactly as it always has. Half a gap is the
    // threshold — at a full gap the two ends can still round together.
    const tickGap = priceRange / (yLabelCount - 1)
    const decimals = Math.min(6, Math.max(2, Math.ceil(-Math.log10(tickGap / 2))))
    const yLabels = Array.from({ length: yLabelCount }, (_, i) => {
      const value = minValue + (priceRange * i) / (yLabelCount - 1)
      return { y: toY(value), label: `${currSym}${value.toFixed(decimals)}` }
    })

    const axisY = top + plotHeight
    const labelY = axisY + 18
    // Denser axis labels on a wider chart. At the original plot width this
    // yields exactly the previous counts, so nothing shifts until there is
    // genuinely more room.
    const baseCount = trimmedHistory.length <= 7 ? trimmedHistory.length : trimmedHistory.length <= 30 ? 4 : 6
    const labelCount = trimmedHistory.length <= 7
      ? baseCount
      : Math.min(trimmedHistory.length, Math.max(baseCount, Math.round((baseCount * plotWidth) / 948)))
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
      width: frame.width, height: volumeTop + volumeHeight,
      points, overlayLines, candles, volumeBars, yLabels, xLabels,
      left, right, top, bottom, plotWidth, plotHeight,
      pricePlotHeight: plotHeight, axisY, labelY, toY, minValue, maxValue,
    }
  }, [frame, trimmedHistory, seriesHistory.length, allOverlays, fxMultiplier, currSym, markerPrice, markers, purchasePrice, purchases, isInternational, showInAud, fxRate, showCandles, yZoom, yCenter])

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

  /**
   * Price at a given y pixel — the inverse of `chartData.toY`, divided back
   * out of the display currency.
   *
   * The stored level must be native: the chart multiplies by `fxMultiplier` at
   * render, so saving the AUD figure while viewing AUD would draw the line in
   * the wrong place the moment the toggle flipped, and doubly wrong on a
   * native view.
   */
  /**
   * X pixel for a calendar date, extrapolating outside the visible window.
   *
   * X is bar-index based, so this interpolates between the two bars either
   * side of the date. Crucially it does *not* clamp: an anchor before the
   * first bar or after the last returns an x outside the plot, and the line is
   * clipped instead. Clamping would move the anchor and silently change the
   * line's slope — the one thing a trendline must never do.
   */
  const dateToX = (date: string): number => {
    const n = trimmedHistory.length
    if (n === 0) return chartData.left
    const xAt = (i: number) => chartData.left + (chartData.plotWidth * i) / Math.max(n - 1, 1)
    const first = trimmedHistory[0].date
    const last = trimmedHistory[n - 1].date
    const day = 86400000
    const spanDays = Math.max((Date.parse(last) - Date.parse(first)) / day, 1)
    // Pixels per calendar day, used only to extrapolate beyond the data.
    const pxPerDay = chartData.plotWidth / spanDays

    if (date <= first) return xAt(0) - ((Date.parse(first) - Date.parse(date)) / day) * pxPerDay
    if (date >= last) return xAt(n - 1) + ((Date.parse(date) - Date.parse(last)) / day) * pxPerDay

    const hi = trimmedHistory.findIndex((h) => h.date >= date)
    const lo = Math.max(hi - 1, 0)
    const t0 = Date.parse(trimmedHistory[lo].date)
    const t1 = Date.parse(trimmedHistory[hi].date)
    const frac = t1 === t0 ? 0 : (Date.parse(date) - t0) / (t1 - t0)
    return xAt(lo) + (xAt(hi) - xAt(lo)) * frac
  }

  /** Nearest bar's date to an x pixel — anchors snap to real trading days. */
  const dateAtX = (x: number): string | null => {
    const n = trimmedHistory.length
    if (n === 0) return null
    const i = Math.round(((x - chartData.left) / chartData.plotWidth) * (n - 1))
    return trimmedHistory[Math.max(0, Math.min(n - 1, i))].date
  }

  const nativePriceAtY = (y: number): number => {
    const { top, pricePlotHeight, minValue, maxValue } = chartData
    const displayPrice = minValue + ((top + pricePlotHeight - y) / pricePlotHeight) * (maxValue - minValue)
    return displayPrice / fxMultiplier
  }

  /**
   * A zoom belongs to the view it was made in — carried across a symbol or
   * timeframe change it looks like the chart has rendered wrongly.
   *
   * Adjusted during render rather than in an effect: React applies this before
   * committing, so the chart never paints one frame at the stale zoom the way
   * an effect-based reset would.
   */
  const zoomView = `${symbol}|${timeframe}|${barInterval}|${showInAud}`
  const [zoomViewSeen, setZoomViewSeen] = useState(zoomView)
  if (zoomViewSeen !== zoomView) {
    setZoomViewSeen(zoomView)
    setYZoom(1)
    setYCenter(null)
  }

  /**
   * Resizing the chart, driven from here rather than by CSS `resize`.
   *
   * The native handle is a scrollbar-layer feature in WebKit: on a box whose
   * content fits exactly, macOS overlay scrollbars materialise that layer only
   * while a resize is in flight, so the grip flashes and vanishes before it can
   * be grabbed. Owning the gesture also means `CHART_HEIGHT_RANGE` is actually
   * enforced — the native handle ignored it — and the height stays React state
   * instead of an inline style the next render must be careful not to undo.
   */
  const handleFrameResize = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault()
    const startY = e.clientY
    const startHeight = chartFrameHeight
    const target = e.currentTarget
    target.setPointerCapture(e.pointerId)

    const onMove = (move: PointerEvent) => {
      const next = startHeight + (move.clientY - startY)
      setChartFrameHeight(
        Math.min(CHART_HEIGHT_RANGE.max, Math.max(CHART_HEIGHT_RANGE.min, Math.round(next))),
      )
    }
    const onUp = (up: PointerEvent) => {
      target.releasePointerCapture(up.pointerId)
      target.removeEventListener('pointermove', onMove)
      target.removeEventListener('pointerup', onUp)
    }
    target.addEventListener('pointermove', onMove)
    target.addEventListener('pointerup', onUp)
  }

  const resetYZoom = () => {
    setYZoom(1)
    setYCenter(null)
  }

  /**
   * Drag the price gutter to zoom, the convention on trading charts. The gutter
   * is free for it: `handleChartClick` already ignores anything outside the
   * price band, since there is no price there to anchor a drawing to.
   *
   * Dragging up magnifies. The centre is pinned on the first drag so the view
   * expands about where it already sits rather than snapping to the data's
   * midpoint.
   */
  const handleAxisDrag = (e: React.MouseEvent<SVGRectElement>) => {
    e.preventDefault()
    e.stopPropagation()
    const startY = e.clientY
    const startZoom = yZoom
    const pinnedCentre = yCenter ?? (chartData.minValue + chartData.maxValue) / 2

    const onMove = (move: MouseEvent) => {
      // 200px of travel doubles or halves the scale — enough to be deliberate,
      // little enough to reach the extremes without letting go.
      const next = startZoom * Math.pow(2, (startY - move.clientY) / 200)
      setYCenter(pinnedCentre)
      setYZoom(Math.min(Y_ZOOM_RANGE.max, Math.max(Y_ZOOM_RANGE.min, next)))
    }
    const onUp = () => {
      window.removeEventListener('mousemove', onMove)
      window.removeEventListener('mouseup', onUp)
    }
    window.addEventListener('mousemove', onMove)
    window.addEventListener('mouseup', onUp)
  }

  const handleChartClick = async (e: React.MouseEvent<SVGSVGElement>) => {
    if (!drawMode) return
    const svg = svgRef.current
    if (!svg) return
    const rect = svg.getBoundingClientRect()
    const y = ((e.clientY - rect.top) / rect.height) * chartData.height
    const x = ((e.clientX - rect.left) / rect.width) * chartData.width
    // Ignore a click in the axis gutter or over the volume panel — there is no
    // price there, and placing an anchor from it would be a guess.
    if (y < chartData.top || y > chartData.top + chartData.pricePlotHeight) return

    const price = nativePriceAtY(y)
    if (!isFinite(price) || price <= 0) return
    setDrawError(null)

    if (drawMode === 'level') {
      try {
        setDrawings(await apiClient.addChartDrawing(symbol, Number(price.toFixed(4))))
      } catch (err) {
        setDrawError(err instanceof Error ? err.message : 'Failed to save the price level')
      }
      return
    }

    // Trendline: first click sets an anchor, second completes the line.
    const date = dateAtX(x)
    if (!date) return
    if (!pendingAnchor) {
      setPendingAnchor({ date, price })
      return
    }
    if (date === pendingAnchor.date) {
      setDrawError('Pick a second point on a different date — a trendline needs two.')
      return
    }
    // Oldest anchor first, so the line always reads left to right regardless
    // of which end was clicked first.
    const [a, b] = date < pendingAnchor.date
      ? [{ date, price }, pendingAnchor]
      : [pendingAnchor, { date, price }]
    try {
      setDrawings(await apiClient.addTrendline(symbol, {
        startDate: a.date,
        startPrice: Number(a.price.toFixed(4)),
        endDate: b.date,
        endPrice: Number(b.price.toFixed(4)),
      }))
      setPendingAnchor(null)
    } catch (err) {
      setDrawError(err instanceof Error ? err.message : 'Failed to save the trendline')
    }
  }

  const removeDrawing = async (id: number) => {
    setDrawError(null)
    try {
      await apiClient.deleteChartDrawing(id)
      setDrawings((rows) => rows.filter((r) => r.id !== id))
    } catch (err) {
      setDrawError(err instanceof Error ? err.message : 'Failed to remove the price level')
    }
  }

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

  const tooltipWidth = 186
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
              <button key={tf} className={`sma-button ${timeframe === tf ? 'active' : ''}`} onClick={() => { userAdjusted.current = true; setTimeframe(tf) }}>
                {tf === '1w' ? '1W' : tf === '1m' ? '1M' : tf === '3m' ? '3M' : tf === '6m' ? '6M' : tf === '12m' ? '12M' : '2Y'}
              </button>
            ))}
            <span style={{ margin: '0 4px', color: '#ccc' }}>|</span>
            <button
              className={`sma-button ${drawMode === 'level' ? 'active' : ''}`}
              onClick={() => { setDrawMode((v) => (v === 'level' ? null : 'level')); setPendingAnchor(null); setDrawError(null) }}
              title="Draw a horizontal price level"
            >
              ⊹ Level
            </button>
            <button
              className={`sma-button ${drawMode === 'trend' ? 'active' : ''}`}
              onClick={() => { setDrawMode((v) => (v === 'trend' ? null : 'trend')); setPendingAnchor(null); setDrawError(null) }}
              title="Draw a trendline between two points"
            >
              ╱ Trend
            </button>
            <span style={{ margin: '0 4px', color: '#ccc' }}>|</span>
            <button
              className={`sma-button ${chartType === 'line' ? 'active' : ''}`}
              onClick={() => { userAdjusted.current = true; setChartType('line') }}
              title="Show the closing price as a line"
            >
              Line
            </button>
            <button
              className={`sma-button ${showCandles ? 'active' : ''}`}
              onClick={() => { userAdjusted.current = true; setChartType('candle') }}
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
              onClick={() => { userAdjusted.current = true; setBarInterval('day') }}
              title="One bar per trading day"
            >
              Day
            </button>
            <button
              className={`sma-button ${barInterval === 'week' ? 'active' : ''}`}
              onClick={() => { userAdjusted.current = true; setBarInterval('week') }}
              title="One bar per week: first open, highest high, lowest low, last close"
            >
              Week
            </button>
            {/* Only while zoomed. Dragging the gutter is the gesture, but
                nothing advertises it, so the way back has to be visible. */}
            {yZoomed && (
              <button
                className="sma-button"
                onClick={resetYZoom}
                title="Return the price scale to fitting the data"
              >
                ⤢ Fit
              </button>
            )}
            <span style={{ margin: '0 4px', color: '#ccc' }}>|</span>
            {OVERLAYS.map((o) => (
              <button
                key={o.id}
                className={`sma-button ${activeOverlays.has(o.id) ? 'active' : ''}`}
                style={activeOverlays.has(o.id) ? { borderColor: o.color, color: o.color } : {}}
                onClick={() => { userAdjusted.current = true; toggleOverlay(o.id) }}
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
        {drawMode === 'level' && (
          <div style={{ fontSize: 12, color: '#546e7a', marginBottom: 4 }}>
            Click anywhere on the price panel to place a level. Click × on a level to remove it.
          </div>
        )}
        {drawMode === 'trend' && (
          <div style={{ fontSize: 12, color: '#546e7a', marginBottom: 4 }}>
            {pendingAnchor
              ? `Anchored at ${pendingAnchor.date} — click a second point to finish the line.`
              : 'Click the first point of the trendline. Anchors snap to trading days.'}
          </div>
        )}
        {drawError && <div className="alert alert-error" style={{ marginBottom: 4 }}>{drawError}</div>}
        {/* Height is a custom property so the box can be sized from React state
            without an inline `height` competing with it. */}
        <div
          ref={frameRef}
          className="chart-resize-box"
          style={{ '--chart-height': `${chartFrameHeight}px` } as React.CSSProperties}
        >
        <svg
          ref={svgRef}
          width={chartData.width}
          height={chartData.height}
          viewBox={`0 0 ${chartData.width} ${chartData.height}`}
          className="chart-svg"
          onMouseMove={handleMouseMove}
          onMouseLeave={() => setHoverIndex(null)}
          onClick={handleChartClick}
          style={{ cursor: 'crosshair' }}
        >
          <defs>
            {/* Trendlines project past their anchors, so they must be kept
                inside the price panel rather than painting over the axis
                labels and the volume bars below. */}
            <clipPath id={clipId}>
              <rect
                x={chartData.left} y={chartData.top}
                width={chartData.plotWidth} height={chartData.pricePlotHeight}
              />
            </clipPath>
          </defs>

          <rect x="0" y="0" width={chartData.width} height={chartData.height} fill="#ffffff" rx="18" />

          {chartData.yLabels.map(({ y, label }) => (
            <g key={label}>
              <line x1={chartData.left} y1={y} x2={chartData.width - chartData.right} y2={y} stroke="#e8edf5" strokeWidth="1" strokeDasharray="4 3" />
              <text x={chartData.left - 6} y={y + 4} textAnchor="end" fontSize="11" fill="#999" fontFamily="inherit">{label}</text>
            </g>
          ))}

          {/* The price gutter, as a grab target. Transparent rather than
              styled: it sits under the tick labels, which stay readable. */}
          <rect
            x={0}
            y={chartData.top}
            width={chartData.left}
            height={chartData.pricePlotHeight}
            fill="transparent"
            style={{ cursor: 'ns-resize' }}
            onMouseDown={handleAxisDrag}
            onDoubleClick={resetYZoom}
          >
            <title>Drag to zoom the price scale, double-click to fit</title>
          </rect>

          <line x1={chartData.left} y1={chartData.top} x2={chartData.left} y2={chartData.top + chartData.pricePlotHeight} stroke="#e1e7f1" strokeWidth="1" />
          <line x1={chartData.left} y1={chartData.top + chartData.pricePlotHeight} x2={chartData.width - chartData.right} y2={chartData.top + chartData.pricePlotHeight} stroke="#e1e7f1" strokeWidth="1" />

          {chartData.xLabels.map(({ x, label }) => (
            <g key={label}>
              <line x1={x} y1={chartData.axisY} x2={x} y2={chartData.axisY + 5} stroke="#aaa" strokeWidth="1" />
              <text x={x} y={chartData.labelY} textAnchor="middle" fontSize="13" fill="#888" fontFamily="inherit">{label}</text>
            </g>
          ))}

          {/* Clipped because the y-range is no longer guaranteed to contain the
              data: zoomed in, the line, the overlays and the candle wicks all
              run past the panel and would paint over the axis labels and the
              volume bars below. */}
          <g clipPath={`url(#${clipId})`}>
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
          </g>

          {/* User-drawn price levels. Anchored by price, so they survive every
              timeframe, interval and currency change; a level outside the
              current y-range is simply not drawn rather than expanding the
              scale and flattening the price series. */}
          {drawings.map((d) => {
            const colour = d.colour ?? DRAWING_COLOR

            if (d.kind === 'trend' && d.start_date && d.end_date && d.end_price != null) {
              const x1 = dateToX(d.start_date)
              const y1 = chartData.toY(d.price * fxMultiplier)
              const x2 = dateToX(d.end_date)
              const y2 = chartData.toY(d.end_price * fxMultiplier)
              if (x2 === x1) return null
              // Projected past the second anchor to the right edge — that is
              // the point of a trendline. The clip path keeps it inside the
              // price panel instead of painting over the axis and volume bars.
              const right = chartData.left + chartData.plotWidth
              const slope = (y2 - y1) / (x2 - x1)
              const yAtRight = y2 + slope * (right - x2)
              return (
                <g key={d.id} clipPath={`url(#${clipId})`}>
                  <line x1={x1} y1={y1} x2={x2} y2={y2} stroke={colour} strokeWidth="1.5" />
                  <line
                    x1={x2} y1={y2} x2={right} y2={yAtRight}
                    stroke={colour} strokeWidth="1.5" strokeDasharray="4 4" opacity="0.7"
                  />
                  <circle cx={x1} cy={y1} r="4" fill={colour} stroke="#fff" strokeWidth="1.5" />
                  <g style={{ cursor: 'pointer' }} onClick={(e) => { e.stopPropagation(); void removeDrawing(d.id) }}>
                    <title>Remove this trendline</title>
                    <circle cx={x2} cy={y2} r="6" fill="#fff" stroke={colour} strokeWidth="1.5" />
                    <text x={x2} y={y2 + 4} fontSize="10" fill={colour} textAnchor="middle" fontFamily="inherit">×</text>
                  </g>
                </g>
              )
            }

            const y = chartData.toY(d.price * fxMultiplier)
            if (y < chartData.top || y > chartData.top + chartData.pricePlotHeight) return null
            return (
              <g key={d.id}>
                <line
                  x1={chartData.left} y1={y}
                  x2={chartData.left + chartData.plotWidth} y2={y}
                  stroke={colour} strokeWidth="1.5" strokeDasharray="6 4"
                />
                <text x={chartData.left + 4} y={y - 4} fontSize="11" fill={colour} fontFamily="inherit">
                  {d.label ? `${d.label} ` : ''}{currSym}{(d.price * fxMultiplier).toFixed(2)}
                </text>
                {/* Past the plot edge, so it never covers a bar. */}
                <g style={{ cursor: 'pointer' }} onClick={(e) => { e.stopPropagation(); void removeDrawing(d.id) }}>
                  <title>Remove this level</title>
                  <circle cx={chartData.left + chartData.plotWidth + 9} cy={y} r="7" fill="#fff" stroke={colour} strokeWidth="1.5" />
                  <text x={chartData.left + chartData.plotWidth + 9} y={y + 4} fontSize="11" fill={colour} textAnchor="middle" fontFamily="inherit">×</text>
                </g>
              </g>
            )
          })}

          {pendingAnchor && (
            <circle
              cx={dateToX(pendingAnchor.date)}
              cy={chartData.toY(pendingAnchor.price * fxMultiplier)}
              r="5" fill="none" stroke={DRAWING_COLOR} strokeWidth="2" strokeDasharray="3 2"
            />
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
                  <g key={id}>
                    <rect x={tooltipX + 10} y={tooltipRowY(i) - 8} width={9} height={9} rx="2" fill={color} />
                    <text x={tooltipX + 24} y={tooltipRowY(i)} fontSize="12" fill={TOOLTIP_INK} fontFamily="inherit">
                      {label}: {currSym}{value.toFixed(2)}
                    </text>
                  </g>
                ) : null
              )}
              {markerDots.map((dot, i) => (
                <g key={`m${i}`}>
                  <rect x={tooltipX + 10} y={tooltipRowY(activeOverlayValues.length + i) - 8} width={9} height={9} rx="2" fill={dot.color} />
                  <text x={tooltipX + 24} y={tooltipRowY(activeOverlayValues.length + i)} fontSize="12" fill={TOOLTIP_INK} fontFamily="inherit">
                    {dot.label}: {currSym}{dot.price.toFixed(2)}
                  </text>
                </g>
              ))}
              {purchaseDot && (
                <g>
                  <rect
                    x={tooltipX + 10} y={tooltipRowY(activeOverlayValues.length + markerDots.length) - 8}
                    width={9} height={9} rx="2"
                    fill={purchaseDot.onAxis ? PURCHASE_AXIS_COLOR : PURCHASE_COLOR}
                  />
                  <text x={tooltipX + 24} y={tooltipRowY(activeOverlayValues.length + markerDots.length)} fontSize="12" fill={TOOLTIP_INK} fontFamily="inherit">
                    Purchase: {currSym}{purchaseDot.price.toFixed(2)}
                  </text>
                </g>
              )}
            </g>
          )}
        </svg>
        </div>
        {/* Always visible, unlike the native grip it replaces. */}
        <div
          className="chart-resize-grip"
          onPointerDown={handleFrameResize}
          onDoubleClick={() => setChartFrameHeight(chartDefaultHeight)}
          role="separator"
          aria-orientation="horizontal"
          aria-label="Drag to resize the chart, double-click to reset"
          title="Drag to resize the chart, double-click to reset"
        />
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
