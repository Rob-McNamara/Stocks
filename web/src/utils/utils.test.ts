import { describe, it, expect } from 'vitest'
import { calculateSMA, calculateEMA, getLatestSMA, smaTrend } from './sma'
import { toWeeklyBars } from './bars'
import { mapLimit } from './async'
import { getActiveHoldingSymbols, getEarliestRemainingPurchaseDate, getRemainingPurchaseLots } from './holdings'
import { parseChartDefaults, FALLBACK_CHART_DEFAULTS, CHART_HEIGHT_RANGE } from './chartDefaults'
import { parseLayoutWidth, LAYOUT_WIDTHS, FALLBACK_LAYOUT_WIDTH } from './layout'

// The FIFO/P&L engine and its test suite now live in the Rust API server
// (src/portfolio.rs) — these tests cover the utilities that remain
// client-side: chart indicators, request throttling and small helpers.

const closes = (values: Array<number | null>) => values.map((close) => ({ close }))

describe('calculateSMA', () => {
  it('computes the rolling average once the window is full', () => {
    const sma = calculateSMA(closes([1, 2, 3, 4, 5]), 3)
    expect(sma[0]).toBeNull()
    expect(sma[1]).toBeNull()
    expect(sma[2]).toBeCloseTo(2)
    expect(sma[3]).toBeCloseTo(3)
    expect(sma[4]).toBeCloseTo(4)
  })

  it('returns null for windows containing null closes', () => {
    const sma = calculateSMA(closes([1, null, 3, 4, 5]), 3)
    expect(sma[2]).toBeNull() // window includes the null
    expect(sma[4]).toBeCloseTo(4) // 3,4,5
  })
})

describe('calculateEMA', () => {
  it('seeds from the simple average of the first full window', () => {
    const ema = calculateEMA(closes([1, 2, 3, 4, 5]), 3)
    expect(ema[0]).toBeNull()
    expect(ema[1]).toBeNull()
    expect(ema[2]).toBeCloseTo(2) // (1+2+3)/3
  })

  it('weights later points by 2/(period+1)', () => {
    const ema = calculateEMA(closes([1, 2, 3, 4, 5]), 3)
    // k = 0.5; 4 × 0.5 + 2 × 0.5 = 3, then 5 × 0.5 + 3 × 0.5 = 4
    expect(ema[3]).toBeCloseTo(3)
    expect(ema[4]).toBeCloseTo(4)
  })

  /** The distinguishing property: EMA tracks a step change faster than SMA. */
  it('responds to a jump sooner than the equivalent SMA', () => {
    const series = closes([10, 10, 10, 10, 10, 20])
    const ema = calculateEMA(series, 5)
    const sma = calculateSMA(series, 5)
    expect(ema[5]!).toBeGreaterThan(sma[5]!)
  })

  it('restarts after a null rather than carrying the average across the gap', () => {
    const ema = calculateEMA(closes([1, 2, 3, null, 4, 5, 6]), 3)
    expect(ema[2]).toBeCloseTo(2)
    expect(ema[3]).toBeNull() // the gap itself
    expect(ema[4]).toBeNull() // rebuilding the seed window
    expect(ema[5]).toBeNull()
    expect(ema[6]).toBeCloseTo(5) // (4+5+6)/3 — a fresh seed
  })

  it('returns all nulls when the series is shorter than the period', () => {
    expect(calculateEMA(closes([1, 2]), 5).every((v) => v === null)).toBe(true)
  })
})

