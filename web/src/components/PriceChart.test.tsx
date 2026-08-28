// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { render, screen, waitFor, cleanup, fireEvent } from '@testing-library/react'
import PriceChart, { DRAWING_COLOR } from './PriceChart'
import { invalidateChartDefaults } from '../utils/chartDefaults'

vi.mock('../services/api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../services/api')>()
  return {
    ...actual,
    apiClient: {
      getPriceHistory: vi.fn(),
      getConfig: vi.fn(),
      getChartDrawings: vi.fn(),
      addChartDrawing: vi.fn(),
      addTrendline: vi.fn(),
      deleteChartDrawing: vi.fn(),
      getSymbolInfo: vi.fn(),
      getFxRateForDate: vi.fn(),
    },
  }
})

import { apiClient } from '../services/api'
const getPriceHistory = apiClient.getPriceHistory as ReturnType<typeof vi.fn>
const getSymbolInfo = apiClient.getSymbolInfo as ReturnType<typeof vi.fn>
const getChartDrawings = apiClient.getChartDrawings as ReturnType<typeof vi.fn>
const addChartDrawing = apiClient.addChartDrawing as ReturnType<typeof vi.fn>
const addTrendline = apiClient.addTrendline as ReturnType<typeof vi.fn>
const deleteChartDrawing = apiClient.deleteChartDrawing as ReturnType<typeof vi.fn>

// Chart geometry constants from PriceChart's chartData
const LEFT = 72
const PLOT_WIDTH = 1040 - 72 - 20

function isoDaysAgo(n: number): string {
  const d = new Date()
  d.setDate(d.getDate() - n)
  return d.toISOString().slice(0, 10)
}

/**
 * 40 recent daily bars, oldest first, closes around $10–12. Deliberately
 * close-only: this is the shape of every bar stored before OHLC ingest existed,
 * so it also exercises the chart's fallback when candles aren't available.
 */
const DATES = Array.from({ length: 40 }, (_, i) => isoDaysAgo(39 - i))
const HISTORY = DATES.map((date, i) => ({
  date, open: null, high: null, low: null, close: 10 + (i % 5) * 0.5, volume: 1000 + i,
}))

/** The same bars with full OHLC, as returned once the backfill has run. */
const OHLC_HISTORY = DATES.map((date, i) => {
  const close = 10 + (i % 5) * 0.5
  const open = close - 0.2
  return { date, open, high: close + 0.4, low: open - 0.3, close, volume: 1000 + i }
})

async function renderChart(props: Partial<Parameters<typeof PriceChart>[0]> = {}) {
  const result = render(<PriceChart symbol="TST.AX" currency="AUD" onLoading={() => {}} {...props} />)
  await waitFor(() => expect(screen.queryByText(/Loading chart/)).toBeNull())
  return result
}

beforeEach(() => {
  cleanup()
  getPriceHistory.mockReset().mockResolvedValue(HISTORY)
  getSymbolInfo.mockReset().mockResolvedValue([])
  // No test needed a foreign symbol before, so this mock had no default and
  // returned undefined the moment one did.
  ;(apiClient.getFxRateForDate as ReturnType<typeof vi.fn>).mockReset().mockResolvedValue(null)
  // The defaults fetch is cached module-wide, so it must be dropped between
  // tests or the first result would be reused by every later one.
  invalidateChartDefaults()
  ;(apiClient.getConfig as ReturnType<typeof vi.fn>).mockReset().mockResolvedValue({})
  getChartDrawings.mockReset().mockResolvedValue([])
  addChartDrawing.mockReset().mockResolvedValue([])
  addTrendline.mockReset().mockResolvedValue([])
  deleteChartDrawing.mockReset().mockResolvedValue(undefined)
})

afterEach(() => {
  vi.restoreAllMocks()
})

describe('SMA defaults and toggles', () => {
  it('starts with SMA50 and SMA150 active', async () => {
    await renderChart()
    const active = [20, 50, 100, 150, 200].filter(
      (p) => screen.getByRole('button', { name: `SMA ${p}` }).className.includes('active'),
    )
    expect(active).toEqual([50, 150])
    // Legend mirrors the active set
    expect(screen.getByText('50-day SMA')).toBeTruthy()
    expect(screen.getByText('150-day SMA')).toBeTruthy()
    expect(screen.queryByText('20-day SMA')).toBeNull()
  })

  it('toggles periods on and off via the buttons', async () => {
    await renderChart()
    fireEvent.click(screen.getByRole('button', { name: 'SMA 150' }))
    expect(screen.getByRole('button', { name: 'SMA 150' }).className).not.toContain('active')
    expect(screen.queryByText('150-day SMA')).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: 'SMA 20' }))
    expect(screen.getByText('20-day SMA')).toBeTruthy()
  })
})

