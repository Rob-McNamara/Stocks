interface Transaction {
  symbol: string
  transaction_type: string
  quantity: number | null
  /** Per-share price converted to AUD. */
  price?: number | null
  /** Per-share price in the trade's own currency, set only on foreign trades. */
  original_price?: number | null
  date?: string
  id?: number
}

export function getEarliestRemainingPurchaseDate(transactions: Transaction[], symbol: string): string | null {
  const sorted = transactions
    .filter((tx) => tx.symbol === symbol)
    .sort((a, b) => (a.date ?? '').localeCompare(b.date ?? '') || (a.id ?? 0) - (b.id ?? 0))

  const lots: Array<{ date: string; quantity: number }> = []
  sorted.forEach((tx) => {
    if (tx.transaction_type === 'purchase' && tx.quantity) {
      lots.push({ date: tx.date ?? '', quantity: tx.quantity })
    } else if (tx.transaction_type === 'sale' && tx.quantity) {
      let remaining = tx.quantity
      while (remaining > 0 && lots.length > 0) {
        const used = Math.min(remaining, lots[0].quantity)
        lots[0].quantity -= used
        remaining -= used
        if (lots[0].quantity <= 0) lots.shift()
      }
    }
  })
  return lots.length > 0 ? lots[0].date : null
}

export function getActiveHoldingSymbols(transactions: Transaction[]): string[] {
  const netShares: Record<string, number> = {}
  transactions.forEach((tx) => {
    if (!netShares[tx.symbol]) netShares[tx.symbol] = 0
    if (tx.transaction_type === 'purchase' && tx.quantity) netShares[tx.symbol] += tx.quantity
    if (tx.transaction_type === 'sale' && tx.quantity) netShares[tx.symbol] -= tx.quantity
  })
  return Object.keys(netShares).filter((s) => netShares[s] > 0)
}

/**
 * Purchase lots still held, oldest first, after FIFO sales are applied.
 *
 * `getEarliestRemainingPurchaseDate` answers "when did the current position
 * start"; this answers "which buys make it up", so a position built in several
 * parcels can be marked at each one rather than at a single averaged point.
 * Lots already sold out are absent — marking a buy you no longer hold would
 * put a dot on the chart with nothing behind it.
 */
export function getRemainingPurchaseLots(
  transactions: Transaction[],
  symbol: string,
): Array<{ date: string; price: number; quantity: number }> {
  const sorted = transactions
    .filter((tx) => tx.symbol === symbol)
    .sort((a, b) => (a.date ?? '').localeCompare(b.date ?? '') || (a.id ?? 0) - (b.id ?? 0))

  const lots: Array<{ date: string; price: number; quantity: number }> = []
  sorted.forEach((tx) => {
    if (tx.transaction_type === 'purchase' && tx.quantity) {
      // Native currency, not AUD: the chart is drawn in the symbol's own
      // currency and applies the FX rate itself, so handing it an AUD figure
      // plots a USD chart against AUD prices — and doubles the error when the
      // chart is toggled to AUD. `original_price` is set only on foreign
      // trades; for an AUD trade `price` is already native.
      const nativePrice = tx.original_price ?? tx.price ?? 0
      lots.push({ date: tx.date ?? '', price: nativePrice, quantity: tx.quantity })
    } else if (tx.transaction_type === 'sale' && tx.quantity) {
      let remaining = tx.quantity
      while (remaining > 0 && lots.length > 0) {
        const used = Math.min(remaining, lots[0].quantity)
        lots[0].quantity -= used
        remaining -= used
        if (lots[0].quantity <= 0) lots.shift()
      }
    }
  })
  return lots.filter((l) => l.quantity > 0 && l.date)
}
