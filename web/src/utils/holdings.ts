interface Transaction {
  symbol: string
  transaction_type: string
  quantity: number | null
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