describe('EMA 40 overlay', () => {
  it('offers an EMA 40 button, off by default, ordered by period', async () => {
    await renderChart()
    const button = screen.getByRole('button', { name: 'EMA 40' })
    expect(button.className).not.toContain('active')
    expect(screen.queryByText('40-day EMA')).toBeNull()

    // Buttons read left to right by period: 20, 40, 50, 100, 150, 200
    const labels = [...document.querySelectorAll('.sma-button')]
      .map((b) => b.textContent!.trim())
      .filter((t) => /^(SMA|EMA) /.test(t))
    expect(labels).toEqual(['SMA 20', 'EMA 40', 'SMA 50', 'SMA 100', 'SMA 150', 'SMA 200'])
  })

  it('draws the EMA line and legend entry when enabled', async () => {
    const { container } = await renderChart()
    const dashed = () => [...container.querySelectorAll('path[stroke-dasharray]')]
    const before = dashed().length

    fireEvent.click(screen.getByRole('button', { name: 'EMA 40' }))

    expect(screen.getByRole('button', { name: 'EMA 40' }).className).toContain('active')
    expect(screen.getByText('40-day EMA')).toBeTruthy()
    expect(dashed().length).toBe(before + 1)
    // Its own dash pattern distinguishes it from the simple averages
    expect(dashed().some((p) => p.getAttribute('stroke-dasharray') === '3 4')).toBe(true)
  })

  it('tracks the price more closely than the same-length simple average', async () => {
    // 60 flat bars then a sustained step up: the EMA must sit above an SMA of
    // comparable length, which is the whole reason to offer it.
    const dates = Array.from({ length: 80 }, (_, i) => isoDaysAgo(79 - i))
    const stepped = dates.map((date, i) => ({
      date, open: null, high: null, low: null,
      close: i < 60 ? 10 : 20,
      volume: 1000,
    }))
    getPriceHistory.mockResolvedValue(stepped)
    const { container } = await renderChart()

    const yOf = (dash: string) => {
      const path = [...container.querySelectorAll('path[stroke-dasharray]')]
        .find((p) => p.getAttribute('stroke-dasharray') === dash)!
      const last = path.getAttribute('d')!.trim().split(/[ML]\s*/).filter(Boolean).pop()!
      return parseFloat(last.trim().split(/\s+/)[1])
    }

    fireEvent.click(screen.getByRole('button', { name: 'EMA 40' }))
    // Lower y is higher on the chart
    expect(yOf('3 4')).toBeLessThan(yOf('8 6'))
  })
})

