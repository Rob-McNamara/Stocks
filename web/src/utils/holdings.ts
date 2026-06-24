interface Transaction {
  symbol: string
  transaction_type: string
  quantity: number | null
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
