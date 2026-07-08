export interface Transaction {
  id: number
  symbol: string
  transaction_type: 'purchase' | 'sale' | 'dividend'
  date: string
  quantity: number | null
  /** Always stored in AUD, even for international stocks */
  price: number | null
  amount: number | null
  brokerage: number | null
  dividends_total: number
  /** ISO currency code of the stock's native currency, e.g. 'USD'. Defaults to 'AUD'. */
  currency?: string
  /** Price in the stock's native currency before FX conversion */
  original_price?: number | null
  /** Exchange rate used at transaction time (native → AUD) */
  fx_rate?: number | null
}

export interface FifoLot {
  quantity: number
  price: number
}

export interface SymbolSummary {
  symbol: string
  /** Remaining lots after all sales */
  lots: FifoLot[]
  /** Remaining shares (sum of lots) */
  remainingShares: number
  /** Cost basis of remaining shares */
  remainingCost: number
  /** Total quantity sold across all sale transactions */
  totalSoldQty: number
  /** Realised P/L from all sales (proceeds - cost basis - brokerage), excluding dividends */
  realisedPL: number
  /** Dividend total from dividends_total field (pre-calculated by API) */
  dividendsTotal: number
}

/** Sort transactions chronologically, then by id for same-day stability */
export function sortTransactions<T extends { date: string; id: number }>(txs: T[]): T[] {
  return [...txs].sort((a, b) => a.date.localeCompare(b.date) || a.id - b.id)
}

/**
 * Apply a FIFO sale against a mutable lot queue.
 * Mutates lots in-place. Returns the cost basis consumed.
 */
export function applyFifoSale(lots: FifoLot[], quantity: number): number {
  let remaining = quantity
  let costBasis = 0
  while (remaining > 0 && lots.length > 0) {
    const used = Math.min(remaining, lots[0].quantity)
    costBasis += used * lots[0].price
    lots[0].quantity -= used
    remaining -= used
    if (lots[0].quantity <= 0) lots.shift()
  }
  return costBasis
}

/**
 * Calculate FIFO summary for a single symbol's transactions.
 * Dividend total is read from dividends_total (pre-computed by the API from dividend_events).
 */
export function calcSymbolSummary(txs: Transaction[]): SymbolSummary {
  const sorted = sortTransactions(txs)
  const symbol = sorted[0]?.symbol ?? ''

  const lots: FifoLot[] = []
  let realisedPL = 0
  let totalSoldQty = 0
  let dividendsTotal = 0

  for (const tx of sorted) {
    if (tx.transaction_type === 'purchase' && tx.quantity != null && tx.price != null) {
      lots.push({ quantity: tx.quantity, price: tx.price })
    } else if (tx.transaction_type === 'sale' && tx.quantity != null && tx.price != null) {
      const costBasis = applyFifoSale(lots, tx.quantity)
      realisedPL += tx.quantity * tx.price - (tx.brokerage ?? 0) - costBasis
      totalSoldQty += tx.quantity
    }
    if (tx.dividends_total > 0) dividendsTotal = tx.dividends_total
  }

  const remainingShares = lots.reduce((s, l) => s + l.quantity, 0)
  const remainingCost = lots.reduce((s, l) => s + l.quantity * l.price, 0)

  return { symbol, lots, remainingShares, remainingCost, totalSoldQty, realisedPL, dividendsTotal }
}

/**
 * Calculate portfolio-level P/L matching what Holdings + Sold Stocks screens show combined:
 * - Active symbols: currentValue - remainingCost + allDividends
 * - All sale transactions: proceeds - costBasis + proportional dividends
 */
