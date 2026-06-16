import { describe, it, expect } from 'vitest'
import { applyFifoSale, calcSymbolSummary, calcPortfolioPL, calcRemainingByLot, sortTransactions, type Transaction } from './fifo'

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeTx(
  id: number,
  type: 'purchase' | 'sale' | 'dividend',
  date: string,
  quantity: number | null,
  price: number | null,
  opts: {
    brokerage?: number
    amount?: number
    dividends_total?: number
    symbol?: string
    currency?: string
    original_price?: number
    fx_rate?: number
  } = {}
): Transaction {
  return {
    id,
    symbol: opts.symbol ?? 'TST.AX',
    transaction_type: type,
    date,
    quantity,
    price,
    amount: opts.amount ?? null,
    brokerage: opts.brokerage ?? null,
    dividends_total: opts.dividends_total ?? 0,
    currency: opts.currency,
    original_price: opts.original_price ?? null,
    fx_rate: opts.fx_rate ?? null,
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

// ---------------------------------------------------------------------------
// calcRemainingByLot
// ---------------------------------------------------------------------------

describe('calcRemainingByLot', () => {
  it('single purchase — full quantity remains', () => {
    const txs = [makeTx(1, 'purchase', '2024-01-01', 100, 10)]
    const r = calcRemainingByLot(txs)
    expect(r[1]).toBe(100)
  })

  it('partial sale reduces the earliest lot', () => {
    const txs = [
      makeTx(1, 'purchase', '2024-01-01', 100, 10),
      makeTx(2, 'sale',     '2024-06-01',  40, 15),
    ]
    const r = calcRemainingByLot(txs)
    expect(r[1]).toBe(60)
  })

  it('full sale sets remaining to 0', () => {
    const txs = [
      makeTx(1, 'purchase', '2024-01-01', 100, 10),
      makeTx(2, 'sale',     '2024-06-01', 100, 15),
    ]
    const r = calcRemainingByLot(txs)
    expect(r[1]).toBe(0)
  })

  it('sale spanning two lots reduces each in FIFO order', () => {
    const txs = [
      makeTx(1, 'purchase', '2024-01-01',  60, 10),
      makeTx(2, 'purchase', '2024-03-01',  60, 20),
      makeTx(3, 'sale',     '2024-06-01',  80, 25),
    ]
    const r = calcRemainingByLot(txs)
    // First lot (60) fully consumed, second lot reduced by 20
    expect(r[1]).toBe(0)
    expect(r[2]).toBe(40)
  })

  it('dividend transactions are ignored', () => {
    const txs = [
      makeTx(1, 'purchase', '2024-01-01', 100, 10),
      makeTx(2, 'dividend', '2024-06-01', null, null, { amount: 50 }),
    ]
    const r = calcRemainingByLot(txs)
    expect(r[1]).toBe(100)
    expect(r[2]).toBeUndefined()
  })

  it('multiple symbols are independent', () => {
    const txs = [
      makeTx(1, 'purchase', '2024-01-01', 100, 10, { symbol: 'AAA.AX' }),
      makeTx(2, 'sale',     '2024-06-01', 100, 15, { symbol: 'AAA.AX' }),
      makeTx(3, 'purchase', '2024-01-01',  50, 20, { symbol: 'BBB.AX' }),
    ]
    const r = calcRemainingByLot(txs)
    expect(r[1]).toBe(0)   // AAA fully sold
    expect(r[3]).toBe(50)  // BBB untouched
  })

  // -------------------------------------------------------------------------
  // International stock (USD) — the SPCX regression case.
  //
  // When a user buys an international stock, `price` is stored as the AUD-
  // converted value (original_price × fx_rate).  The FIFO logic uses `price`
  // only, so currency fields are informational and must not affect whether the
  // lot appears in the active-holdings table (remaining > 0).
  // -------------------------------------------------------------------------

  it('USD stock purchase appears with positive remaining — single buy, no sale', () => {
    // Represents: 50 shares of SPCX at USD 1.50, FX rate 1.496, stored as AUD 2.244
    const txs = [
      makeTx(1, 'purchase', '2026-01-15', 50, 2.244, {
        symbol: 'SPCX',
        currency: 'USD',
        original_price: 1.50,
        fx_rate: 1.496,
      }),
    ]
    const r = calcRemainingByLot(txs)
    // Must be > 0 so that the Active Holdings table shows this transaction
    expect(r[1]).toBe(50)
    expect(r[1]).toBeGreaterThan(0)
  })

  it('USD stock partial sale — remaining lot still positive', () => {
    const txs = [
      makeTx(1, 'purchase', '2026-01-15', 100, 2.244, {
        symbol: 'SPCX',
        currency: 'USD',
        original_price: 1.50,
        fx_rate: 1.496,
      }),
      makeTx(2, 'sale', '2026-06-01', 40, 2.80, {
        symbol: 'SPCX',
        currency: 'USD',
        original_price: 1.87,
        fx_rate: 1.496,
      }),
    ]
    const r = calcRemainingByLot(txs)
    expect(r[1]).toBe(60)
    expect(r[1]).toBeGreaterThan(0)
  })

  it('USD stock full sale — remaining is 0 and stock is not shown as active', () => {
    const txs = [
      makeTx(1, 'purchase', '2026-01-15', 50, 2.244, {
        symbol: 'SPCX',
        currency: 'USD',
        original_price: 1.50,
        fx_rate: 1.496,
      }),
      makeTx(2, 'sale', '2026-06-01', 50, 2.80, {
        symbol: 'SPCX',
        currency: 'USD',
        original_price: 1.87,
        fx_rate: 1.496,
      }),
    ]
    const r = calcRemainingByLot(txs)
    expect(r[1]).toBe(0)
  })

  it('mixed portfolio — USD stock and AUD stock both appear correctly', () => {
    const txs = [
      makeTx(1, 'purchase', '2026-01-15', 50, 2.244, {
        symbol: 'SPCX',
        currency: 'USD',
        original_price: 1.50,
        fx_rate: 1.496,
      }),
      makeTx(2, 'purchase', '2026-02-01', 200, 45.00, { symbol: 'CBA.AX' }),
      makeTx(3, 'sale',     '2026-05-01', 200, 48.00, { symbol: 'CBA.AX' }),
    ]
    const r = calcRemainingByLot(txs)
    // SPCX: still active
    expect(r[1]).toBe(50)
    // CBA: fully sold
    expect(r[2]).toBe(0)
  })
})

// ---------------------------------------------------------------------------
// International stocks in calcSymbolSummary and calcPortfolioPL
// ---------------------------------------------------------------------------

describe('international stock — calcSymbolSummary', () => {
  it('USD purchase gives positive remainingShares', () => {
    const txs = [
      makeTx(1, 'purchase', '2026-01-15', 50, 2.244, {
        symbol: 'SPCX',
        currency: 'USD',
        original_price: 1.50,
        fx_rate: 1.496,
      }),
    ]
    const s = calcSymbolSummary(txs)
    expect(s.remainingShares).toBe(50)
    expect(s.remainingCost).toBeCloseTo(50 * 2.244)
  })

  it('P/L uses AUD price consistently regardless of currency field', () => {
    // Bought 100 shares at AUD 2.244 (USD 1.50 × 1.496)
    // Sold 100 shares at AUD 2.80 (USD 1.87 × ~1.497)
    const txs = [
      makeTx(1, 'purchase', '2026-01-15', 100, 2.244, {
        symbol: 'SPCX',
        currency: 'USD',
        original_price: 1.50,
        fx_rate: 1.496,
      }),
      makeTx(2, 'sale', '2026-06-01', 100, 2.80, {
        symbol: 'SPCX',
        currency: 'USD',
        original_price: 1.87,
        fx_rate: 1.497,
      }),
    ]
    const s = calcSymbolSummary(txs)
    // P/L = 100 * (2.80 - 2.244) = 55.60 AUD
    expect(s.realisedPL).toBeCloseTo(55.60)
    expect(s.remainingShares).toBe(0)
  })
})

// ---------------------------------------------------------------------------
// Full-dataset regression — SPCX (USD) missing from Active Holdings table
//
// Reproduces the real database state (60 transactions).
// After FIFO matching, purchases with remaining > 0 should appear in the
// Active Holdings table.  SPCX (id 61) had the only purchase with no
// matching sale, so it must appear.
// ---------------------------------------------------------------------------

const REAL_TRANSACTIONS: Transaction[] = [
  { id: 2,  symbol: 'GMG.AX',     transaction_type: 'purchase', date: '2025-07-25', quantity: 45,   price: 34.75,              amount: null, brokerage: null, dividends_total: 0 },
  { id: 3,  symbol: 'AX1.AX',     transaction_type: 'purchase', date: '2025-07-21', quantity: 1000, price: 1.48,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 4,  symbol: 'PWH.AX',     transaction_type: 'purchase', date: '2025-01-07', quantity: 260,  price: 7.8,                amount: null, brokerage: null, dividends_total: 0 },
  { id: 5,  symbol: 'ADH.AX',     transaction_type: 'purchase', date: '2025-04-04', quantity: 750,  price: 2.0,                amount: null, brokerage: null, dividends_total: 0 },
  { id: 6,  symbol: 'COH.AX',     transaction_type: 'purchase', date: '2025-01-07', quantity: 5,    price: 302.5,              amount: null, brokerage: null, dividends_total: 0 },
  { id: 7,  symbol: 'DMP.AX',     transaction_type: 'purchase', date: '2025-01-08', quantity: 50,   price: 28.95,              amount: null, brokerage: null, dividends_total: 0 },
  { id: 8,  symbol: 'MTO.AX',     transaction_type: 'purchase', date: '2025-11-07', quantity: 400,  price: 3.64,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 9,  symbol: 'NHF.AX',     transaction_type: 'purchase', date: '2025-01-09', quantity: 270,  price: 5.5,                amount: null, brokerage: null, dividends_total: 0 },
  { id: 10, symbol: 'SIQ.AX',     transaction_type: 'purchase', date: '2025-03-17', quantity: 200,  price: 6.95,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 11, symbol: 'SOL.AX',     transaction_type: 'purchase', date: '2026-01-02', quantity: 156,  price: 34.31,              amount: null, brokerage: null, dividends_total: 0 },
  { id: 12, symbol: 'GMG.AX',     transaction_type: 'sale',     date: '2026-05-26', quantity: 45,   price: 28.77,              amount: null, brokerage: null, dividends_total: 0 },
  { id: 13, symbol: 'AX1.AX',     transaction_type: 'sale',     date: '2026-05-25', quantity: 1000, price: 0.56,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 14, symbol: 'NDQ.AX',     transaction_type: 'purchase', date: '2025-01-02', quantity: 60,   price: 50.52,              amount: null, brokerage: null, dividends_total: 0 },
  { id: 15, symbol: 'IPG.AX',     transaction_type: 'purchase', date: '2025-01-07', quantity: 520,  price: 3.85,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 16, symbol: 'CRYP.AX',    transaction_type: 'purchase', date: '2025-01-07', quantity: 190,  price: 7.96,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 17, symbol: 'CCLD.AX',    transaction_type: 'purchase', date: '2025-01-07', quantity: 100,  price: 15.38,              amount: null, brokerage: null, dividends_total: 0 },
  { id: 18, symbol: 'MOAT.AX',    transaction_type: 'purchase', date: '2025-01-07', quantity: 12,   price: 131.5,              amount: null, brokerage: null, dividends_total: 0 },
  { id: 19, symbol: 'HACK.AX',    transaction_type: 'purchase', date: '2025-01-07', quantity: 110,  price: 14.1,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 20, symbol: 'VAS.AX',     transaction_type: 'purchase', date: '2025-01-08', quantity: 40,   price: 101.96,             amount: null, brokerage: null, dividends_total: 0 },
  { id: 21, symbol: 'VTS.AX',     transaction_type: 'purchase', date: '2025-01-08', quantity: 7,    price: 467.7,              amount: null, brokerage: null, dividends_total: 0 },
  { id: 22, symbol: 'TWE.AX',     transaction_type: 'purchase', date: '2025-01-08', quantity: 140,  price: 11.0,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 23, symbol: 'APE.AX',     transaction_type: 'purchase', date: '2025-01-08', quantity: 160,  price: 12.342,             amount: null, brokerage: null, dividends_total: 0 },
  { id: 24, symbol: 'CTD.AX',     transaction_type: 'purchase', date: '2025-01-08', quantity: 120,  price: 12.93,              amount: null, brokerage: null, dividends_total: 0 },
  { id: 25, symbol: 'VSO.AX',     transaction_type: 'purchase', date: '2025-01-09', quantity: 15,   price: 67.3,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 26, symbol: 'NCK.AX',     transaction_type: 'purchase', date: '2025-01-13', quantity: 100,  price: 14.99,              amount: null, brokerage: null, dividends_total: 0 },
  { id: 27, symbol: 'SUL.AX',     transaction_type: 'purchase', date: '2025-01-17', quantity: 100,  price: 15.2,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 28, symbol: 'SUL.AX',     transaction_type: 'purchase', date: '2025-02-28', quantity: 100,  price: 14.3,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 29, symbol: 'ELD.AX',     transaction_type: 'purchase', date: '2025-03-19', quantity: 200,  price: 6.9,                amount: null, brokerage: null, dividends_total: 0 },
  { id: 30, symbol: 'JLG.AX',     transaction_type: 'purchase', date: '2025-03-28', quantity: 700,  price: 2.25,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 31, symbol: 'SRV.AX',     transaction_type: 'purchase', date: '2025-04-04', quantity: 300,  price: 5.3,                amount: null, brokerage: null, dividends_total: 0 },
  { id: 32, symbol: 'IVV.AX',     transaction_type: 'purchase', date: '2025-04-09', quantity: 30,   price: 54.5,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 33, symbol: 'QLTY.AX',    transaction_type: 'purchase', date: '2025-04-09', quantity: 50,   price: 28.3,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 34, symbol: 'XRF.AX',     transaction_type: 'purchase', date: '2025-07-01', quantity: 850,  price: 1.78,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 35, symbol: 'SKS.AX',     transaction_type: 'purchase', date: '2025-10-15', quantity: 350,  price: 4.2,                amount: null, brokerage: null, dividends_total: 0 },
  { id: 36, symbol: 'SKS.AX',     transaction_type: 'purchase', date: '2025-11-10', quantity: 450,  price: 3.37,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 37, symbol: 'NUGG.AX',    transaction_type: 'purchase', date: '2026-02-02', quantity: 25,   price: 68.0,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 38, symbol: 'VAE.AX',     transaction_type: 'purchase', date: '2026-02-02', quantity: 20,   price: 97.5,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 39, symbol: 'ETPMPM.AX',  transaction_type: 'purchase', date: '2026-03-09', quantity: 3,    price: 471.0,              amount: null, brokerage: null, dividends_total: 0 },
  { id: 40, symbol: 'DTEC.AX',    transaction_type: 'purchase', date: '2026-03-11', quantity: 75,   price: 19.2,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 41, symbol: 'NUGG.AX',    transaction_type: 'purchase', date: '2026-04-20', quantity: 25,   price: 66.0,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 42, symbol: 'NXT.AX',     transaction_type: 'purchase', date: '2026-05-19', quantity: 100,  price: 14.5,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 43, symbol: 'JLG.AX',     transaction_type: 'sale',     date: '2025-08-01', quantity: 700,  price: 3.91,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 44, symbol: 'ELD.AX',     transaction_type: 'sale',     date: '2026-05-18', quantity: 200,  price: 6.0,                amount: null, brokerage: null, dividends_total: 0 },
  { id: 45, symbol: 'SKS.AX',     transaction_type: 'sale',     date: '2026-05-19', quantity: 800,  price: 7.88385,            amount: null, brokerage: null, dividends_total: 0 },
  { id: 46, symbol: 'APE.AX',     transaction_type: 'sale',     date: '2026-05-20', quantity: 160,  price: 21.816563,          amount: null, brokerage: null, dividends_total: 0 },
  { id: 47, symbol: 'PWH.AX',     transaction_type: 'sale',     date: '2026-05-20', quantity: 260,  price: 6.18,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 48, symbol: 'IPG.AX',     transaction_type: 'sale',     date: '2026-05-21', quantity: 520,  price: 5.67,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 49, symbol: 'SRV.AX',     transaction_type: 'sale',     date: '2026-05-21', quantity: 300,  price: 6.18,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 50, symbol: 'NCK.AX',     transaction_type: 'sale',     date: '2026-05-22', quantity: 1000, price: 1.32,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 51, symbol: 'SUL.AX',     transaction_type: 'sale',     date: '2026-05-25', quantity: 200,  price: 11.091,             amount: null, brokerage: null, dividends_total: 0 },
  { id: 52, symbol: 'XRF.AX',     transaction_type: 'sale',     date: '2026-05-26', quantity: 850,  price: 1.78,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 53, symbol: 'TWE.AX',     transaction_type: 'sale',     date: '2026-06-01', quantity: 400,  price: 4.200025,           amount: null, brokerage: null, dividends_total: 0 },
  { id: 54, symbol: 'NXT.AX',     transaction_type: 'sale',     date: '2026-06-09', quantity: 100,  price: 15.065,             amount: null, brokerage: null, dividends_total: 0 },
  { id: 55, symbol: 'IVV.AX',     transaction_type: 'sale',     date: '2026-06-12', quantity: 30,   price: 70.08,              amount: null, brokerage: null, dividends_total: 0 },
  { id: 56, symbol: 'DMP.AX',     transaction_type: 'sale',     date: '2026-06-12', quantity: 50,   price: 15.98,              amount: null, brokerage: null, dividends_total: 0 },
  { id: 57, symbol: 'COH.AX',     transaction_type: 'sale',     date: '2026-06-12', quantity: 5,    price: 103.88,             amount: null, brokerage: null, dividends_total: 0 },
  { id: 58, symbol: 'MTO.AX',     transaction_type: 'sale',     date: '2026-06-12', quantity: 400,  price: 2.44,               amount: null, brokerage: null, dividends_total: 0 },
  { id: 59, symbol: 'TSM',        transaction_type: 'purchase', date: '2026-06-09', quantity: 3,    price: 612.022028113604,   amount: null, brokerage: null, dividends_total: 0, currency: 'USD', original_price: 431.53, fx_rate: 1.4187 },
  { id: 60, symbol: 'TXG',        transaction_type: 'purchase', date: '2026-06-09', quantity: 40,   price: 43.3995566082001,   amount: null, brokerage: null, dividends_total: 0, currency: 'USD', original_price: 30.59,  fx_rate: 1.4187 },
  { id: 61, symbol: 'SPCX',       transaction_type: 'purchase', date: '2026-06-07', quantity: 6,    price: 191.66611790657,    amount: null, brokerage: null, dividends_total: -0, currency: 'USD', original_price: 135.0,  fx_rate: 1.41974902153015 },
]

describe('SPCX regression — full dataset active-holdings filter', () => {
  it('calcRemainingByLot gives SPCX id=61 a positive remaining with full real dataset', () => {
    const r = calcRemainingByLot(REAL_TRANSACTIONS)
    expect(r[61]).toBeGreaterThan(0)
    expect(r[61]).toBe(6)
  })

  it('SPCX id=61 passes the activeTransactions filter (purchase + remaining > 0)', () => {
    const r = calcRemainingByLot(REAL_TRANSACTIONS)
    const active = REAL_TRANSACTIONS.filter(
      (tx) => tx.transaction_type === 'purchase' && (r[tx.id] ?? 0) > 0
    )
    const spcx = active.find((tx) => tx.symbol === 'SPCX')
    expect(spcx).toBeDefined()
    expect(spcx?.id).toBe(61)
  })

  it('TSM and TXG (other USD purchases) also appear in active holdings', () => {
    const r = calcRemainingByLot(REAL_TRANSACTIONS)
    const active = REAL_TRANSACTIONS.filter(
      (tx) => tx.transaction_type === 'purchase' && (r[tx.id] ?? 0) > 0
    )
    const symbols = active.map((tx) => tx.symbol)
    expect(symbols).toContain('TSM')
    expect(symbols).toContain('TXG')
    expect(symbols).toContain('SPCX')
  })

  it('fully-sold AUD symbols are excluded from active holdings', () => {
    const r = calcRemainingByLot(REAL_TRANSACTIONS)
    const active = REAL_TRANSACTIONS.filter(
      (tx) => tx.transaction_type === 'purchase' && (r[tx.id] ?? 0) > 0
    )
    const symbols = active.map((tx) => tx.symbol)
    // These were all fully sold
    expect(symbols).not.toContain('GMG.AX')
    expect(symbols).not.toContain('AX1.AX')
    expect(symbols).not.toContain('COH.AX')
    expect(symbols).not.toContain('JLG.AX')
  })

  it('dividends_total of -0 on SPCX does not affect remaining quantity', () => {
    // The API returns dividends_total: -0.0 for SPCX — ensure this does not
    // corrupt the FIFO result or exclude the transaction from active holdings
    const spcxOnly = REAL_TRANSACTIONS.filter((tx) => tx.symbol === 'SPCX')
    const r = calcRemainingByLot(spcxOnly)
    expect(r[61]).toBe(6)
  })
})

describe('international stock — calcPortfolioPL', () => {
  it('active USD holding is counted in stockCount and valued in AUD', () => {
    const txs = [
      makeTx(1, 'purchase', '2026-01-15', 50, 2.244, {
        symbol: 'SPCX',
        currency: 'USD',
        original_price: 1.50,
        fx_rate: 1.496,
      }),
    ]
    // Price in map is already AUD-converted (as done by toAudPrice in the UI)
    const result = calcPortfolioPL(txs, { SPCX: 2.80 })
    expect(result.stockCount).toBe(1)
    expect(result.totalValue).toBeCloseTo(50 * 2.80)
    // P/L = 50 * (2.80 - 2.244) = 27.80 AUD
    expect(result.holdingsPL).toBeCloseTo(27.80)
  })

  it('USD and AUD stocks coexist correctly in portfolio totals', () => {
    const txs = [
      makeTx(1, 'purchase', '2026-01-15', 50, 2.244, {
        symbol: 'SPCX',
        currency: 'USD',
        original_price: 1.50,
        fx_rate: 1.496,
      }),
      makeTx(2, 'purchase', '2026-02-01', 100, 45.00, { symbol: 'CBA.AX' }),
    ]
    const result = calcPortfolioPL(txs, { SPCX: 2.80, 'CBA.AX': 48.00 })
    expect(result.stockCount).toBe(2)
    // SPCX: 50*(2.80-2.244)=27.80  CBA: 100*(48-45)=300
    expect(result.holdingsPL).toBeCloseTo(327.80)
  })
})
