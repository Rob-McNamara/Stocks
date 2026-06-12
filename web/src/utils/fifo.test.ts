import { describe, it, expect } from 'vitest'
import { applyFifoSale, calcSymbolSummary, calcPortfolioPL, sortTransactions, type Transaction } from './fifo'

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeTx(
  id: number,
  type: 'purchase' | 'sale' | 'dividend',
  date: string,
  quantity: number | null,
  price: number | null,
  opts: { brokerage?: number; amount?: number; dividends_total?: number } = {}
): Transaction {
  return {
    id,
    symbol: 'TST.AX',
    transaction_type: type,
    date,
    quantity,
    price,
    amount: opts.amount ?? null,
    brokerage: opts.brokerage ?? null,
    dividends_total: opts.dividends_total ?? 0,
  }
}

// ---------------------------------------------------------------------------
// sortTransactions
// ---------------------------------------------------------------------------

describe('sortTransactions', () => {
  it('sorts by date ascending', () => {
    const txs = [makeTx(1, 'purchase', '2024-06-01', 100, 10), makeTx(2, 'purchase', '2024-01-01', 50, 9)]
    const sorted = sortTransactions(txs)
    expect(sorted[0].date).toBe('2024-01-01')
  })

  it('breaks ties by id', () => {
    const txs = [makeTx(3, 'sale', '2024-01-01', 50, 12), makeTx(1, 'purchase', '2024-01-01', 100, 10)]
    const sorted = sortTransactions(txs)
    expect(sorted[0].id).toBe(1)
  })
})

// ---------------------------------------------------------------------------
// applyFifoSale
// ---------------------------------------------------------------------------

describe('applyFifoSale', () => {
  it('consumes a single lot fully', () => {
    const lots = [{ quantity: 100, price: 10 }]
    const costBasis = applyFifoSale(lots, 100)
    expect(costBasis).toBe(1000)
    expect(lots).toHaveLength(0)
  })

  it('consumes a single lot partially', () => {
    const lots = [{ quantity: 100, price: 10 }]
    const costBasis = applyFifoSale(lots, 40)
    expect(costBasis).toBe(400)
    expect(lots[0].quantity).toBe(60)
  })

  it('consumes across multiple lots in order', () => {
    const lots = [{ quantity: 50, price: 10 }, { quantity: 50, price: 20 }]
    const costBasis = applyFifoSale(lots, 75)
    // 50 @ $10 + 25 @ $20 = $1000
    expect(costBasis).toBe(1000)
    expect(lots).toHaveLength(1)
    expect(lots[0].quantity).toBe(25)
    expect(lots[0].price).toBe(20)
  })

  it('returns 0 and leaves lots empty when quantity exceeds supply', () => {
    const lots = [{ quantity: 30, price: 10 }]
    const costBasis = applyFifoSale(lots, 50)
    expect(costBasis).toBe(300)  // only 30 available
    expect(lots).toHaveLength(0)
  })
})

// ---------------------------------------------------------------------------
// calcSymbolSummary
// ---------------------------------------------------------------------------

describe('calcSymbolSummary', () => {
  it('single purchase, no sales', () => {
    const txs = [makeTx(1, 'purchase', '2024-01-01', 100, 10)]
    const s = calcSymbolSummary(txs)
    expect(s.remainingShares).toBe(100)
    expect(s.remainingCost).toBe(1000)
    expect(s.realisedPL).toBe(0)
    expect(s.totalSoldQty).toBe(0)
  })

  it('partial sale — remaining shares and cost correct', () => {
    const txs = [
      makeTx(1, 'purchase', '2024-01-01', 100, 10),
      makeTx(2, 'sale',     '2024-06-01',  40, 15),
    ]
    const s = calcSymbolSummary(txs)
    expect(s.remainingShares).toBe(60)
    expect(s.remainingCost).toBeCloseTo(600)
    // proceeds = 40 * 15 = 600; cost = 40 * 10 = 400; P/L = 200
    expect(s.realisedPL).toBeCloseTo(200)
  })

  it('full sale — zero remaining', () => {
    const txs = [
      makeTx(1, 'purchase', '2024-01-01', 100, 10),
      makeTx(2, 'sale',     '2024-06-01', 100, 15),
    ]
    const s = calcSymbolSummary(txs)
    expect(s.remainingShares).toBe(0)
    expect(s.remainingCost).toBe(0)
    expect(s.realisedPL).toBeCloseTo(500)
  })

  it('full sale at a loss', () => {
    const txs = [
      makeTx(1, 'purchase', '2024-01-01', 100, 10),
      makeTx(2, 'sale',     '2024-06-01', 100,  7),
    ]
    const s = calcSymbolSummary(txs)
    expect(s.realisedPL).toBeCloseTo(-300)
  })

  it('brokerage reduces realised P/L', () => {
    const txs = [
      makeTx(1, 'purchase', '2024-01-01', 100, 10),
      makeTx(2, 'sale',     '2024-06-01', 100, 15, { brokerage: 9.95 }),
    ]
    const s = calcSymbolSummary(txs)
    expect(s.realisedPL).toBeCloseTo(500 - 9.95)
  })

  it('multiple purchases FIFO order respected', () => {
    const txs = [
      makeTx(1, 'purchase', '2024-01-01', 50, 10),  // lot 1: cheaper
      makeTx(2, 'purchase', '2024-03-01', 50, 20),  // lot 2: more expensive
      makeTx(3, 'sale',     '2024-06-01', 50, 15),  // sells lot 1 first
    ]
    const s = calcSymbolSummary(txs)
    // cost basis = 50 * 10 = 500; proceeds = 50 * 15 = 750; P/L = 250
    expect(s.realisedPL).toBeCloseTo(250)
    // Remaining: lot 2 intact at $20
    expect(s.remainingShares).toBe(50)
    expect(s.remainingCost).toBeCloseTo(1000)
  })

  it('multiple sales consume lots sequentially', () => {
    const txs = [
      makeTx(1, 'purchase', '2024-01-01', 100, 10),
      makeTx(2, 'sale',     '2024-04-01',  60, 15),
      makeTx(3, 'sale',     '2024-08-01',  40, 20),
    ]
    const s = calcSymbolSummary(txs)
    expect(s.remainingShares).toBe(0)
    // Sale 1: 60 * 15 - 60 * 10 = 300
    // Sale 2: 40 * 20 - 40 * 10 = 400
    expect(s.realisedPL).toBeCloseTo(700)
  })

  it('reads dividends_total from transactions', () => {
    const txs = [makeTx(1, 'purchase', '2024-01-01', 100, 10, { dividends_total: 55.5 })]
    const s = calcSymbolSummary(txs)
    expect(s.dividendsTotal).toBe(55.5)
  })
})