describe('day / week interval', () => {
  /**
   * Eight trading days spanning exactly two calendar weeks: Mon 3 – Fri 7 Aug,
   * then Mon 10 – Wed 12 Aug. The aggregation maths itself is covered by the
   * toWeeklyBars unit tests; these check the chart is actually wired to it.
   */
  const TWO_WEEKS = [
    { date: '2026-08-03', open: 10, high: 12, low: 9, close: 11, volume: 100 },
    { date: '2026-08-04', open: 11, high: 15, low: 8, close: 14, volume: 100 },
    { date: '2026-08-05', open: 14, high: 13, low: 12, close: 13, volume: 100 },
    { date: '2026-08-06', open: 13, high: 14, low: 11, close: 12, volume: 100 },
    { date: '2026-08-07', open: 12, high: 16, low: 10, close: 15, volume: 100 },
    { date: '2026-08-10', open: 20, high: 22, low: 19, close: 21, volume: 100 },
    { date: '2026-08-11', open: 21, high: 25, low: 18, close: 24, volume: 100 },
    { date: '2026-08-12', open: 24, high: 23, low: 22, close: 23, volume: 100 },
  ]

  it('defaults to Day', async () => {
    await renderChart()
    expect(screen.getByRole('button', { name: 'Day' }).className).toContain('active')
    expect(screen.getByRole('button', { name: 'Week' }).className).not.toContain('active')
  })

  it('collapses the daily bars into weekly ones', async () => {
    getPriceHistory.mockResolvedValue(TWO_WEEKS)
    const { container } = await renderChart()
    fireEvent.click(screen.getByRole('button', { name: 'Candles' }))
    expect(container.querySelectorAll('rect.candle-body')).toHaveLength(8)

    fireEvent.click(screen.getByRole('button', { name: 'Week' }))
    expect(container.querySelectorAll('rect.candle-body')).toHaveLength(2)

    // And back again
    fireEvent.click(screen.getByRole('button', { name: 'Day' }))
    expect(container.querySelectorAll('rect.candle-body')).toHaveLength(8)
  })

  it('aggregates the line and volume too, not just the candles', async () => {
    getPriceHistory.mockResolvedValue(TWO_WEEKS)
    const { container } = await renderChart()
    expect(container.querySelectorAll('rect').length).toBeGreaterThan(0)
    const dailyVolumeBars = container.querySelectorAll('rect').length

    fireEvent.click(screen.getByRole('button', { name: 'Week' }))
    expect(container.querySelectorAll('rect').length).toBeLessThan(dailyVolumeBars)
    // The close line is still drawn, over the weekly closes
    expect(container.querySelector('path.close-line')).toBeTruthy()
  })

  /**
   * The averages are computed on the plotted bars, so their period counts
   * weeks in week mode. The legend has to say so or "50" is misleading.
   */
  it('labels the moving averages in the active interval', async () => {
    await renderChart()
    expect(screen.getByText('50-day SMA')).toBeTruthy()

    fireEvent.click(screen.getByRole('button', { name: 'Week' }))
    expect(screen.getByText('50-week SMA')).toBeTruthy()
    expect(screen.queryByText('50-day SMA')).toBeNull()
  })
})

