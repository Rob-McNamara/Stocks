import { useState, useEffect } from 'react'
import { apiClient } from '../services/api'
import { calculateSMA, crossoverStats, getLatestSMA } from '../utils/sma'
import PriceChart from './PriceChart'

interface WatchlistSymbol {
  id: number
  symbol: string
  list_name: string
  added_at: string
  notes: string | null
  custom_fields: Record<string, string>
}

interface CustomFieldDef {
  key: string
  label: string
  type: 'text' | 'number' | 'date'
}

interface CurrentPrice {
  symbol: string
  price: number | null
  change: number | null
  change_percent: number | null
  volume: number | null
  last_updated: string
  price_date?: string | null
  sma50?: number | null
  sma150?: number | null
  volumeChangePct?: number | null
  daysSince50SMA?: number | null
  volumePct50SMA?: number | null
  daysSince150SMA?: number | null
  volumePct150SMA?: number | null
  error?: string
}

interface WatchlistManagerProps {
  onLoading: (loading: boolean) => void
  initialSymbol?: string | null
  onInitialSymbolConsumed?: () => void
}


async function enrichWithSMA(pricesData: CurrentPrice[]): Promise<CurrentPrice[]> {
  return Promise.all(
    pricesData.map(async (price) => {
      try {
        const history = await apiClient.getPriceHistory(price.symbol, 300)
        const sma50Array = calculateSMA(history, 50)
        const sma150Array = calculateSMA(history, 150)
        const sma50 = getLatestSMA(sma50Array)
        const sma150 = getLatestSMA(sma150Array)
        const volPoints = history.filter((p) => p.volume !== null && p.volume > 0).slice(-10)
        const last5 = volPoints.slice(-5)
        const prev5 = volPoints.slice(-10, -5)
        let volumeChangePct: number | null = null
        if (last5.length === 5 && prev5.length === 5) {
          const avg = (pts: typeof volPoints) => pts.reduce((s, p) => s + p.volume!, 0) / pts.length
          const prevAvg = avg(prev5)
          if (prevAvg > 0) volumeChangePct = ((avg(last5) - prevAvg) / prevAvg) * 100
        }
        let daysSince50SMA: number | null = null
        let volumePct50SMA: number | null = null
        if (price.price !== null && sma50 !== null && price.price > sma50) {
          const stats = crossoverStats(history, sma50Array, price.volume)
          daysSince50SMA = stats.days
          volumePct50SMA = stats.volumePct
        }
        let daysSince150SMA: number | null = null
        let volumePct150SMA: number | null = null
        if (price.price !== null && sma150 !== null && price.price > sma150) {
          const stats = crossoverStats(history, sma150Array, price.volume)
          daysSince150SMA = stats.days
          volumePct150SMA = stats.volumePct
        }
        return { ...price, sma50, sma150, volumeChangePct, daysSince50SMA, volumePct50SMA, daysSince150SMA, volumePct150SMA }
      } catch {
        return price
      }
    })
  )
}