describe('toWeeklyBars', () => {
  const bar = (date: string, o: number, h: number, l: number, c: number, v: number) =>
    ({ date, open: o, high: h, low: l, close: c, volume: v })

  it('takes the first open, extreme high/low, last close and summed volume', () => {
    // Mon–Wed of one week
    const weekly = toWeeklyBars([
      bar('2026-08-03', 10, 12, 9, 11, 100),
      bar('2026-08-04', 11, 15, 8, 14, 200),
      bar('2026-08-05', 14, 13, 12, 13, 300),
    ])
    expect(weekly).toHaveLength(1)
    expect(weekly[0]).toMatchObject({ open: 10, high: 15, low: 8, close: 13, volume: 600 })
  })

  it('dates each bar by its last trading day', () => {
    const weekly = toWeeklyBars([bar('2026-08-03', 1, 1, 1, 1, 1), bar('2026-08-05', 1, 1, 1, 1, 1)])
    expect(weekly[0].date).toBe('2026-08-05')
  })

  it('splits on the Monday boundary', () => {
    // Sun 9 Aug closes one week; Mon 10 Aug starts the next
    const weekly = toWeeklyBars([
      bar('2026-08-07', 1, 1, 1, 1, 1), // Friday
      bar('2026-08-09', 2, 2, 2, 2, 1), // Sunday — same week
      bar('2026-08-10', 3, 3, 3, 3, 1), // Monday — new week
    ])
    expect(weekly).toHaveLength(2)
    expect(weekly[0].date).toBe('2026-08-09')
    expect(weekly[1].date).toBe('2026-08-10')
    expect(weekly[1].open).toBe(3)
  })

  it('spans a year boundary without merging weeks', () => {
    const weekly = toWeeklyBars([
      bar('2025-12-30', 1, 1, 1, 1, 1), // Tuesday
      bar('2026-01-01', 2, 2, 2, 2, 1), // Thursday, same week
      bar('2026-01-05', 3, 3, 3, 3, 1), // Monday, next week
    ])
    expect(weekly).toHaveLength(2)
    expect(weekly[0].close).toBe(2)
  })

  /** Bars predating OHLC ingest must aggregate to null, not to zero. */
  it('keeps null OHLC null rather than coercing to zero', () => {
    const weekly = toWeeklyBars([
      { date: '2026-08-03', open: null, high: null, low: null, close: 10, volume: null },
      { date: '2026-08-04', open: null, high: null, low: null, close: 11, volume: null },
    ])
    expect(weekly[0]).toMatchObject({ open: null, high: null, low: null, close: 11, volume: null })
  })

  it('returns an empty array for no input', () => {
    expect(toWeeklyBars([])).toEqual([])
  })
})

describe('getLatestSMA', () => {
  it('returns the last non-null value', () => {
    expect(getLatestSMA([null, 2, 3, null])).toBe(3)
  })

  it('returns null when all values are null', () => {
    expect(getLatestSMA([null, null])).toBeNull()
  })
})

describe('smaTrend', () => {
  it('detects an upward trend', () => {
    const sma = [null, null, 1, 2, 3, 4, 5, 6, 7, 8]
    expect(smaTrend(sma, 5)).toBe('up')
  })

  it('detects a downward trend', () => {
    const sma = [null, null, 8, 7, 6, 5, 4, 3, 2, 1]
    expect(smaTrend(sma, 5)).toBe('down')
  })

  it('returns null with insufficient data', () => {
    expect(smaTrend([null, 1, 2], 5)).toBeNull()
  })
})

describe('mapLimit', () => {
  it('preserves input order in results', async () => {
    const results = await mapLimit([3, 1, 2], 2, async (n) => {
      await new Promise((resolve) => setTimeout(resolve, n * 5))
      return n * 10
    })
    expect(results).toEqual([30, 10, 20])
  })

  it('never exceeds the concurrency limit', async () => {
    let inFlight = 0
    let maxInFlight = 0
    await mapLimit([1, 2, 3, 4, 5, 6], 2, async () => {
      inFlight++
      maxInFlight = Math.max(maxInFlight, inFlight)
      await new Promise((resolve) => setTimeout(resolve, 5))
      inFlight--
    })
    expect(maxInFlight).toBeLessThanOrEqual(2)
  })

  it('handles an empty input', async () => {
    expect(await mapLimit([], 4, async (x) => x)).toEqual([])
  })
})

describe('getActiveHoldingSymbols', () => {
  it('includes only symbols with net positive shares', () => {
    const txs = [
      { symbol: 'AAA.AX', transaction_type: 'purchase', quantity: 100 },
      { symbol: 'AAA.AX', transaction_type: 'sale', quantity: 100 },
      { symbol: 'BBB.AX', transaction_type: 'purchase', quantity: 50 },
    ]
    expect(getActiveHoldingSymbols(txs)).toEqual(['BBB.AX'])
  })
})