describe('candle / line toggle', () => {
  it('defaults to the close line and disables candles when no OHLC is stored', async () => {
    const { container } = await renderChart()
    expect(container.querySelector('path.close-line')).toBeTruthy()
    expect(container.querySelectorAll('rect.candle-body').length).toBe(0)

    const candleButton = screen.getByRole('button', { name: 'Candles' }) as HTMLButtonElement
    expect(candleButton.disabled).toBe(true)
    expect(candleButton.title).toMatch(/backfill/i)
    expect(screen.getByRole('button', { name: 'Line' }).className).toContain('active')
  })

  it('renders candles instead of the line once OHLC is present', async () => {
    getPriceHistory.mockResolvedValue(OHLC_HISTORY)
    const { container } = await renderChart()

    const candleButton = screen.getByRole('button', { name: 'Candles' }) as HTMLButtonElement
    expect(candleButton.disabled).toBe(false)

    fireEvent.click(candleButton)
    expect(container.querySelectorAll('rect.candle-body').length).toBe(OHLC_HISTORY.length)
    expect(container.querySelectorAll('line.candle-wick').length).toBe(OHLC_HISTORY.length)
    expect(container.querySelector('path.close-line')).toBeNull()
    expect(screen.getByText('Close ≥ Open')).toBeTruthy()

    // Switching back restores the line
    fireEvent.click(screen.getByRole('button', { name: 'Line' }))
    expect(container.querySelector('path.close-line')).toBeTruthy()
    expect(container.querySelectorAll('rect.candle-body').length).toBe(0)
  })

  it('colours each candle against its own open, not the previous close', async () => {
    // Bar 0 closes above its open (green), bar 1 closes below its own open (red)
    // while still closing *above* bar 0 — the two rules disagree here.
    getPriceHistory.mockResolvedValue([
      { date: DATES[0], open: 10, high: 11, low: 9.8, close: 10.5, volume: 100 },
      { date: DATES[1], open: 12, high: 12.2, low: 10.6, close: 11.0, volume: 100 },
    ])
    const { container } = await renderChart()
    fireEvent.click(screen.getByRole('button', { name: 'Candles' }))

    const bodies = container.querySelectorAll('rect.candle-body')
    expect(bodies.length).toBe(2)
    expect(bodies[0].getAttribute('fill')).toBe('#4caf50')
    expect(bodies[1].getAttribute('fill')).toBe('#f44336')
  })

  it('scales the y-axis to the wicks so highs and lows stay inside the plot', async () => {
    // A single spiking bar: its high is far above every close in the series
    const spiking = [
      ...OHLC_HISTORY.slice(0, 39),
      { date: DATES[39], open: 10, high: 30, low: 2, close: 11, volume: 100 },
    ]
    getPriceHistory.mockResolvedValue(spiking)
    const { container } = await renderChart()
    fireEvent.click(screen.getByRole('button', { name: 'Candles' }))

    const wicks = container.querySelectorAll('line.candle-wick')
    const last = wicks[wicks.length - 1]
    const highY = parseFloat(last.getAttribute('y1')!)
    const lowY = parseFloat(last.getAttribute('y2')!)
    // Chart plot area runs from y=20 (top) to y=240 (top + plotHeight)
    expect(highY).toBeGreaterThanOrEqual(20)
    expect(lowY).toBeLessThanOrEqual(240)
    expect(highY).toBeLessThan(lowY)
  })

  it('gives a doji a visible body', async () => {
    getPriceHistory.mockResolvedValue([
      { date: DATES[0], open: 10, high: 10.5, low: 9.5, close: 10, volume: 100 },
    ])
    const { container } = await renderChart()
    fireEvent.click(screen.getByRole('button', { name: 'Candles' }))
    const body = container.querySelector('rect.candle-body')!
    expect(parseFloat(body.getAttribute('height')!)).toBeGreaterThanOrEqual(1)
  })

  it('shows open, high and low in the tooltip in candle mode only', async () => {
    vi.spyOn(Element.prototype, 'getBoundingClientRect').mockReturnValue({
      x: 0, y: 0, left: 0, top: 0, right: 1040, bottom: 260, width: 1040, height: 260,
      toJSON: () => ({}),
    } as DOMRect)

    getPriceHistory.mockResolvedValue(OHLC_HISTORY)
    const { container } = await renderChart()

    fireEvent.mouseMove(container.querySelector('svg')!, { clientX: 500, clientY: 100 })
    await waitFor(() => expect(screen.getByText(/^Price:/)).toBeTruthy())
    expect(screen.queryByText(/^Open:/)).toBeNull()

    fireEvent.click(screen.getByRole('button', { name: 'Candles' }))
    fireEvent.mouseMove(container.querySelector('svg')!, { clientX: 500, clientY: 100 })
    await waitFor(() => expect(screen.getByText(/^Open:/)).toBeTruthy())
    expect(screen.getByText(/^High:/)).toBeTruthy()
    expect(screen.getByText(/^Low:/)).toBeTruthy()
  })
})

describe('purchase dot placement', () => {
  it('lands on the first bar at or after the purchase date', async () => {
    const purchaseIdx = 10
    const { container } = await renderChart({
      purchasePrice: 10,
      purchaseDate: DATES[purchaseIdx],
    })
    const dot = container.querySelector('circle[fill="#1565c0"]')!
    expect(dot).toBeTruthy()
    const expectedX = LEFT + (PLOT_WIDTH * purchaseIdx) / (DATES.length - 1)
    expect(Math.abs(parseFloat(dot.getAttribute('cx')!) - expectedX)).toBeLessThan(0.5)
  })

  it('renders on the axis in orange when the purchase predates the chart range', async () => {
    const { container } = await renderChart({
      purchasePrice: 10,
      purchaseDate: '2020-01-02',
    })
    expect(container.querySelector('circle[fill="#1565c0"]')).toBeNull()
    const axisDot = container.querySelector('circle[fill="#ff9800"]')!
    expect(axisDot).toBeTruthy()
    expect(parseFloat(axisDot.getAttribute('cx')!)).toBe(LEFT)
  })

  // A position built in several parcels was previously shown as one dot at the
  // averaged cost, a price that was never actually paid on any single day.
  it('marks every purchase when the individual lots are supplied', async () => {
    const first = 5
    const second = 20
    const { container } = await renderChart({
      purchasePrice: 11,
      purchaseDate: DATES[first],
      purchases: [
        { date: DATES[first], price: 10 },
        { date: DATES[second], price: 13 },
      ],
    })
    const dots = [...container.querySelectorAll('circle[fill="#1565c0"]')]
    expect(dots).toHaveLength(2)
    const xs = dots.map((d) => parseFloat(d.getAttribute('cx')!)).sort((a, b) => a - b)
    for (const [i, idx] of [first, second].entries()) {
      const expected = LEFT + (PLOT_WIDTH * idx) / (DATES.length - 1)
      expect(Math.abs(xs[i] - expected)).toBeLessThan(0.5)
    }
  })

  // Several older buys would stack on the same axis point, so they collapse to
  // one marker rather than drawing identical dots on top of each other.
  it('collapses purchases older than the range into a single axis marker', async () => {
    const visible = 12
    const { container } = await renderChart({
      purchasePrice: 11,
      purchaseDate: '2020-01-02',
      purchases: [
        { date: '2020-01-02', price: 8 },
        { date: '2020-06-02', price: 12 },
        { date: DATES[visible], price: 15 },
      ],
    })
    expect(container.querySelectorAll('circle[fill="#ff9800"]')).toHaveLength(1)
    const inRange = [...container.querySelectorAll('circle[fill="#1565c0"]')]
    expect(inRange).toHaveLength(1)
    const expected = LEFT + (PLOT_WIDTH * visible) / (DATES.length - 1)
    expect(Math.abs(parseFloat(inRange[0].getAttribute('cx')!) - expected)).toBeLessThan(0.5)
  })
})