// ---------------------------------------------------------------------------
// calcPortfolioPL
// ---------------------------------------------------------------------------

describe('calcPortfolioPL', () => {
  it('active holding with known price', () => {
    const txs = [makeTx(1, 'purchase', '2024-01-01', 100, 10)]
    const result = calcPortfolioPL(txs, { 'TST.AX': 15 })
    // P/L = 100*15 - 100*10 = 500
    expect(result.holdingsPL).toBeCloseTo(500)
    expect(result.soldPL).toBe(0)
    expect(result.totalPL).toBeCloseTo(500)
    expect(result.totalValue).toBeCloseTo(1500)
    expect(result.stockCount).toBe(1)
  })

  it('active holding with no price contributes 0 to value and negative P/L', () => {
    const txs = [makeTx(1, 'purchase', '2024-01-01', 100, 10)]
    const result = calcPortfolioPL(txs, { 'TST.AX': null })
    expect(result.totalValue).toBe(0)
    expect(result.holdingsPL).toBeCloseTo(-1000)
  })

  it('fully sold position adds to soldPL', () => {
    const txs = [
      makeTx(1, 'purchase', '2024-01-01', 100, 10),
      makeTx(2, 'sale',     '2024-06-01', 100, 15),
    ]
    const result = calcPortfolioPL(txs, {})
    expect(result.soldPL).toBeCloseTo(500)
    expect(result.holdingsPL).toBe(0)
    expect(result.stockCount).toBe(0)
  })

  it('partial sale: unrealised on remaining, realised on sold portion', () => {
    const txs = [
      makeTx(1, 'purchase', '2024-01-01', 100, 10),
      makeTx(2, 'sale',     '2024-06-01',  40, 15),
    ]
    // 60 remain @ cost $600, current price $18 → unrealised = 60*18 - 600 = 480
    // 40 sold: proceeds 600, cost 400 → realised = 200
    const result = calcPortfolioPL(txs, { 'TST.AX': 18 })
    expect(result.holdingsPL).toBeCloseTo(480)
    expect(result.soldPL).toBeCloseTo(200)
    expect(result.totalPL).toBeCloseTo(680)
  })

  it('dividends added to holdingsPL for active position', () => {
    const txs = [makeTx(1, 'purchase', '2024-01-01', 100, 10, { dividends_total: 50 })]
    const result = calcPortfolioPL(txs, { 'TST.AX': 10 })
    // price = cost, so unrealised = 0; P/L = dividends = 50
    expect(result.holdingsPL).toBeCloseTo(50)
  })

  it('dividends proportionally added to soldPL for sold position', () => {
    const txs = [
      makeTx(1, 'purchase', '2024-01-01', 100, 10, { dividends_total: 60 }),
      makeTx(2, 'sale',     '2024-06-01', 100, 10),  // sold at cost (no price gain)
    ]
    // All dividends ($60) go to sold P/L since fully sold
    const result = calcPortfolioPL(txs, {})
    expect(result.soldPL).toBeCloseTo(60)
  })

  it('multiple symbols summed correctly', () => {
    const txs = [
      { ...makeTx(1, 'purchase', '2024-01-01', 100, 10), symbol: 'AAA.AX' },
      { ...makeTx(2, 'purchase', '2024-01-01',  50, 20), symbol: 'BBB.AX' },
    ]
    const result = calcPortfolioPL(txs, { 'AAA.AX': 12, 'BBB.AX': 18 })
    // AAA: 100*(12-10) = 200; BBB: 50*(18-20) = -100
    expect(result.totalPL).toBeCloseTo(100)
    expect(result.stockCount).toBe(2)
  })
})