describe('getEarliestRemainingPurchaseDate', () => {
  it('skips lots fully consumed by FIFO sales', () => {
    const txs = [
      { symbol: 'TST.AX', transaction_type: 'purchase', quantity: 50, date: '2024-01-01', id: 1 },
      { symbol: 'TST.AX', transaction_type: 'purchase', quantity: 50, date: '2024-03-01', id: 2 },
      { symbol: 'TST.AX', transaction_type: 'sale', quantity: 50, date: '2024-06-01', id: 3 },
    ]
    expect(getEarliestRemainingPurchaseDate(txs, 'TST.AX')).toBe('2024-03-01')
  })

  it('returns null when everything is sold', () => {
    const txs = [
      { symbol: 'TST.AX', transaction_type: 'purchase', quantity: 50, date: '2024-01-01', id: 1 },
      { symbol: 'TST.AX', transaction_type: 'sale', quantity: 50, date: '2024-06-01', id: 2 },
    ]
    expect(getEarliestRemainingPurchaseDate(txs, 'TST.AX')).toBeNull()
  })
})

describe('getRemainingPurchaseLots', () => {
  const tx = (id: number, type: string, date: string, quantity: number, price: number) =>
    ({ id, symbol: 'AAA.AX', transaction_type: type, date, quantity, price })

  it('returns every buy that makes up the current position', () => {
    const lots = getRemainingPurchaseLots([
      tx(1, 'purchase', '2025-01-10', 100, 10),
      tx(2, 'purchase', '2025-06-15', 50, 12),
    ], 'AAA.AX')
    expect(lots.map((l) => [l.date, l.price, l.quantity])).toEqual([
      ['2025-01-10', 10, 100],
      ['2025-06-15', 12, 50],
    ])
  })

  // A dot for a parcel you no longer hold would sit on the chart with nothing
  // behind it.
  it('drops lots that FIFO sales have consumed', () => {
    const lots = getRemainingPurchaseLots([
      tx(1, 'purchase', '2025-01-10', 100, 10),
      tx(2, 'purchase', '2025-06-15', 50, 12),
      tx(3, 'sale', '2025-08-01', 100, 15),
    ], 'AAA.AX')
    expect(lots.map((l) => l.date)).toEqual(['2025-06-15'])
  })

  it('keeps the unsold remainder of a partly sold lot', () => {
    const lots = getRemainingPurchaseLots([
      tx(1, 'purchase', '2025-01-10', 100, 10),
      tx(2, 'sale', '2025-08-01', 40, 15),
    ], 'AAA.AX')
    expect(lots).toEqual([{ date: '2025-01-10', price: 10, quantity: 60 }])
  })

  // The chart draws in the symbol's own currency and applies the FX rate
  // itself, so an AUD figure here plots a USD chart against AUD prices — and
  // doubles the error once the chart is toggled to AUD.
  it('reports a foreign lot in its native currency, not AUD', () => {
    const lots = getRemainingPurchaseLots([
      { id: 1, symbol: 'GOOGL', transaction_type: 'purchase', date: '2026-08-05',
        quantity: 8, price: 542.38, original_price: 382.2 },
      { id: 2, symbol: 'GOOGL', transaction_type: 'purchase', date: '2026-08-20',
        quantity: 8, price: 482.09, original_price: 342.83 },
    ], 'GOOGL')
    expect(lots.map((l) => l.price)).toEqual([382.2, 342.83])
  })

  it('uses price directly for an AUD trade, which has no original_price', () => {
    const lots = getRemainingPurchaseLots([
      { id: 1, symbol: 'NUGG.AX', transaction_type: 'purchase', date: '2026-02-02',
        quantity: 25, price: 68, original_price: null },
    ], 'NUGG.AX')
    expect(lots[0].price).toBe(68)
  })

  it('ignores other symbols and fully closed positions', () => {
    const lots = getRemainingPurchaseLots([
      tx(1, 'purchase', '2025-01-10', 100, 10),
      tx(2, 'sale', '2025-08-01', 100, 15),
      { id: 3, symbol: 'BBB.AX', transaction_type: 'purchase', date: '2025-02-01', quantity: 5, price: 3 },
    ], 'AAA.AX')
    expect(lots).toEqual([])
  })
})