describe('stop-loss marker', () => {
  it('renders the marker dot when a stop loss is set and omits it otherwise', async () => {
    const { container } = await renderChart({
      markerPrice: 9.0,
      markerLabel: 'Stop Loss',
      markerMode: 'stoploss',
    })
    expect(container.querySelector('circle[fill="#e91e63"]')).toBeTruthy()

    cleanup()
    const { container: bare } = await renderChart()
    expect(bare.querySelector('circle[fill="#e91e63"]')).toBeNull()
  })

  it('shows the trailing-sell label in the hover tooltip', async () => {
    // jsdom reports a zero-size bounding box; give the SVG its real size so
    // mouse coordinates map to an index
    vi.spyOn(Element.prototype, 'getBoundingClientRect').mockReturnValue({
      x: 0, y: 0, left: 0, top: 0, right: 1040, bottom: 260, width: 1040, height: 260,
      toJSON: () => ({}),
    } as DOMRect)

    const { container } = await renderChart({
      markerPrice: 9.0,
      markerLabel: 'Trailing Sell',
      markerMode: 'stoploss',
    })
    fireEvent.mouseMove(container.querySelector('svg')!, { clientX: 500, clientY: 100 })
    await waitFor(() => expect(screen.getByText(/Trailing Sell: \$9\.00/)).toBeTruthy())
  })
})

describe('drawn price levels', () => {
  const level = (over: Record<string, unknown> = {}) => ({
    id: 1, symbol: 'TST.AX', kind: 'horizontal' as const,
    price: 10, label: null, colour: null,
    start_date: null, end_date: null, end_price: null, created_at: 'x', ...over,
  })

  it('draws a level as a full-width line labelled with its price', async () => {
    getChartDrawings.mockResolvedValue([level({ price: 10, label: 'support' })])
    const { container } = await renderChart()
    await waitFor(() => expect(screen.getByText(/support/)).toBeTruthy())
    const line = [...container.querySelectorAll('line')]
      .find((l) => l.getAttribute('stroke-dasharray') === '6 4')!
    expect(line).toBeTruthy()
    expect(parseFloat(line.getAttribute('x1')!)).toBe(LEFT)
    expect(parseFloat(line.getAttribute('x2')!)).toBe(LEFT + PLOT_WIDTH)
    // Horizontal: both ends at the same height.
    expect(line.getAttribute('y1')).toBe(line.getAttribute('y2'))
  })

  // Expanding the scale to fit a stray level would flatten the price series
  // into a band; the level is simply not drawn instead.
  it('omits a level far outside the visible price range', async () => {
    getChartDrawings.mockResolvedValue([level({ price: 99999 })])
    const { container } = await renderChart()
    await waitFor(() => expect(getChartDrawings).toHaveBeenCalled())
    const dashed = [...container.querySelectorAll('line')]
      .filter((l) => l.getAttribute('stroke-dasharray') === '6 4')
    expect(dashed).toHaveLength(0)
  })

  it('removes a level when its × is clicked', async () => {
    getChartDrawings.mockResolvedValue([level({ id: 7, price: 10 })])
    const { container } = await renderChart()
    await waitFor(() => expect(container.querySelector('line[stroke-dasharray="6 4"]')).toBeTruthy())
    // The <title> lives inside the group that carries the handler; selecting
    // by descendant would match the outer wrapper, which has none.
    const title = [...container.querySelectorAll('title')]
      .find((t) => t.textContent === 'Remove this level')!
    fireEvent.click(title.parentElement!)
    await waitFor(() => expect(deleteChartDrawing).toHaveBeenCalledWith(7))
  })
})

