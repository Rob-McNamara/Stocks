import { useEffect, useMemo, useState } from 'react'
import { apiClient } from '../services/api'
import { calculateSMA, getLatestSMA } from '../utils/sma'

interface HoldingTransaction {
  id: number
  symbol: string
  transaction_type: 'purchase' | 'sale' | 'dividend'
  date: string
  quantity: number | null
  price: number | null
  amount: number | null
  brokerage: number | null
  notes: string | null
  created_at: string
  dividends_total: number
}

interface SymbolSmaEntry {
  symbol: string
  price: number | null
  sma: number | null
  pctDiff: number | null
}

export default function Dashboard({ onLoading, holdingsVersion }: { onLoading: (loading: boolean) => void; holdingsVersion?: number }) {
  const [transactions, setTransactions] = useState<HoldingTransaction[]>([])
  const [holdingPrices, setHoldingPrices] = useState<Record<string, number | null>>({})
  const [holdingSma, setHoldingSma] = useState<Record<string, number | null>>({})
  const [watchlistEntries, setWatchlistEntries] = useState<SymbolSmaEntry[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    load()
  }, [holdingsVersion])

  const load = async () => {
    try {
      setLoading(true)
      setError(null)
      onLoading(true)

      const [txData, watchlistSymbols, watchlistPricesData, dividendData, configData] = await Promise.all([
        apiClient.getHoldings(),
        apiClient.getWatchlistSymbols(),
        apiClient.getWatchlistPrices(),
        apiClient.getDividends(),
        apiClient.getConfig(),
      ])
      setTransactions(txData)

      const holdingSymbols = Array.from(new Set(txData.map((tx) => tx.symbol)))

      const [pricesData] = await Promise.all([
        holdingSymbols.length > 0 ? apiClient.getCurrentPrices(holdingSymbols) : Promise.resolve([]),
      ])

      const priceMap: Record<string, number | null> = {}
      pricesData.forEach((p) => { priceMap[p.symbol] = p.price })

      // Apply manual prices from config (same as HoldingsManager)
      Object.entries(configData).forEach(([key, value]) => {
        if (key.startsWith('manual_price_')) {
          const symbol = key.replace('manual_price_', '')
          const parsed = parseFloat(value)
          if (!isNaN(parsed) && (priceMap[symbol] == null || priceMap[symbol] === 0)) {
            priceMap[symbol] = parsed
          }
        }
      })

      setHoldingPrices(priceMap)

      // Fetch SMA for holdings and watchlist in parallel
      const allSymbols = Array.from(new Set([...holdingSymbols, ...watchlistSymbols.map((s) => s.symbol)]))
      const smaResults = await Promise.all(
        allSymbols.map(async (sym) => {
          try {
            const history = await apiClient.getPriceHistory(sym, 300)
            const smaArray = calculateSMA(history, 150)
            return { symbol: sym, sma: getLatestSMA(smaArray) }
          } catch {
            return { symbol: sym, sma: null }
          }
        })
      )

      const smaMap: Record<string, number | null> = {}
      smaResults.forEach(({ symbol, sma }) => { smaMap[symbol] = sma })
      setHoldingSma(smaMap)

      const watchlistWithSma: SymbolSmaEntry[] = watchlistPricesData.map((p) => {
        const sma = smaMap[p.symbol] ?? null
        const pctDiff = p.price && sma ? ((p.price - sma) / sma) * 100 : null
        return { symbol: p.symbol, price: p.price, sma, pctDiff }
      })
      setWatchlistEntries(watchlistWithSma)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load dashboard')
    } finally {
      setLoading(false)
      onLoading(false)
    }
  }

  const portfolio = useMemo(() => {
    const groupedBySymbol: Record<string, HoldingTransaction[]> = {}
    transactions.forEach((tx) => {
      if (!groupedBySymbol[tx.symbol]) groupedBySymbol[tx.symbol] = []
      groupedBySymbol[tx.symbol].push(tx)
    })

    let stockCount = 0
    let totalValue = 0
    let holdingsPL = 0
    let soldPL = 0

    Object.entries(groupedBySymbol).forEach(([symbol, txs]) => {
      const sorted = [...txs].sort((a, b) => a.date.localeCompare(b.date) || a.id - b.id)

      // Collect dividends the same way as HoldingsManager / SoldStocks
      let dividends = 0
      let dividendsFromTotal = 0
      sorted.forEach((tx) => {
        if (tx.dividends_total > 0) dividendsFromTotal = tx.dividends_total
        else if (tx.transaction_type === 'dividend' && tx.amount) dividends += tx.amount
      })
      const symbolDividends = dividendsFromTotal > 0 ? dividendsFromTotal : dividends

      // Total sold qty used for proportional dividend attribution (matches SoldStocks)
      const totalSoldQty = sorted.reduce((s, tx) =>
        tx.transaction_type === 'sale' && tx.quantity ? s + tx.quantity : s, 0)

      // FIFO lots — recompute for each sale to get cost basis
      const lots: Array<{ quantity: number; price: number }> = []
      let symbolSoldPL = 0

      sorted.forEach((tx) => {
        if (tx.transaction_type === 'purchase' && tx.quantity && tx.price) {
          lots.push({ quantity: tx.quantity, price: tx.price })
        } else if (tx.transaction_type === 'sale' && tx.quantity && tx.price) {
          let remaining = tx.quantity
          let costBasis = 0
          while (remaining > 0 && lots.length > 0) {
            const used = Math.min(remaining, lots[0].quantity)
            costBasis += used * lots[0].price
            lots[0].quantity -= used
            remaining -= used
            if (lots[0].quantity <= 0) lots.shift()
          }
          const saleDividends = totalSoldQty > 0 ? (tx.quantity / totalSoldQty) * symbolDividends : 0
          symbolSoldPL += tx.quantity * tx.price - (tx.brokerage ?? 0) - costBasis + saleDividends
        }
      })

      const remainingShares = lots.reduce((s, l) => s + l.quantity, 0)
      const remainingCost = lots.reduce((s, l) => s + l.quantity * l.price, 0)

      if (remainingShares > 0) {
        // Matches HoldingsManager: unrealised P/L on remaining shares + all dividends
        stockCount++
        const price = holdingPrices[symbol]
        const currentValue = price ? remainingShares * price : 0
        if (price) totalValue += currentValue
        holdingsPL += currentValue - remainingCost + symbolDividends
      }

      // All sale transactions contribute to soldPL (matches SoldStocks which shows all sales)
      soldPL += symbolSoldPL
    })

    return { stockCount, totalValue, totalPL: holdingsPL + soldPL }
  }, [transactions, holdingPrices])

  const worstHoldings = useMemo((): SymbolSmaEntry[] => {
    const bySymbol: Record<string, number> = {}
    transactions.forEach((tx) => {
      if (!bySymbol[tx.symbol]) bySymbol[tx.symbol] = 0
      if (tx.transaction_type === 'purchase' && tx.quantity) bySymbol[tx.symbol] += tx.quantity
      if (tx.transaction_type === 'sale' && tx.quantity) bySymbol[tx.symbol] -= tx.quantity
    })

    return Object.entries(bySymbol)
      .filter(([, shares]) => shares > 0)
      .map(([symbol]) => {
        const price = holdingPrices[symbol] ?? null
        const sma = holdingSma[symbol] ?? null
        const pctDiff = price && sma ? ((price - sma) / sma) * 100 : null
        return { symbol, price, sma, pctDiff }
      })
      .filter((item): item is SymbolSmaEntry & { pctDiff: number } => item.pctDiff !== null)
      .sort((a, b) => a.pctDiff - b.pctDiff)
      .slice(0, 3)
  }, [transactions, holdingPrices, holdingSma])

  const bestWatchlist = useMemo(() => {
    return [...watchlistEntries]
      .filter((item): item is SymbolSmaEntry & { pctDiff: number } => item.pctDiff !== null)
      .sort((a, b) => b.pctDiff - a.pctDiff)
      .slice(0, 3)
  }, [watchlistEntries])

  if (loading) return <p className="loading-text">Loading dashboard...</p>
  if (error) return <div className="alert alert-error">❌ {error}</div>

  return (
    <div className="dashboard">
      <div className="dashboard-stats">
        <div className="stat-card">
          <div className="stat-label">Holdings</div>
          <div className="stat-value">{portfolio.stockCount}</div>
          <div className="stat-sub">stocks</div>
        </div>
        <div className="stat-card">
          <div className="stat-label">Total Value</div>
          <div className="stat-value">${portfolio.totalValue.toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}</div>
        </div>
        <div className="stat-card">
          <div className="stat-label">Profit / Loss</div>
          <div className={`stat-value ${portfolio.totalPL >= 0 ? 'positive' : 'negative'}`}>
            {portfolio.totalPL >= 0 ? '+' : '−'}${Math.abs(portfolio.totalPL).toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
          </div>
        </div>
      </div>

      <div className="dashboard-lists">
        <div className="manager-card">
          <h2>Worst Holdings — 150SMA</h2>
          <p className="dashboard-list-desc">Holdings trading furthest below their 150-day moving average</p>
          {worstHoldings.length === 0 ? (
            <p className="empty-text">No SMA data available for holdings.</p>
          ) : (
            <table className="holdings-table compact">
              <thead>
                <tr>
                  <th>Symbol</th>
                  <th>Price</th>
                  <th>150SMA</th>
                  <th>Difference</th>
                </tr>
              </thead>
              <tbody>
                {worstHoldings.map((item) => (
                  <tr key={item.symbol}>
                    <td><strong>{item.symbol}</strong></td>
                    <td>{item.price !== null ? `$${item.price.toFixed(2)}` : '—'}</td>
                    <td>{item.sma !== null ? `$${item.sma.toFixed(2)}` : '—'}</td>
                    <td style={{ color: '#f44336', fontWeight: 600 }}>
                      {(item.pctDiff as number).toFixed(2)}%
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>

        <div className="manager-card">
          <h2>Best Watchlist — 150SMA</h2>
          <p className="dashboard-list-desc">Watchlist stocks trading furthest above their 150-day moving average</p>
          {bestWatchlist.length === 0 ? (
            <p className="empty-text">No SMA data available for watchlist.</p>
          ) : (
            <table className="holdings-table compact">
              <thead>
                <tr>
                  <th>Symbol</th>
                  <th>Price</th>
                  <th>150SMA</th>
                  <th>Difference</th>
                </tr>
              </thead>
              <tbody>
                {bestWatchlist.map((item) => (
                  <tr key={item.symbol}>
                    <td><strong>{item.symbol}</strong></td>
                    <td>{item.price !== null ? `$${item.price.toFixed(2)}` : '—'}</td>
                    <td>{item.sma !== null ? `$${item.sma.toFixed(2)}` : '—'}</td>
                    <td style={{ color: '#4caf50', fontWeight: 600 }}>
                      +{(item.pctDiff as number).toFixed(2)}%
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </div>

    </div>
  )
}
