import { useEffect, useState } from 'react'
import { apiClient, type SoldEntry } from '../services/api'

// Thin client: all FIFO/P&L math for sold positions is computed by the API
// server (GET /api/portfolio/sold) so every client shows identical numbers.

export default function SoldStocks({ onLoading, holdingsVersion }: { onLoading: (loading: boolean) => void; holdingsVersion?: number }) {
  const [entries, setEntries] = useState<SoldEntry[]>([])
  const [totalRealisedPL, setTotalRealisedPL] = useState(0)
  const [totalRealisedCost, setTotalRealisedCost] = useState(0)
  const [loading, setLoading] = useState(true)
  const [refreshing, setRefreshing] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [success, setSuccess] = useState<string | null>(null)

  const loadSold = async () => {
    const data = await apiClient.getPortfolioSold()
    setEntries(data.entries)
    setTotalRealisedPL(data.total_realised_pl)
    setTotalRealisedCost(data.total_cost)
  }

  useEffect(() => {
    const load = async () => {
      try {
        setLoading(true)
        setError(null)
        onLoading(true)
        await loadSold()
      } catch (err) {
        setError(err instanceof Error ? err.message : 'Failed to load sold stocks')
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
      await loadSold()
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

  return (
    <div className="sold-stocks">
      <div className="manager-card">
        <div className="card-header">
          <h2>Sold Stocks</h2>
          <div style={{ display: 'flex', alignItems: 'center', gap: 16 }}>
            {entries.length > 0 && (
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
        ) : entries.length === 0 ? (
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
                {entries.map((item, i) => (
                  <tr key={i}>
                    <td><strong>{item.symbol}</strong></td>
                    <td>{new Date(item.date).toLocaleDateString()}</td>
                    <td>{item.quantity.toFixed(2)}</td>
                    <td>${item.avg_purchase_price.toFixed(2)}</td>
                    <td>${item.sale_price.toFixed(2)}</td>
                    <td>{item.days_held} days</td>
                    <td>{item.brokerage > 0 ? `$${item.brokerage.toFixed(2)}` : '—'}</td>
                    <td>{item.dividends > 0 ? `$${item.dividends.toFixed(2)}` : '—'}</td>
                    <td style={{ color: item.realised_pl >= 0 ? '#4caf50' : '#f44336', fontWeight: 600 }}>
                      {item.realised_pl >= 0 ? '+' : '−'}${Math.abs(item.realised_pl).toFixed(2)}
                      {item.avg_purchase_price > 0 && (
                        <span style={{ fontWeight: 400, marginLeft: 4 }}>
                          ({item.realised_pl >= 0 ? '+' : ''}{((item.realised_pl / (item.avg_purchase_price * item.quantity)) * 100).toFixed(1)}%)
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
