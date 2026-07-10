import { describe, it, expect } from 'vitest'
import { calculateSMA, getLatestSMA, smaTrend } from './sma'
import { mapLimit } from './async'
import { getActiveHoldingSymbols, getEarliestRemainingPurchaseDate } from './holdings'

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