export function calcPortfolioPL(
  transactions: Transaction[],
  priceMap: Record<string, number | null>
): { holdingsPL: number; soldPL: number; totalPL: number; totalValue: number; stockCount: number } {
  const bySymbol: Record<string, Transaction[]> = {}
  for (const tx of transactions) {
    if (!bySymbol[tx.symbol]) bySymbol[tx.symbol] = []
    bySymbol[tx.symbol].push(tx)
  }

  let holdingsPL = 0
  let soldPL = 0
  let totalValue = 0
  let stockCount = 0

  for (const [symbol, txs] of Object.entries(bySymbol)) {
    const sorted = sortTransactions(txs)

    let dividends = 0
    let dividendsFromTotal = 0
    for (const tx of sorted) {
      if (tx.dividends_total > 0) dividendsFromTotal = tx.dividends_total
      else if (tx.transaction_type === 'dividend' && tx.amount) dividends += tx.amount
    }
    const symbolDividends = dividendsFromTotal > 0 ? dividendsFromTotal : dividends

    const totalSoldQty = sorted.reduce((s, tx) =>
      tx.transaction_type === 'sale' && tx.quantity ? s + tx.quantity : s, 0)

    const lots: FifoLot[] = []
    let symbolSoldPL = 0

    for (const tx of sorted) {
      if (tx.transaction_type === 'purchase' && tx.quantity != null && tx.price != null) {
        lots.push({ quantity: tx.quantity, price: tx.price })
      } else if (tx.transaction_type === 'sale' && tx.quantity != null && tx.price != null) {
        const costBasis = applyFifoSale(lots, tx.quantity)
        symbolSoldPL += tx.quantity * tx.price - (tx.brokerage ?? 0) - costBasis
      }
    }

    const remainingShares = lots.reduce((s, l) => s + l.quantity, 0)
    const remainingCost = lots.reduce((s, l) => s + l.quantity * l.price, 0)

    // Count each dividend dollar exactly once: it stays on the holdings side
    // while any shares remain, and moves to the sold side once the position
    // is fully closed.
    if (remainingShares > 0) {
      stockCount++
      const price = priceMap[symbol] ?? null
      const currentValue = price ? remainingShares * price : 0
      if (price) totalValue += currentValue
      holdingsPL += currentValue - remainingCost + symbolDividends
    } else if (totalSoldQty > 0) {
      symbolSoldPL += symbolDividends
    }

    soldPL += symbolSoldPL
  }

  return { holdingsPL, soldPL, totalPL: holdingsPL + soldPL, totalValue, stockCount }
}

/**
 * For each purchase transaction, compute how many of its shares remain unsold
 * after FIFO matching against all subsequent sales.
 *
 * Returns a map of { [transaction.id]: remainingQuantity }.
 * Only purchase transactions appear as keys. A value of 0 means fully consumed by sales.
 *
 * This drives the "Active Holdings" table — a transaction is shown if its remaining > 0.
 * International stocks (currency !== 'AUD') are handled identically; `price` is always AUD.
 */
export function calcRemainingByLot(transactions: Transaction[]): Record<number, number> {
  const bySymbol: Record<string, Transaction[]> = {}
  for (const tx of transactions) {
    if (!bySymbol[tx.symbol]) bySymbol[tx.symbol] = []
    bySymbol[tx.symbol].push(tx)
  }

  const result: Record<number, number> = {}

  for (const group of Object.values(bySymbol)) {
    const sorted = sortTransactions(group)
    const lots: Array<{ id: number; quantity: number }> = []

    for (const tx of sorted) {
      if (tx.transaction_type === 'purchase' && tx.quantity != null) {
        lots.push({ id: tx.id, quantity: tx.quantity })
        result[tx.id] = tx.quantity
      } else if (tx.transaction_type === 'sale' && tx.quantity != null) {
        let remaining = tx.quantity
        while (remaining > 0 && lots.length > 0) {
          const lot = lots[0]
          const used = Math.min(remaining, lot.quantity)
          lot.quantity -= used
          result[lot.id] = lot.quantity
          remaining -= used
          if (lot.quantity <= 0) lots.shift()
        }
      }
    }
  }

  return result
}