export default function WatchlistManager({ onLoading, initialSymbol, onInitialSymbolConsumed }: WatchlistManagerProps) {
  const [lists, setLists] = useState<string[]>(['Default'])
  const [selectedList, setSelectedList] = useState('Default')
  const [creatingList, setCreatingList] = useState(false)
  const [newListName, setNewListName] = useState('')
  const [symbols, setSymbols] = useState<WatchlistSymbol[]>([])
  const [allPrices, setAllPrices] = useState<CurrentPrice[]>([])
  const [symbolInfo, setSymbolInfo] = useState<Record<string, { instrument_type: string | null; long_name: string | null; currency: string | null }>>({})
  const [selectedSymbol, setSelectedSymbol] = useState('')
  const [newSymbol, setNewSymbol] = useState('')
  const [newSymbolNotes, setNewSymbolNotes] = useState('')
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [success, setSuccess] = useState<string | null>(null)
  const [editingSymbol, setEditingSymbol] = useState<string | null>(null)
  const [editListChecks, setEditListChecks] = useState<Record<string, boolean>>({})
  const [editNotes, setEditNotes] = useState('')
  const [customFieldDefs, setCustomFieldDefs] = useState<CustomFieldDef[]>([])
  const [newSymbolFields, setNewSymbolFields] = useState<Record<string, string>>({})
  const [editCustomFields, setEditCustomFields] = useState<Record<string, string>>({})

  useEffect(() => {
    loadWatchlistData()
  }, [])

  useEffect(() => {
    if (!initialSymbol) return
    const entry = symbols.find((s) => s.symbol === initialSymbol)
    if (entry) {
      setSelectedList(entry.list_name)
      setSelectedSymbol(initialSymbol)
      onInitialSymbolConsumed?.()
    }
  }, [initialSymbol, symbols])

  const loadWatchlistData = async () => {
    try {
      setLoading(true)
      setError(null)
      const [listsData, symbolsData, pricesData, infoData, configData] = await Promise.all([
        apiClient.getWatchlistLists(),
        apiClient.getWatchlistSymbols(),
        apiClient.getWatchlistPrices(),
        apiClient.getSymbolInfo(),
        apiClient.getConfig(),
      ])
      try {
        const defs = JSON.parse(configData['watchlist_custom_fields'] ?? '[]') as CustomFieldDef[]
        setCustomFieldDefs(defs)
      } catch { setCustomFieldDefs([]) }
      const infoMap: Record<string, { instrument_type: string | null; long_name: string | null; currency: string | null }> = {}
      infoData.forEach((i) => { infoMap[i.symbol] = { instrument_type: i.instrument_type, long_name: i.long_name, currency: i.currency } })
      setSymbolInfo(infoMap)
      setLists(listsData.length > 0 ? listsData : ['Default'])
      setSymbols(symbolsData)
      const pricesWithSMA = await enrichWithSMA(pricesData)
      setAllPrices(pricesWithSMA)
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

  const handleCreateList = () => {
    const name = newListName.trim()
    if (!name) return
    if (!lists.includes(name)) {
      setLists((prev) => [...prev, name].sort())
    }
    setSelectedList(name)
    setCreatingList(false)
    setNewListName('')
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
      const result = await apiClient.addWatchlistSymbol(newSymbol.trim(), selectedList, newSymbolNotes.trim() || undefined, newSymbolFields)
      setNewSymbol('')
      setNewSymbolNotes('')
      setNewSymbolFields({})
      if (!selectedSymbol) setSelectedSymbol(result.symbol)
      setSuccess(`Added ${result.symbol} to "${selectedList}"`)
      // Reload all symbols so notes/custom_fields are consistent across all list memberships
      const [listsData, symbolsData, pricesData] = await Promise.all([
        apiClient.getWatchlistLists(),
        apiClient.getWatchlistSymbols(),
        apiClient.getWatchlistPrices(),
      ])
      setLists(listsData)
      setSymbols(symbolsData)
      setAllPrices(await enrichWithSMA(pricesData))
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
      setAllPrices(await enrichWithSMA(pricesData))
      const hasPrice = pricesData.some((item) => item.price !== null)
      const errorDetails = pricesData.filter((item) => item.error).map((item) => `${item.symbol}: ${item.error}`).join(' | ')
      if (!hasPrice) {
        setError(errorDetails ? `Update failed: ${errorDetails}` : 'Update failed: no valid prices returned')
      } else {
        setSuccess('Watchlist prices updated')
        if (errorDetails) setError(`Partial errors: ${errorDetails}`)
        setTimeout(() => setSuccess(null), 2000)
      }
    } catch (err) {
      setError(err instanceof Error ? `Update failed: ${err.message}` : 'Update failed')
    } finally {
      setLoading(false)
      onLoading(false)
    }
  }

  const handleRemoveSymbol = async (id: number, symbol: string) => {
    if (!confirm(`Remove ${symbol} from "${selectedList}"?`)) return
    try {
      setLoading(true)
      setError(null)
      setSuccess(null)
      await apiClient.removeWatchlistSymbol(id)
      const remaining = symbols.filter((s) => s.id !== id)
      setSymbols(remaining)
      if (selectedSymbol === symbol) setSelectedSymbol(remaining[0]?.symbol || '')
      setSuccess(`Removed ${symbol} from "${selectedList}"`)
      // Refresh lists (list may now be empty and removed from DB)
      const [listsData, pricesData] = await Promise.all([
        apiClient.getWatchlistLists(),
        apiClient.getWatchlistPrices(),
      ])
      setLists(listsData.length > 0 ? listsData : ['Default'])
      setAllPrices(await enrichWithSMA(pricesData))
      setTimeout(() => setSuccess(null), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to remove symbol')
    } finally {
      setLoading(false)
      onLoading(false)
    }
  }

  const handleOpenEdit = (symbol: string) => {
    const checks: Record<string, boolean> = {}
    lists.forEach((l) => { checks[l] = symbols.some((s) => s.symbol === symbol && s.list_name === l) })
    setEditListChecks(checks)
    const existing = symbols.find((s) => s.symbol === symbol)
    setEditNotes(existing?.notes ?? '')
    setEditCustomFields(existing?.custom_fields ?? {})
    setEditingSymbol(symbol)
  }

  const handleSaveEdit = async () => {
    if (!editingSymbol) return
    try {
      setLoading(true)
      setError(null)
      const toAdd = lists.filter((l) => editListChecks[l] && !symbols.some((s) => s.symbol === editingSymbol && s.list_name === l))
      const toRemove = symbols.filter((s) => s.symbol === editingSymbol && !editListChecks[s.list_name])
      // Update notes on all existing rows for this symbol
      const existingRows = symbols.filter((s) => s.symbol === editingSymbol && editListChecks[s.list_name])
      const fields = editCustomFields
      await Promise.all([
        ...toAdd.map((l) => apiClient.addWatchlistSymbol(editingSymbol, l, editNotes.trim() || undefined, fields)),
        ...toRemove.map((s) => apiClient.removeWatchlistSymbol(s.id)),
        ...existingRows.map((s) => apiClient.updateWatchlistSymbol(s.id, editNotes.trim() || null, fields)),
      ])
      setEditingSymbol(null)
      const [listsData, symbolsData] = await Promise.all([
        apiClient.getWatchlistLists(),
        apiClient.getWatchlistSymbols(),
      ])
      setLists(listsData.length > 0 ? listsData : ['Default'])
      setSymbols(symbolsData)
      setSuccess(`Updated lists for ${editingSymbol}`)
      setTimeout(() => setSuccess(null), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to update lists')
    } finally {
      setLoading(false)
      onLoading(false)
    }
  }

  const listSymbols = symbols.filter((s) => s.list_name === selectedList)
  const listPrices = allPrices.filter((p) => listSymbols.some((s) => s.symbol === p.symbol))

  const renderItem = ({ id, symbol, added_at, notes, custom_fields }: WatchlistSymbol) => {
    const priceData = listPrices.find((p) => p.symbol === symbol)
    const symCurrency = symbolInfo[symbol]?.currency?.toUpperCase()
    return (
      <li key={id} className={`symbol-item${selectedSymbol === symbol ? ' symbol-item-active' : ''}`} onClick={() => setSelectedSymbol(symbol)} style={{ cursor: 'pointer', position: 'relative' }}>
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
          {notes && (
            <div style={{ fontSize: 11, color: '#5c6bc0', fontStyle: 'italic', marginTop: 1 }}>{notes}</div>
          )}
          {customFieldDefs.filter((def) => custom_fields[def.key]).map((def) => (
            <div key={def.key} style={{ fontSize: 11, color: '#555', marginTop: 1 }}>
              <span style={{ color: '#999' }}>{def.label}:</span> {custom_fields[def.key]}
            </div>
          ))}
          <span className="symbol-date">Added: {new Date(added_at).toLocaleDateString()}</span>
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
                <div className="volume">Vol: {priceData.volume.toLocaleString()}</div>
              )}
              {(priceData.sma50 != null || priceData.sma150 != null) && (
                <div style={{ display: 'flex', gap: 10, flexWrap: 'wrap' }}>
                  {priceData.sma50 != null && (
                    <span className={`sma-value ${priceData.price !== null && priceData.sma50 > priceData.price ? 'sma-above-price' : ''}`}>
                      50SMA: ${priceData.sma50.toFixed(2)}
                      {priceData.daysSince50SMA != null && (
                        <span style={{ marginLeft: 4, fontSize: 10, color: '#2e7d32', fontWeight: 600 }}>
                          ↑{priceData.daysSince50SMA}d
                          {priceData.volumePct50SMA != null && (
                            <span style={{ marginLeft: 3, color: priceData.volumePct50SMA >= 0 ? '#2e7d32' : '#c62828' }}>
                              (vol {priceData.volumePct50SMA >= 0 ? '+' : ''}{priceData.volumePct50SMA.toFixed(0)}%)
                            </span>
                          )}
                        </span>
                      )}
                    </span>
                  )}
                  {priceData.sma150 != null && (
                    <span className={`sma-value ${priceData.price !== null && priceData.sma150 > priceData.price ? 'sma-above-price' : ''}`}>
                      150SMA: ${priceData.sma150.toFixed(2)}
                      {priceData.daysSince150SMA != null && (
                        <span style={{ marginLeft: 4, fontSize: 10, color: '#2e7d32', fontWeight: 600 }}>
                          ↑{priceData.daysSince150SMA}d
                          {priceData.volumePct150SMA != null && (
                            <span style={{ marginLeft: 3, color: priceData.volumePct150SMA >= 0 ? '#2e7d32' : '#c62828' }}>
                              (vol {priceData.volumePct150SMA >= 0 ? '+' : ''}{priceData.volumePct150SMA.toFixed(0)}%)
                            </span>
                          )}
                        </span>
                      )}
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
        <div style={{ position: 'absolute', top: 6, right: 8, display: 'flex', gap: 4 }}>
          <button
            onClick={(e) => { e.stopPropagation(); handleOpenEdit(symbol) }}
            className="btn btn-outline btn-small"
            disabled={loading}
            title="Edit watchlists"
            style={{ padding: '2px 6px', fontSize: 13, lineHeight: 1 }}
          >
            ✏️
          </button>
          <button
            onClick={(e) => { e.stopPropagation(); handleRemoveSymbol(id, symbol) }}
            className="btn btn-danger btn-small"
            disabled={loading}
            title="Remove from watchlist"
            style={{ padding: '2px 6px', fontSize: 13, lineHeight: 1, fontWeight: 700 }}
          >
            ✕
          </button>
        </div>
      </li>
    )
  }

  const chartSymbols = symbols.filter((s) => s.list_name === selectedList)

  return (
    <div className="watchlist-manager">
      <div className="manager-card">
        <div className="card-header" style={{ marginBottom: 12 }}>
          <h2 style={{ margin: 0 }}>Watchlist</h2>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <label htmlFor="list-select" style={{ fontSize: 13, fontWeight: 500 }}>List:</label>
            <select
              id="list-select"
              value={selectedList}
              onChange={(e) => { setSelectedList(e.target.value); setSelectedSymbol('') }}
              disabled={loading}
              style={{ padding: '4px 8px', borderRadius: 4, border: '1px solid #ccc', fontSize: 13 }}
            >
              {lists.map((l) => <option key={l} value={l}>{l}</option>)}
            </select>
            {!creatingList ? (
              <button
                className="btn btn-outline"
                style={{ fontSize: 12, padding: '4px 10px' }}
                onClick={() => setCreatingList(true)}
                disabled={loading}
              >
                + New List
              </button>
            ) : (
              <div style={{ display: 'flex', gap: 4 }}>
                <input
                  autoFocus
                  type="text"
                  value={newListName}
                  onChange={(e) => setNewListName(e.target.value)}
                  onKeyDown={(e) => { if (e.key === 'Enter') handleCreateList(); if (e.key === 'Escape') { setCreatingList(false); setNewListName('') } }}
                  placeholder="List name"
                  style={{ padding: '4px 8px', borderRadius: 4, border: '1px solid #ccc', fontSize: 13, width: 130 }}
                  maxLength={40}
                />
                <button className="btn btn-primary" style={{ fontSize: 12, padding: '4px 10px' }} onClick={handleCreateList} disabled={!newListName.trim()}>Create</button>
                <button className="btn btn-outline" style={{ fontSize: 12, padding: '4px 10px' }} onClick={() => { setCreatingList(false); setNewListName('') }}>Cancel</button>
              </div>
            )}
          </div>
        </div>
        <form onSubmit={handleAddSymbol} className="add-symbol-form">
          <div className="form-group">
            <input
              type="text"
              value={newSymbol}
              onChange={(e) => setNewSymbol(e.target.value.toUpperCase())}
              placeholder={`Symbol to add to "${selectedList}"`}
              className="symbol-input"
              disabled={loading}
              maxLength={12}
            />
            <input
              type="text"
              value={newSymbolNotes}
              onChange={(e) => setNewSymbolNotes(e.target.value)}
              placeholder="Notes (optional)"
              className="symbol-input"
              disabled={loading}
              style={{ flex: 2 }}
            />
            <button type="submit" className="btn btn-primary" disabled={loading || !newSymbol.trim()}>
              {loading ? 'Adding...' : 'Add Symbol'}
            </button>
          </div>
          {customFieldDefs.length > 0 && (
            <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', marginTop: 8 }}>
              {customFieldDefs.map((def) => (
                <input
                  key={def.key}
                  type={def.type}
                  value={newSymbolFields[def.key] ?? ''}
                  onChange={(e) => setNewSymbolFields((prev) => ({ ...prev, [def.key]: e.target.value }))}
                  placeholder={def.label}
                  className="symbol-input"
                  disabled={loading}
                  style={{ flex: 1, minWidth: 120 }}
                />
              ))}
            </div>
          )}
        </form>
      </div>

      {error && <div className="alert alert-error">❌ {error}</div>}
      {success && <div className="alert alert-success">✓ {success}</div>}

      <div className="manager-card">
        <div className="card-header">
          <h2>{selectedList} ({listSymbols.length})</h2>
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
        ) : listSymbols.length === 0 ? (
          <p className="empty-text">No symbols in "{selectedList}" yet. Add one above.</p>
        ) : (
          <ul className="symbols-list">{listSymbols.map(renderItem)}</ul>
        )}
      </div>

      {chartSymbols.length > 0 && (
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
                {chartSymbols.map((s) => (
                  <option key={s.id} value={s.symbol}>{s.symbol}</option>
                ))}
              </select>
            </div>
          </div>
          <PriceChart
            symbol={selectedSymbol || chartSymbols[0]?.symbol}
            currency={symbolInfo[selectedSymbol || chartSymbols[0]?.symbol]?.currency?.toUpperCase() ?? 'AUD'}
            onLoading={onLoading}
            currentPrice={allPrices.find((p) => p.symbol === (selectedSymbol || chartSymbols[0]?.symbol))?.price ?? null}
            currentVolume={allPrices.find((p) => p.symbol === (selectedSymbol || chartSymbols[0]?.symbol))?.volume ?? null}
            currentPriceDate={allPrices.find((p) => p.symbol === (selectedSymbol || chartSymbols[0]?.symbol))?.price_date ?? null}
          />
        </div>
      )}

      {editingSymbol && (
        <div style={{ position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.4)', zIndex: 1000, display: 'flex', alignItems: 'center', justifyContent: 'center' }}
          onClick={() => setEditingSymbol(null)}>
          <div style={{ background: '#fff', borderRadius: 8, padding: 24, minWidth: 280, boxShadow: '0 4px 24px rgba(0,0,0,0.18)' }}
            onClick={(e) => e.stopPropagation()}>
            <h3 style={{ margin: '0 0 4px' }}>Edit {editingSymbol}</h3>
            <p style={{ margin: '0 0 16px', fontSize: 13, color: '#666' }}>Select which watchlists this stock appears in:</p>
            <div style={{ display: 'flex', flexDirection: 'column', gap: 10, marginBottom: 16 }}>
              {lists.map((l) => (
                <label key={l} style={{ display: 'flex', alignItems: 'center', gap: 8, cursor: 'pointer', fontSize: 14 }}>
                  <input
                    type="checkbox"
                    checked={!!editListChecks[l]}
                    onChange={(e) => setEditListChecks((prev) => ({ ...prev, [l]: e.target.checked }))}
                    style={{ width: 16, height: 16, cursor: 'pointer' }}
                  />
                  {l}
                </label>
              ))}
            </div>
            <div style={{ display: 'flex', flexDirection: 'column', gap: 4, marginBottom: customFieldDefs.length > 0 ? 12 : 20 }}>
              <label style={{ fontSize: 13, color: '#666' }}>Notes (optional)</label>
              <input
                type="text"
                value={editNotes}
                onChange={(e) => setEditNotes(e.target.value)}
                placeholder="Add a note about this stock…"
                className="symbol-input"
                style={{ width: '100%' }}
              />
            </div>
            {customFieldDefs.length > 0 && (
              <div style={{ display: 'flex', flexDirection: 'column', gap: 10, marginBottom: 20 }}>
                {customFieldDefs.map((def) => (
                  <div key={def.key} style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                    <label style={{ fontSize: 13, color: '#666' }}>{def.label}</label>
                    <input
                      type={def.type}
                      value={editCustomFields[def.key] ?? ''}
                      onChange={(e) => setEditCustomFields((prev) => ({ ...prev, [def.key]: e.target.value }))}
                      className="symbol-input"
                      style={{ width: '100%' }}
                    />
                  </div>
                ))}
              </div>
            )}
            <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
              <button className="btn btn-outline" onClick={() => setEditingSymbol(null)}>Cancel</button>
              <button className="btn btn-primary" onClick={handleSaveEdit} disabled={loading || !lists.some((l) => editListChecks[l])}>
                {loading ? 'Saving...' : 'Save'}
              </button>
            </div>
          </div>
        </div>
      )}

      <div className="manager-card info-card">
        <h3>ℹ️ How It Works</h3>
        <ul>
          <li>Create multiple named watchlists using the <strong>+ New List</strong> button and select a list before adding symbols</li>
          <li>The Dashboard "Best Watchlist" card shows symbols across all lists combined</li>
          <li>Symbols added here are fetched every 10-15 minutes (configurable)</li>
          <li>Use the Update Watchlist Prices button to refresh all watchlist quotes immediately</li>
          <li>Use full symbols: ASX stocks need <code>.AX</code> suffix (e.g. BHP.AX), US stocks use plain ticker (e.g. AAPL, MSFT)</li>
        </ul>
      </div>
    </div>
  )
}