describe('placing a level', () => {
  const clickPricePanel = (container: HTMLElement, svgY: number) => {
    const svg = container.querySelector('svg')!
    // jsdom gives every element a zero-sized rect, so the component's
    // clientY→viewBox maths needs a real one to divide by.
    svg.getBoundingClientRect = () => ({
      top: 0, left: 0, width: 1040, height: 460, right: 1040, bottom: 460, x: 0, y: 0, toJSON: () => {},
    }) as DOMRect
    fireEvent.click(svg, { clientY: svgY })
  }

  it('does nothing until draw mode is on', async () => {
    const { container } = await renderChart()
    clickPricePanel(container, 200)
    expect(addChartDrawing).not.toHaveBeenCalled()
  })

  // The chart multiplies by the FX rate at render, so a level saved while
  // viewing AUD must be divided back out — otherwise it lands in the wrong
  // place the moment the currency toggle flips. This is the same contract the
  // purchase markers get wrong if `price` is used instead of `original_price`.
  it('stores a native price, not the AUD figure on screen', async () => {
    getSymbolInfo.mockResolvedValue([{ symbol: 'TST.AX', currency: 'USD', instrument_type: null, long_name: null }])
    const { container } = await renderChart({ symbol: 'TST.AX', currency: 'USD' })
    fireEvent.click(screen.getByTitle(/Draw a horizontal price level/))
    clickPricePanel(container, 200)
    await waitFor(() => expect(addChartDrawing).toHaveBeenCalled())
    const nativePrice = addChartDrawing.mock.calls[0][1]

    // With no AUD toggle applied the multiplier is 1, so the stored figure is
    // the same one the axis shows — the invariant that must hold either way.
    const axisLabels = [...container.querySelectorAll('text')]
      .map((t) => t.textContent ?? '')
      .filter((t) => /^(US\$|\$)[\d.]+$/.test(t))
      .map((t) => parseFloat(t.replace(/[^\d.]/g, '')))
    const lo = Math.min(...axisLabels)
    const hi = Math.max(...axisLabels)
    expect(nativePrice).toBeGreaterThanOrEqual(lo - (hi - lo))
    expect(nativePrice).toBeLessThanOrEqual(hi + (hi - lo))
  })

  it('saves the price under the cursor', async () => {
    const { container } = await renderChart()
    fireEvent.click(screen.getByTitle(/Draw a horizontal price level/))
    clickPricePanel(container, 200)
    await waitFor(() => expect(addChartDrawing).toHaveBeenCalled())
    const [symbol, price] = addChartDrawing.mock.calls[0]
    expect(symbol).toBe('TST.AX')
    expect(price).toBeGreaterThan(0)
  })
})

