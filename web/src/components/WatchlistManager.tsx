import { useState, useEffect } from 'react'
import { apiClient } from '../services/api'
import PriceChart from './PriceChart'

interface WatchlistSymbol {
  id: number
  symbol: string
  added_at: string
}

interface CurrentPrice {
  symbol: string
  price: number | null
  change: number | null
  change_percent: number | null
  volume: number | null
  last_updated: string
  error?: string
}

interface WatchlistManagerProps {
  onLoading: (loading: boolean) => void
}

export default function WatchlistManager({ onLoading }: WatchlistManagerProps) {
  const [symbols, setSymbols] = useState<WatchlistSymbol[]>([])
  const [currentPrices, setCurrentPrices] = useState<CurrentPrice[]>([])
  const [selectedSymbol, setSelectedSymbol] = useState('')
  const [newSymbol, setNewSymbol] = useState('')
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [success, setSuccess] = useState<string | null>(null)

  useEffect(() => {
    loadWatchlistData()
  }, [])

  const loadWatchlistData = async () => {
    try {
      setLoading(true)
      setError(null)
      const [symbolsData, pricesData] = await Promise.all([
        apiClient.getWatchlistSymbols(),
        apiClient.getWatchlistPrices()
      ])
      setSymbols(symbolsData)
      setCurrentPrices(pricesData)
      if (symbolsData.length && !selectedSymbol) {
        setSelectedSymbol(symbolsData[0].symbol)
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load watchlist')
    } finally {
      setLoading(false)
      onLoading(false)
    }
  }

  const handleAddSymbol = async (e: React.FormEvent) => {
    e.preventDefault()
    
    if (!newSymbol.trim()) {
      setError('Please enter a symbol')
      return
    }

    try {
      setLoading(true)
      setError(null)
      setSuccess(null)
      
      const result = await apiClient.addWatchlistSymbol(newSymbol.trim())
      const nextSymbols = [...symbols, result]
      setSymbols(nextSymbols)
      setNewSymbol('')
      if (!selectedSymbol) {
        setSelectedSymbol(result.symbol)
      }
      setSuccess(`Added ${result.symbol} to watchlist`)
      
      // Refresh prices after adding symbol
      const pricesData = await apiClient.getWatchlistPrices()
      setCurrentPrices(pricesData)
      
      // Clear success message after 3 seconds
      setTimeout(() => setSuccess(null), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to add symbol')
    } finally {
      setLoading(false)
      onLoading(false)
    }
  }

  const handleRefreshPrices = async () => {
    try {
      setLoading(true)
      setError(null)
      setSuccess(null)
      const pricesData = await apiClient.getWatchlistPrices()
      setCurrentPrices(pricesData)

      const hasPrice = pricesData.some((item) => item.price !== null)
      const errorDetails = pricesData
        .filter((item) => item.error)
        .map((item) => `${item.symbol}: ${item.error}`)
        .join(' | ')

      if (!hasPrice) {
        setError(
          errorDetails
            ? `Update Watchlist Prices failed: ${errorDetails}`
            : 'Update Watchlist Prices failed: no valid prices were returned'
        )
      } else {
        setSuccess('Watchlist prices updated')
        if (errorDetails) {
          setError(`Partial errors: ${errorDetails}`)
        }
        setTimeout(() => setSuccess(null), 2000)
      }
    } catch (err) {
      setError(
        err instanceof Error
          ? `Update Watchlist Prices failed: ${err.message}`
          : 'Update Watchlist Prices failed'
      )
    } finally {
      setLoading(false)
      onLoading(false)
    }
  }

  const handleRemoveSymbol = async (id: number, symbol: string) => {
    if (!confirm(`Remove ${symbol} from watchlist?`)) {
      return
    }

    try {
      setLoading(true)
      setError(null)
      setSuccess(null)

      const remaining = symbols.filter((s) => s.id !== id)
      await apiClient.removeWatchlistSymbol(id)
      setSymbols(remaining)
      if (selectedSymbol === symbol) {
        setSelectedSymbol(remaining[0]?.symbol || '')
      }
      setSuccess(`Removed ${symbol} from watchlist`)

      const pricesData = await apiClient.getWatchlistPrices()
      setCurrentPrices(pricesData)
      setTimeout(() => setSuccess(null), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to remove symbol')
    } finally {
      setLoading(false)
      onLoading(false)
    }
  }

  return (
    <div className="watchlist-manager">
      <div className="manager-card">
        <h2>Add Stock Symbol</h2>
        <form onSubmit={handleAddSymbol} className="add-symbol-form">
          <div className="form-group">
            <input
              type="text"
              value={newSymbol}
              onChange={(e) => setNewSymbol(e.target.value.toUpperCase())}
              placeholder="Enter ASX symbol (e.g., CBA, BHP)"
              className="symbol-input"
              disabled={loading}
              maxLength={6}
            />
            <button 
              type="submit" 
              className="btn btn-primary" 
              disabled={loading || !newSymbol.trim()}
            >
              {loading ? 'Adding...' : 'Add Symbol'}
            </button>
          </div>
        </form>
      </div>

      {error && (
        <div className="alert alert-error">
          ❌ {error}
        </div>
      )}

      {success && (
        <div className="alert alert-success">
          ✓ {success}
        </div>
      )}

      <div className="manager-card">
        <div className="card-header">
          <h2>Active Watchlist ({symbols.length})</h2>
          <button
            onClick={handleRefreshPrices}
            className="btn btn-outline"
            disabled={loading || symbols.length === 0}
            title="Update current prices for all watchlist stocks"
          >
            🔄 Update Watchlist Prices
          </button>
        </div>
        
        {loading && symbols.length === 0 ? (
          <p className="loading-text">Loading watchlist...</p>
        ) : symbols.length === 0 ? (
          <p className="empty-text">No symbols in watchlist yet. Add one to get started.</p>
        ) : (
          <ul className="symbols-list">
            {symbols.map(({ id, symbol, added_at }) => {
              const priceData = currentPrices.find(p => p.symbol === symbol)
              return (
                <li key={id} className="symbol-item">
                  <div className="symbol-info">
                    <span className="symbol-name">{symbol}</span>
                    <span className="symbol-date">
                      Added: {new Date(added_at).toLocaleDateString()}
                    </span>
                  </div>
                  <div className="price-info">
                    {priceData ? (
                      <div className="price-details">
                        <div className="current-price">
                          {priceData.price ? (
                            <span className="price-value">${priceData.price.toFixed(2)}</span>
                          ) : (
                            <span className="price-unavailable">—</span>
                          )}
                        </div>
                        {priceData.change !== null && priceData.change_percent !== null && (
                          <div className={`price-change ${priceData.change >= 0 ? 'positive' : 'negative'}`}>
                            <span className="change-value">
                              {priceData.change >= 0 ? '+' : ''}{priceData.change.toFixed(2)}
                            </span>
                            <span className="change-percent">
                              ({priceData.change >= 0 ? '+' : ''}{priceData.change_percent.toFixed(2)}%)
                            </span>
                          </div>
                        )}
                        {priceData.volume && (
                          <div className="volume">
                            Vol: {priceData.volume.toLocaleString()}
                          </div>
                        )}
                      </div>
                    ) : (
                      <div className="price-loading">Loading...</div>
                    )}
                  </div>
                  <button
                    onClick={() => handleRemoveSymbol(id, symbol)}
                    className="btn btn-danger btn-small"
                    disabled={loading}
                    title="Remove from watchlist"
                  >
                    Remove
                  </button>
                </li>
              )
            })}
          </ul>
        )}
      </div>

      {symbols.length > 0 && (
        <div className="manager-card chart-card">
          <div className="card-header">
            <h2>150-Day SMA Chart</h2>
            <div className="chart-select">
              <label htmlFor="chart-symbol">Symbol</label>
              <select
                id="chart-symbol"
                value={selectedSymbol}
                onChange={(e) => setSelectedSymbol(e.target.value)}
                disabled={loading}
              >
                {symbols.map((symbol) => (
                  <option key={symbol.id} value={symbol.symbol}>
                    {symbol.symbol}
                  </option>
                ))}
              </select>
            </div>
          </div>
          <PriceChart symbol={selectedSymbol} onLoading={onLoading} />
        </div>
      )}

      <div className="manager-card info-card">
        <h3>ℹ️ How It Works</h3>
        <ul>
          <li>Symbols added here are fetched every 10-15 minutes (configurable)</li>
          <li>Use the Update Watchlist Prices button to refresh all watchlist quotes immediately</li>
          <li>Prices are stored in a separate watchlist table for intraday tracking</li>
          <li>Changes take effect on the next fetch cycle</li>
          <li>Use ASX stock symbols (e.g., CBA for Commonwealth Bank, BHP for BHP Group)</li>
        </ul>
      </div>
    </div>
  )
}