describe('parseChartDefaults', () => {
  const known = ['sma20', 'ema40', 'sma50', 'sma100', 'sma150', 'sma200']

  it('returns the built-in defaults when nothing is stored', () => {
    expect(parseChartDefaults(undefined, known)).toEqual(FALLBACK_CHART_DEFAULTS)
  })

  it('reads a stored setting back', () => {
    const stored = JSON.stringify({ timeframe: '2y', chartType: 'candle', barInterval: 'week', overlays: ['ema40'], height: 520 })
    expect(parseChartDefaults(stored, known)).toEqual({
      timeframe: '2y', chartType: 'candle', barInterval: 'week', overlays: ['ema40'], height: 520,
    })
  })

  // A value from an older version must not leave the chart in a state its own
  // controls cannot represent — every button would read inactive with nothing
  // explaining why.
  it('falls back field by field on unknown values', () => {
    const stored = JSON.stringify({ timeframe: '10y', chartType: 'pie', barInterval: 'month', overlays: ['ema40'] })
    const result = parseChartDefaults(stored, known)
    expect(result.timeframe).toBe(FALLBACK_CHART_DEFAULTS.timeframe)
    expect(result.chartType).toBe(FALLBACK_CHART_DEFAULTS.chartType)
    expect(result.barInterval).toBe(FALLBACK_CHART_DEFAULTS.barInterval)
    expect(result.overlays).toEqual(['ema40'])
  })

  it('drops overlay ids that no longer exist', () => {
    const stored = JSON.stringify({ overlays: ['sma50', 'sma999', 'ema40'] })
    expect(parseChartDefaults(stored, known).overlays).toEqual(['sma50', 'ema40'])
  })

  it('clamps a height outside the usable range', () => {
    expect(parseChartDefaults(JSON.stringify({ height: 50 }), known).height).toBe(CHART_HEIGHT_RANGE.min)
    expect(parseChartDefaults(JSON.stringify({ height: 5000 }), known).height).toBe(CHART_HEIGHT_RANGE.max)
  })

  /**
   * The floor is not arbitrary: under it the price panel is clamped to its
   * minimum and the chart SVG grows taller than the frame that measures it,
   * which the frame's `overflow: auto` would surface as a scrollbar. The
   * volume panel is 100, its gap 40, and the smallest usable price panel 120.
   */
  it('floors the height where the volume panel and price panel both still fit', () => {
    expect(CHART_HEIGHT_RANGE.min).toBe(100 + 40 + 120)
    // A height stored under an older, lower floor is lifted rather than lost.
    expect(parseChartDefaults(JSON.stringify({ height: 240 }), known).height).toBe(CHART_HEIGHT_RANGE.min)
  })

  it('falls back when the height is not a usable number', () => {
    for (const bad of ['400', null, NaN]) {
      expect(parseChartDefaults(JSON.stringify({ height: bad }), known).height)
        .toBe(FALLBACK_CHART_DEFAULTS.height)
    }
  })

  it('survives malformed JSON', () => {
    expect(parseChartDefaults('{not json', known)).toEqual(FALLBACK_CHART_DEFAULTS)
  })

  it('allows an empty overlay set, which is a real choice', () => {
    expect(parseChartDefaults(JSON.stringify({ overlays: [] }), known).overlays).toEqual([])
  })
})

describe('parseLayoutWidth', () => {
  it('accepts every offered width', () => {
    for (const [value] of LAYOUT_WIDTHS) expect(parseLayoutWidth(value)).toBe(value)
  })

  it('falls back for nothing stored or an unknown value', () => {
    expect(parseLayoutWidth(undefined)).toBe(FALLBACK_LAYOUT_WIDTH)
    expect(parseLayoutWidth('ultrawide')).toBe(FALLBACK_LAYOUT_WIDTH)
    expect(parseLayoutWidth('')).toBe(FALLBACK_LAYOUT_WIDTH)
  })
})