describe('trendlines', () => {
  const trend = (over: Record<string, unknown> = {}) => ({
    id: 5, symbol: 'TST.AX', kind: 'trend' as const,
    price: 9, label: null, colour: null,
    start_date: DATES[5], end_date: DATES[20], end_price: 11,
    created_at: 'x', ...over,
  })
  const solidLines = (c: HTMLElement) =>
    [...c.querySelectorAll('line')].filter((l) => !l.getAttribute('stroke-dasharray') && l.getAttribute('stroke') === DRAWING_COLOR)

  it('draws a sloped segment between its two anchors', async () => {
    getChartDrawings.mockResolvedValue([trend()])
    const { container } = await renderChart()
    await waitFor(() => expect(solidLines(container).length).toBeGreaterThan(0))
    const seg = solidLines(container)[0]
    const x1 = parseFloat(seg.getAttribute('x1')!)
    const x2 = parseFloat(seg.getAttribute('x2')!)
    const y1 = parseFloat(seg.getAttribute('y1')!)
    const y2 = parseFloat(seg.getAttribute('y2')!)
    expect(Math.abs(x1 - (LEFT + (PLOT_WIDTH * 5) / (DATES.length - 1)))).toBeLessThan(0.5)
    expect(Math.abs(x2 - (LEFT + (PLOT_WIDTH * 20) / (DATES.length - 1)))).toBeLessThan(0.5)
    // Rising price means a lower y at the later anchor.
    expect(y2).toBeLessThan(y1)
  })

  // Projection past the second anchor is the point of drawing a trendline.
  it('projects past the second anchor to the right edge', async () => {
    getChartDrawings.mockResolvedValue([trend()])
    const { container } = await renderChart()
    await waitFor(() => expect(solidLines(container).length).toBeGreaterThan(0))
    const projected = [...container.querySelectorAll('line')]
      .find((l) => l.getAttribute('stroke-dasharray') === '4 4')!
    expect(projected).toBeTruthy()
    expect(Math.abs(parseFloat(projected.getAttribute('x2')!) - (LEFT + PLOT_WIDTH))).toBeLessThan(0.5)
  })

  // Clamping an off-screen anchor would move it and change the slope — the one
  // thing a trendline must never do. It is clipped instead.
  it('keeps its slope when an anchor predates the visible window', async () => {
    getChartDrawings.mockResolvedValue([trend({ start_date: '2020-01-02', price: 5 })])
    const { container } = await renderChart()
    await waitFor(() => expect(solidLines(container).length).toBeGreaterThan(0))
    const seg = solidLines(container)[0]
    expect(parseFloat(seg.getAttribute('x1')!)).toBeLessThan(LEFT)
    expect(seg.closest('g')!.getAttribute('clip-path')).toMatch(/^url\(#/)
  })

  it('needs two clicks on different dates to save a line', async () => {
    const { container } = await renderChart()
    fireEvent.click(screen.getByTitle(/Draw a trendline/))
    const svg = container.querySelector('svg')!
    svg.getBoundingClientRect = () => ({
      top: 0, left: 0, width: 1040, height: 460, right: 1040, bottom: 460, x: 0, y: 0, toJSON: () => {},
    }) as DOMRect

    fireEvent.click(svg, { clientX: 200, clientY: 200 })
    expect(addTrendline).not.toHaveBeenCalled()
    await waitFor(() => expect(screen.getByText(/Anchored at/)).toBeTruthy())

    fireEvent.click(svg, { clientX: 800, clientY: 150 })
    await waitFor(() => expect(addTrendline).toHaveBeenCalled())
    const [, line] = addTrendline.mock.calls[0]
    expect(line.startDate < line.endDate).toBe(true)
    expect(line.startPrice).toBeGreaterThan(0)
    expect(line.endPrice).toBeGreaterThan(0)
  })
})

describe('configured defaults', () => {
  const setStored = (d: Record<string, unknown>) =>
    (apiClient.getConfig as ReturnType<typeof vi.fn>).mockResolvedValue({ chart_defaults: JSON.stringify(d) })

  it('opens with the configured period, style, bars and overlays', async () => {
    setStored({ timeframe: '1m', chartType: 'candle', barInterval: 'week', overlays: ['ema40'] })
    await renderChart()
    const active = (label: string) =>
      screen.getAllByRole('button').find((b) => b.textContent!.trim() === label)!.className.includes('active')
    await waitFor(() => expect(active('1M')).toBe(true))
    expect(active('6M')).toBe(false)
    expect(active('EMA 40')).toBe(true)
    expect(active('SMA 50')).toBe(false)
    expect(active('Week')).toBe(true)
  })

  // The config arrives after first render; snapping a control back a moment
  // after the user clicked it would look like the chart ignoring the click.
  it('does not overwrite a control the user has already changed', async () => {
    let release: (v: Record<string, string>) => void = () => {}
    ;(apiClient.getConfig as ReturnType<typeof vi.fn>).mockReturnValue(
      new Promise<Record<string, string>>((res) => { release = res }),
    )
    await renderChart()
    fireEvent.click(screen.getAllByRole('button').find((b) => b.textContent!.trim() === '1W')!)

    release({ chart_defaults: JSON.stringify({ timeframe: '2y' }) })
    await new Promise((r) => setTimeout(r, 50))

    const active = (label: string) =>
      screen.getAllByRole('button').find((b) => b.textContent!.trim() === label)!.className.includes('active')
    expect(active('1W')).toBe(true)
    expect(active('2Y')).toBe(false)
  })
})
