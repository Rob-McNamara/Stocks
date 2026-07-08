import { useEffect, useMemo, useState } from 'react'
import { apiClient } from '../services/api'

interface HoldingTransaction {
  id: number
  symbol: string
  transaction_type: 'purchase' | 'sale' | 'dividend'
  date: string
  quantity: number | null
  price: number | null
  brokerage: number | null
  notes: string | null
  created_at: string
  amount: number | null
  dividends_total: number
}

interface SoldEntry {
  symbol: string
  date: string
  quantity: number
  avgPurchasePrice: number
  salePrice: number
  brokerage: number
  dividends: number
  daysHeld: number
  realisedPL: number
}

export default function SoldStocks({ onLoading, holdingsVersion }: { onLoading: (loading: boolean) => void; holdingsVersion?: number }) {
  const [transactions, setTransactions] = useState<HoldingTransaction[]>([])
  const [loading, setLoading] = useState(true)
  const [refreshing, setRefreshing] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [success, setSuccess] = useState<string | null>(null)

  useEffect(() => {
    const load = async () => {
      try {
        setLoading(true)
        setError(null)
        onLoading(true)
        const data = await apiClient.getHoldings()
        setTransactions(data)
      } catch (err) {
        setError(err instanceof Error ? err.message : 'Failed to load transactions')
      } finally {
        setLoading(false)
        onLoading(false)
      }
    }
    load()
  }, [holdingsVersion])

  const handleRefreshDividends = async () => {
    try {
      setRefreshing(true)
      setError(null)
      setSuccess(null)
      const result = await apiClient.refreshSoldDividends()
      const data = await apiClient.getHoldings()
      setTransactions(data)
      const msg = result.updated > 0
        ? `Dividends updated for ${result.updated} sold symbol${result.updated !== 1 ? 's' : ''}`
        : 'No sold symbols with dividend data found'
      setSuccess(msg)
      if (result.errors.length > 0) setError(`Errors: ${result.errors.join(' | ')}`)
      setTimeout(() => setSuccess(null), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to refresh dividends')
    } finally {
      setRefreshing(false)
    }
  }

  const soldStocks = useMemo((): SoldEntry[] => {
    const bySymbol: Record<string, HoldingTransaction[]> = {}
    const sorted = [...transactions].sort((a, b) => a.date.localeCompare(b.date) || a.id - b.id)
    sorted.forEach((tx) => {
      if (!bySymbol[tx.symbol]) bySymbol[tx.symbol] = []
      bySymbol[tx.symbol].push(tx)
    })

    const results: SoldEntry[] = []

    Object.entries(bySymbol).forEach(([, txs]) => {
      // Collect total dividends for this symbol
      let totalDividends = 0
      let dividendsFromTotal = 0
      txs.forEach((tx) => {
        if (tx.dividends_total > 0) dividendsFromTotal = tx.dividends_total
        else if (tx.transaction_type === 'dividend' && tx.amount) totalDividends += tx.amount
      })
      const symbolDividends = dividendsFromTotal > 0 ? dividendsFromTotal : totalDividends

      // Collect total sold quantity to distribute dividends proportionally
      const totalSoldQty = txs.reduce((sum, tx) =>
        tx.transaction_type === 'sale' && tx.quantity ? sum + tx.quantity : sum, 0)

      const lots: Array<{ quantity: number; costPerShare: number; date: string }> = []
      const sales: SoldEntry[] = []

      txs.forEach((tx) => {
        if (tx.transaction_type === 'purchase' && tx.quantity && tx.price) {
          lots.push({ quantity: tx.quantity, costPerShare: tx.price, date: tx.date })
        } else if (tx.transaction_type === 'sale' && tx.quantity && tx.price) {
          let remaining = tx.quantity
          let costBasis = 0
          let earliestPurchaseDate = tx.date
          const lotsClone = lots.map((l) => ({ ...l }))
          while (remaining > 0 && lotsClone.length > 0) {
            const lot = lotsClone[0]
            const used = Math.min(remaining, lot.quantity)
            if (lot.date < earliestPurchaseDate) earliestPurchaseDate = lot.date
            costBasis += used * lot.costPerShare
            remaining -= used
            lot.quantity -= used
            if (lot.quantity <= 0) lotsClone.shift()
          }
          lots.length = 0
          lots.push(...lotsClone)
          const brokerage = tx.brokerage ?? 0
          const saleProceeds = tx.quantity * tx.price - brokerage - costBasis
          const daysHeld = Math.round(
            (new Date(tx.date).getTime() - new Date(earliestPurchaseDate).getTime()) / 86400000
          )
          sales.push({
            symbol: tx.symbol,
            date: tx.date,
            quantity: tx.quantity,
            avgPurchasePrice: tx.quantity > 0 ? costBasis / tx.quantity : 0,
            salePrice: tx.price,
            brokerage,
            dividends: 0,
            daysHeld,
            realisedPL: saleProceeds,
          })
        }
      })

      // Dividends count on the sold side only once the position is fully
      // closed (while shares remain they belong to the Holdings screen).
      // Distribute them across the sales proportionally by quantity.
      const remainingShares = lots.reduce((s, l) => s + l.quantity, 0)
      if (remainingShares === 0 && totalSoldQty > 0 && symbolDividends > 0) {
        sales.forEach((sale) => {
          const share = (sale.quantity / totalSoldQty) * symbolDividends
          sale.dividends = share
          sale.realisedPL += share
        })
      }

      results.push(...sales)
    })

    return results.sort((a, b) => b.date.localeCompare(a.date))
  }, [transactions])

  const totalRealisedPL = useMemo(
    () => soldStocks.reduce((sum, item) => sum + item.realisedPL, 0),
    [soldStocks]
  )
  const totalRealisedCost = useMemo(
    () => soldStocks.reduce((sum, item) => sum + item.avgPurchasePrice * item.quantity, 0),
    [soldStocks]
  )

  return (
    <div className="sold-stocks">
      <div className="manager-card">
        <div className="card-header">
          <h2>Sold Stocks</h2>
          <div style={{ display: 'flex', alignItems: 'center', gap: 16 }}>
            {soldStocks.length > 0 && (
              <span style={{ fontWeight: 600, fontSize: 15, color: totalRealisedPL >= 0 ? '#4caf50' : '#f44336' }}>
                Total Realised P/L: {totalRealisedPL >= 0 ? '+' : '−'}${Math.abs(totalRealisedPL).toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
                {totalRealisedCost > 0 && (
                  <span style={{ fontWeight: 400, marginLeft: 4 }}>
                    ({totalRealisedPL >= 0 ? '+' : ''}{((totalRealisedPL / totalRealisedCost) * 100).toFixed(1)}%)
                  </span>
                )}
              </span>
            )}
            <button
              onClick={handleRefreshDividends}
              className="btn btn-outline"
              disabled={refreshing || loading}
              title="Fetch dividend history for sold stocks from Yahoo Finance"
            >
              {refreshing ? 'Refreshing...' : '🔄 Refresh Dividends'}
            </button>
          </div>
        </div>

        {success && <div className="alert alert-success">✓ {success}</div>}
        {error && <div className="alert alert-error">❌ {error}</div>}

        {loading ? (
          <p className="loading-text">Loading sold stocks...</p>
        ) : soldStocks.length === 0 ? (
          <p className="empty-text">No sold stocks recorded yet.</p>
        ) : (
          <div className="holdings-table-wrapper">
            <table className="holdings-table">
              <thead>
                <tr>
                  <th>Symbol</th>
                  <th>Date</th>
                  <th>Quantity</th>
                  <th>Avg Purchase Price</th>
                  <th>Sale Price</th>
                  <th>Duration Held</th>
                  <th>Brokerage</th>
                  <th>Dividends</th>
                  <th>Realised P/L</th>
                </tr>
              </thead>
              <tbody>
                {soldStocks.map((item, i) => (
                  <tr key={i}>
                    <td><strong>{item.symbol}</strong></td>
                    <td>{new Date(item.date).toLocaleDateString()}</td>
                    <td>{item.quantity.toFixed(2)}</td>
                    <td>${item.avgPurchasePrice.toFixed(2)}</td>
                    <td>${item.salePrice.toFixed(2)}</td>
                    <td>{item.daysHeld} days</td>
                    <td>{item.brokerage > 0 ? `$${item.brokerage.toFixed(2)}` : '—'}</td>
                    <td>{item.dividends > 0 ? `$${item.dividends.toFixed(2)}` : '—'}</td>
                    <td style={{ color: item.realisedPL >= 0 ? '#4caf50' : '#f44336', fontWeight: 600 }}>
                      {item.realisedPL >= 0 ? '+' : '−'}${Math.abs(item.realisedPL).toFixed(2)}
                      {item.avgPurchasePrice > 0 && (
                        <span style={{ fontWeight: 400, marginLeft: 4 }}>
                          ({item.realisedPL >= 0 ? '+' : ''}{((item.realisedPL / (item.avgPurchasePrice * item.quantity)) * 100).toFixed(1)}%)
                        </span>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  )
}
