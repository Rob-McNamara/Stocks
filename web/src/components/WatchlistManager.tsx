import { useState, useEffect } from 'react'
import { apiClient } from '../services/api'
import { calculateSMA, getLatestSMA } from '../utils/sma'
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
  sma50?: number | null
  sma150?: number | null
  volumeChangePct?: number | null
  error?: string
}

interface WatchlistManagerProps {
  onLoading: (loading: boolean) => void
}

export default function WatchlistManager({ onLoading }: WatchlistManagerProps) {
  const [symbols, setSymbols] = useState<WatchlistSymbol[]>([])
  const [currentPrices, setCurrentPrices] = useState<CurrentPrice[]>([])
  const [symbolInfo, setSymbolInfo] = useState<Record<string, { instrument_type: string | null; long_name: string | null; currency: string | null }>>({})
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
      const [symbolsData, pricesData, infoData] = await Promise.all([
        apiClient.getWatchlistSymbols(),
        apiClient.getWatchlistPrices(),
        apiClient.getSymbolInfo(),
      ])
      const infoMap: Record<string, { instrument_type: string | null; long_name: string | null; currency: string | null }> = {}
      infoData.forEach((i) => { infoMap[i.symbol] = { instrument_type: i.instrument_type, long_name: i.long_name, currency: i.currency } })
      setSymbolInfo(infoMap)
      
      // Fetch and calculate SMA for each symbol
      const pricesWithSMA = await Promise.all(
        pricesData.map(async (price) => {
          try {
            const history = await apiClient.getPriceHistory(price.symbol, 300)
            const sma50 = getLatestSMA(calculateSMA(history, 50))
            const sma150 = getLatestSMA(calculateSMA(history, 150))
            const volPoints = history.filter((p) => p.volume !== null && p.volume > 0).slice(-10)
            const last5 = volPoints.slice(-5)
            const prev5 = volPoints.slice(-10, -5)
            let volumeChangePct: number | null = null
            if (last5.length === 5 && prev5.length === 5) {
              const avg = (pts: typeof volPoints) => pts.reduce((s, p) => s + p.volume!, 0) / pts.length
              const prevAvg = avg(prev5)
              if (prevAvg > 0) volumeChangePct = ((avg(last5) - prevAvg) / prevAvg) * 100
            }
            return { ...price, sma50, sma150, volumeChangePct }
          } catch (err) {
            // If SMA calculation fails, just return price without SMA
            return price
          }
        })
      )
      
      setSymbols(symbolsData)
      setCurrentPrices(pricesWithSMA)
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
      const pricesWithSMA = await Promise.all(
        pricesData.map(async (price) => {
          try {
            const history = await apiClient.getPriceHistory(price.symbol, 300)
            const sma50 = getLatestSMA(calculateSMA(history, 50))
            const sma150 = getLatestSMA(calculateSMA(history, 150))
            const volPoints = history.filter((p) => p.volume !== null && p.volume > 0).slice(-10)
            const last5 = volPoints.slice(-5)
            const prev5 = volPoints.slice(-10, -5)
            let volumeChangePct: number | null = null
            if (last5.length === 5 && prev5.length === 5) {
              const avg = (pts: typeof volPoints) => pts.reduce((s, p) => s + p.volume!, 0) / pts.length
              const prevAvg = avg(prev5)
              if (prevAvg > 0) volumeChangePct = ((avg(last5) - prevAvg) / prevAvg) * 100
            }
            return { ...price, sma50, sma150, volumeChangePct }
          } catch (err) {
            return price
          }
        })
      )
      setCurrentPrices(pricesWithSMA)
      
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
      
      // Fetch and calculate SMA for each symbol
      const pricesWithSMA = await Promise.all(
        pricesData.map(async (price) => {
          try {
            const history = await apiClient.getPriceHistory(price.symbol, 300)
            const sma50 = getLatestSMA(calculateSMA(history, 50))
            const sma150 = getLatestSMA(calculateSMA(history, 150))
            const volPoints = history.filter((p) => p.volume !== null && p.volume > 0).slice(-10)
            const last5 = volPoints.slice(-5)
            const prev5 = volPoints.slice(-10, -5)
            let volumeChangePct: number | null = null
            if (last5.length === 5 && prev5.length === 5) {
              const avg = (pts: typeof volPoints) => pts.reduce((s, p) => s + p.volume!, 0) / pts.length
              const prevAvg = avg(prev5)
              if (prevAvg > 0) volumeChangePct = ((avg(last5) - prevAvg) / prevAvg) * 100
            }
            return { ...price, sma50, sma150, volumeChangePct }
          } catch (err) {
            return price
          }
        })
      )
      
      setCurrentPrices(pricesWithSMA)

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
              placeholder="Symbol (e.g. BHP.AX, AAPL)"
              className="symbol-input"
              disabled={loading}
              maxLength={12}
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
        ) : (() => {
          const isInternational = (sym: string) => {
            const cur = symbolInfo[sym]?.currency?.toUpperCase()
            if (cur) return cur !== 'AUD'
            // Fall back to suffix: ASX stocks always end with .AX
            return !sym.endsWith('.AX')
          }
          const renderItem = ({ id, symbol, added_at }: { id: number; symbol: string; added_at: string }) => {
            const priceData = currentPrices.find(p => p.symbol === symbol)
            const symCurrency = symbolInfo[symbol]?.currency?.toUpperCase()
            return (
              <li key={id} className={`symbol-item${selectedSymbol === symbol ? ' symbol-item-active' : ''}`} onClick={() => setSelectedSymbol(symbol)} style={{ cursor: 'pointer' }}>
                <div className="symbol-info">
                  <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                    <span className="symbol-name">{symbol}</span>
                    {symbolInfo[symbol]?.instrument_type && (
                      <span style={{ fontSize: 10, fontWeight: 600, padding: '1px 5px', borderRadius: 4, background: symbolInfo[symbol].instrument_type === 'ETF' ? '#e3f2fd' : '#f3e5f5', color: symbolInfo[symbol].instrument_type === 'ETF' ? '#1565c0' : '#6a1b9a' }}>
                        {symbolInfo[symbol].instrument_type}
                      </span>
                    )}
                    {(symCurrency && symCurrency !== 'AUD')
                      ? (
                        <span style={{ fontSize: 10, fontWeight: 600, padding: '1px 5px', borderRadius: 4, background: '#fff3e0', color: '#e65100' }}>
                          {symCurrency}
                        </span>
                      )
                      : (!symCurrency && !symbol.endsWith('.AX')) && (
                        <span style={{ fontSize: 10, fontWeight: 600, padding: '1px 5px', borderRadius: 4, background: '#fff3e0', color: '#e65100' }}>
                          Intl
                        </span>
                      )}
                  </div>
                  {symbolInfo[symbol]?.long_name && (
                    <div style={{ fontSize: 11, color: '#888' }}>{symbolInfo[symbol].long_name}</div>
                  )}
                  <span className="symbol-date">
                    Added: {new Date(added_at).toLocaleDateString()}
                  </span>
                </div>
                <div className="price-info">
                  {priceData ? (
                    <div className="price-details">
                      <div style={{ display: 'flex', alignItems: 'baseline', gap: 8, flexWrap: 'wrap' }}>
                        {priceData.price ? (
                          <span className="price-value">${priceData.price.toFixed(2)}</span>
                        ) : (
                          <span className="price-unavailable">—</span>
                        )}
                        {priceData.change !== null && priceData.change_percent !== null && (
                          <span className={`price-change ${priceData.change >= 0 ? 'positive' : 'negative'}`} style={{ fontSize: 12 }}>
                            {priceData.change >= 0 ? '+' : ''}{priceData.change.toFixed(2)}
                            {' '}({priceData.change >= 0 ? '+' : ''}{priceData.change_percent.toFixed(2)}%)
                          </span>
                        )}
                      </div>
                      {priceData.volume && (
                        <div className="volume">
                          Vol: {priceData.volume.toLocaleString()}
                        </div>
                      )}
                      {(priceData.sma50 != null || priceData.sma150 != null) && (
                        <div style={{ display: 'flex', gap: 10, flexWrap: 'wrap' }}>
                          {priceData.sma50 != null && (
                            <span className={`sma-value ${priceData.price !== null && priceData.sma50 > priceData.price ? 'sma-above-price' : ''}`}>
                              50SMA: ${priceData.sma50.toFixed(2)}
                            </span>
                          )}
                          {priceData.sma150 != null && (
                            <span className={`sma-value ${priceData.price !== null && priceData.sma150 > priceData.price ? 'sma-above-price' : ''}`}>
                              150SMA: ${priceData.sma150.toFixed(2)}
                            </span>
                          )}
                        </div>
                      )}
                      {priceData.volumeChangePct !== undefined && priceData.volumeChangePct !== null && (
                        <div style={{ color: priceData.volumeChangePct < 0 ? '#f44336' : '#888', fontSize: 12 }}>
                          Vol (5d vs prev 5d): {priceData.volumeChangePct >= 0 ? '+' : ''}{priceData.volumeChangePct.toFixed(1)}%
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
          }

          const local = symbols.filter(s => !isInternational(s.symbol))
          const intl = symbols.filter(s => isInternational(s.symbol))

          return (
            <>
              {local.length > 0 && (
                <>
                  {intl.length > 0 && (
                    <h3 style={{ margin: '0 0 8px', fontSize: 14, color: '#555', fontWeight: 600 }}>Local</h3>
                  )}
                  <ul className="symbols-list">{local.map(renderItem)}</ul>
                </>
              )}
              {intl.length > 0 && (
                <>
                  <h3 style={{ margin: '16px 0 8px', fontSize: 14, color: '#555', fontWeight: 600 }}>International</h3>
                  <ul className="symbols-list">{intl.map(renderItem)}</ul>
                </>
              )}
            </>
          )
        })()}
      </div>

      {symbols.length > 0 && (
        <div className="manager-card chart-card">
          <div className="card-header">
            <h2>Simple Moving Average Chart</h2>
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
          <PriceChart
            symbol={selectedSymbol}
            currency={symbolInfo[selectedSymbol]?.currency?.toUpperCase() ?? 'AUD'}
            onLoading={onLoading}
          />
        </div>
      )}

      <div className="manager-card info-card">
        <h3>ℹ️ How It Works</h3>
        <ul>
          <li>Symbols added here are fetched every 10-15 minutes (configurable)</li>
          <li>Use the Update Watchlist Prices button to refresh all watchlist quotes immediately</li>
          <li>Prices are stored in a separate watchlist table for intraday tracking</li>
          <li>Changes take effect on the next fetch cycle</li>
          <li>Use full symbols: ASX stocks need <code>.AX</code> suffix (e.g. BHP.AX), US stocks use plain ticker (e.g. AAPL, MSFT). US prices are shown converted to AUD.</li>
        </ul>
      </div>
    </div>
  )
}
