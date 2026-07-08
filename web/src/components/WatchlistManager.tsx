import { useState, useEffect } from 'react'
import { apiClient } from '../services/api'
import { calculateSMA, crossoverStats, getLatestSMA, smaTrend } from '../utils/sma'
import { mapLimit } from '../utils/async'
import { SECTORS } from '../utils/sectors'
import PriceChart from './PriceChart'
import StockAnalysis from './StockAnalysis'

interface WatchlistSymbol {
  id: number
  symbol: string
  list_name: string
  added_at: string
  notes: string | null
  breakthrough_price: number | null
  stop_loss_price: number | null
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
  sma50Trend?: 'up' | 'down' | null
  sma150Trend?: 'up' | 'down' | null
  error?: string
}

interface WatchlistManagerProps {
  onLoading: (loading: boolean) => void
  initialSymbol?: string | null
  onInitialSymbolConsumed?: () => void
  onMoveToHoldings?: (data: { symbol: string; price?: number; notes?: string; customFields?: Record<string, string> }) => void
  /** Symbol whose memberships should be removed — set by App once a "Move to Holdings" transaction is saved */
  removeSymbolRequest?: string | null
  onRemoveSymbolConsumed?: () => void
}


async function enrichOne(price: CurrentPrice): Promise<CurrentPrice> {
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
    const sma50Trend = smaTrend(sma50Array)
    const sma150Trend = smaTrend(sma150Array)
    return { ...price, sma50, sma150, volumeChangePct, daysSince50SMA, volumePct50SMA, daysSince150SMA, volumePct150SMA, sma50Trend, sma150Trend }
  } catch {
    return price
  }
}

async function enrichWithSMA(pricesData: CurrentPrice[]): Promise<CurrentPrice[]> {
  return mapLimit(pricesData, 6, enrichOne)
}

export default function WatchlistManager({ onLoading, initialSymbol, onInitialSymbolConsumed, onMoveToHoldings, removeSymbolRequest, onRemoveSymbolConsumed }: WatchlistManagerProps) {
  const [lists, setLists] = useState<string[]>(['Default'])
  const [defaultList, setDefaultList] = useState('Default')
  const [selectedList, setSelectedList] = useState('Default')
  const [creatingList, setCreatingList] = useState(false)
  const [newListName, setNewListName] = useState('')
  const [renamingList, setRenamingList] = useState(false)
  const [renameListValue, setRenameListValue] = useState('')
  const [symbols, setSymbols] = useState<WatchlistSymbol[]>([])
  const [allPrices, setAllPrices] = useState<CurrentPrice[]>([])
  const [symbolInfo, setSymbolInfo] = useState<Record<string, { instrument_type: string | null; long_name: string | null; currency: string | null }>>({})
  const [selectedSymbol, setSelectedSymbol] = useState('')
  const [newSymbol, setNewSymbol] = useState('')
  const [newSymbolNotes, setNewSymbolNotes] = useState('')
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [success, setSuccess] = useState<string | null>(null)
  const [pricesUpdatedAt, setPricesUpdatedAt] = useState<string | null>(null)
  const [editingSymbol, setEditingSymbol] = useState<string | null>(null)
  const [analyzingSymbol, setAnalyzingSymbol] = useState<string | null>(null)
  const [editListChecks, setEditListChecks] = useState<Record<string, boolean>>({})
  const [editNotes, setEditNotes] = useState('')
  const [customFieldDefs, setCustomFieldDefs] = useState<CustomFieldDef[]>([])
  const builtInWatchlistKeys = ['breakthrough_price', 'stop_loss_price', 'sector']
  const extraCustomFieldDefs = customFieldDefs.filter((def) => !builtInWatchlistKeys.includes(def.key))
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

  // A "Move to Holdings" transaction was saved — now it's safe to drop the
  // symbol from all watchlists.
  useEffect(() => {
    if (!removeSymbolRequest) return
    const entries = symbols.filter((s) => s.symbol === removeSymbolRequest)
    onRemoveSymbolConsumed?.()
    if (entries.length === 0) return
    const removeAll = async () => {
      try {
        for (const entry of entries) {
          await apiClient.removeWatchlistSymbol(entry.id)
        }
        const remaining = symbols.filter((s) => s.symbol !== removeSymbolRequest)
        setSymbols(remaining)
        if (selectedSymbol === removeSymbolRequest) setSelectedSymbol(remaining[0]?.symbol || '')
        const listsData = await apiClient.getWatchlistLists()
        setLists(listsData.length > 0 ? listsData : ['Default'])
        setSuccess(`${removeSymbolRequest} moved to Holdings and removed from watchlist`)
        setTimeout(() => setSuccess(null), 3000)
      } catch (err) {
        setError(err instanceof Error ? err.message : 'Failed to remove from watchlist')
      }
    }
    removeAll()
  }, [removeSymbolRequest])

  const loadWatchlistData = async () => {
    try {
      setLoading(true)
      setError(null)
      const [listsData, symbolsData, pricesData, infoData, configData] = await Promise.all([
        apiClient.getWatchlistLists(),
        apiClient.getWatchlistSymbols(),
        apiClient.getWatchlistCachedPrices(),
        apiClient.getSymbolInfo(),
        apiClient.getConfig(),
      ])
      try {
        const defs = JSON.parse(configData['watchlist_custom_fields'] ?? '[]') as CustomFieldDef[]
        setCustomFieldDefs(defs)
      } catch { setCustomFieldDefs([]) }
      setPricesUpdatedAt(configData['watchlist_prices_updated_at'] ?? null)
      const savedDefault = configData['default_watchlist'] ?? ''
      const effectiveLists = listsData.length > 0 ? listsData : ['Default']
      const effectiveDefault = effectiveLists.includes(savedDefault) ? savedDefault : effectiveLists[0]
      setDefaultList(effectiveDefault)
      if (!selectedSymbol) setSelectedList(effectiveDefault)
      const infoMap: Record<string, { instrument_type: string | null; long_name: string | null; currency: string | null }> = {}
      infoData.forEach((i) => { infoMap[i.symbol] = { instrument_type: i.instrument_type, long_name: i.long_name, currency: i.currency } })
      setSymbolInfo(infoMap)
      setLists(effectiveLists)
      setSymbols(symbolsData)
      setAllPrices(pricesData)
      if (symbolsData.length && !selectedSymbol) {
        setSelectedSymbol(symbolsData[0].symbol)
      }
      // Show UI immediately, then enrich with SMA data in the background
      setLoading(false)
      onLoading(false)
      enrichWithSMA(pricesData).then((enriched) => setAllPrices(enriched))
      return
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

  const handleRenameList = async () => {
    const newName = renameListValue.trim()
    if (!newName || newName === selectedList) {
      setRenamingList(false)
      return
    }
    try {
      setLoading(true)
      await apiClient.renameWatchlistList(selectedList, newName)
      const [listsData, symbolsData] = await Promise.all([
        apiClient.getWatchlistLists(),
        apiClient.getWatchlistSymbols(),
      ])
      setLists(listsData.length > 0 ? listsData : ['Default'])
      setSymbols(symbolsData)
      setSelectedList(newName)
      setRenamingList(false)
      setSuccess(`Renamed list to "${newName}"`)
      setTimeout(() => setSuccess(null), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to rename list')
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
      const parsePrice = (s: string | undefined) => {
        const n = parseFloat(s ?? '')
        return Number.isNaN(n) ? null : n
      }
      const bp = parsePrice(newSymbolFields['breakthrough_price'])
      const sl = parsePrice(newSymbolFields['stop_loss_price'])
      const { breakthrough_price: _bp, stop_loss_price: _sl, ...cfFields } = newSymbolFields
      const result = await apiClient.addWatchlistSymbol(newSymbol.trim(), selectedList, newSymbolNotes.trim() || undefined, { breakthroughPrice: bp, stopLossPrice: sl, customFields: cfFields })
      setNewSymbol('')
      setNewSymbolNotes('')
      setNewSymbolFields({})
      if (!selectedSymbol) setSelectedSymbol(result.symbol)
      setSuccess(`Added ${result.symbol} to "${selectedList}"`)
      // Fetch live price for just the new symbol, use cache for the rest
      const [listsData, symbolsData, cachedPrices, newSymbolPrices] = await Promise.all([
        apiClient.getWatchlistLists(),
        apiClient.getWatchlistSymbols(),
        apiClient.getWatchlistCachedPrices(),
        apiClient.getCurrentPrices([result.symbol]),
      ])
      // Merge: replace cached entry for the new symbol with live data
      const priceMap = new Map(cachedPrices.map((p) => [p.symbol, p]))
      newSymbolPrices.forEach((p) => priceMap.set(p.symbol, p))
      const pricesData = Array.from(priceMap.values())
      setLists(listsData)
      setSymbols(symbolsData)
      setAllPrices(pricesData)
      enrichWithSMA(pricesData).then((enriched) => setAllPrices(enriched))
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
      setAllPrices(pricesData)
      enrichWithSMA(pricesData).then((enriched) => setAllPrices(enriched))
      // Reload config to get updated timestamp
      apiClient.getConfig().then((cfg) => setPricesUpdatedAt(cfg['watchlist_prices_updated_at'] ?? null))
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
        apiClient.getWatchlistCachedPrices(),
      ])
      setLists(listsData.length > 0 ? listsData : ['Default'])
      setAllPrices(pricesData)
      enrichWithSMA(pricesData).then((enriched) => setAllPrices(enriched))
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
    setEditCustomFields({
      ...existing?.custom_fields ?? {},
      breakthrough_price: existing?.breakthrough_price != null ? String(existing.breakthrough_price) : '',
      stop_loss_price: existing?.stop_loss_price != null ? String(existing.stop_loss_price) : '',
    })
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
      const { breakthrough_price: bpStr, stop_loss_price: slStr, ...cfFields } = editCustomFields
      const parsePrice = (s: string | undefined) => {
        const n = parseFloat(s ?? '')
        return Number.isNaN(n) ? null : n
      }
      const bp = parsePrice(bpStr)
      const sl = parsePrice(slStr)
      const opts = { breakthroughPrice: bp, stopLossPrice: sl, customFields: cfFields }
      await Promise.all([
        ...toAdd.map((l) => apiClient.addWatchlistSymbol(editingSymbol, l, editNotes.trim() || undefined, opts)),
        ...toRemove.map((s) => apiClient.removeWatchlistSymbol(s.id)),
        ...existingRows.map((s) => apiClient.updateWatchlistSymbol(s.id, editNotes.trim() || null, opts)),
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

  const renderItem = (entry: WatchlistSymbol) => {
    const { id, symbol, added_at, notes, custom_fields } = entry
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
          {entry.breakthrough_price != null && (
            <div style={{ fontSize: 11, color: '#555', marginTop: 1 }}>
              <span style={{ color: '#999' }}>Breakthrough Price:</span> {entry.breakthrough_price}
            </div>
          )}
          {entry.stop_loss_price != null && (
            <div style={{ fontSize: 11, color: '#555', marginTop: 1 }}>
              <span style={{ color: '#999' }}>Stop Loss Price:</span> {entry.stop_loss_price}
            </div>
          )}
          {custom_fields['sector'] && (
            <div style={{ fontSize: 11, color: '#555', marginTop: 1 }}>
              <span style={{ color: '#999' }}>Sector:</span> {custom_fields['sector']}
            </div>
          )}
          {extraCustomFieldDefs.filter((def) => custom_fields[def.key]).map((def) => (
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
                {priceData.price != null ? (
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
                      {priceData.sma50Trend != null && (
                        <span style={{ marginLeft: 4, fontSize: 10, color: priceData.sma50Trend === 'down' ? '#c62828' : '#2e7d32', fontWeight: 600 }}>
                          {priceData.sma50Trend === 'down' ? '↓' : '↑'}
                        </span>
                      )}
                      {priceData.daysSince50SMA != null && (
                        <span style={{ marginLeft: 2, fontSize: 10, color: '#2e7d32', fontWeight: 600 }}>
                          {priceData.daysSince50SMA}d
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
                      {priceData.sma150Trend != null && (
                        <span style={{ marginLeft: 4, fontSize: 10, color: priceData.sma150Trend === 'down' ? '#c62828' : '#2e7d32', fontWeight: 600 }}>
                          {priceData.sma150Trend === 'down' ? '↓' : '↑'}
                        </span>
                      )}
                      {priceData.daysSince150SMA != null && (
                        <span style={{ marginLeft: 2, fontSize: 10, color: '#2e7d32', fontWeight: 600 }}>
                          {priceData.daysSince150SMA}d
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
            onClick={(e) => { e.stopPropagation(); setAnalyzingSymbol(symbol) }}
            className="btn btn-outline btn-small"
            disabled={loading}
            title="AI Analysis"
            style={{ padding: '2px 6px', fontSize: 13, lineHeight: 1 }}
          >
            🔍
          </button>
          {onMoveToHoldings && (
            <button
              onClick={(e) => {
                e.stopPropagation()
                // Pre-fills the Holdings form; the watchlist entry is only
                // removed once the holding transaction is actually saved
                // (via the removeSymbolRequest prop from App).
                const priceData = listPrices.find((p) => p.symbol === symbol)
                onMoveToHoldings({
                  symbol,
                  price: priceData?.price ?? undefined,
                  notes: notes ?? undefined,
                  customFields: { ...custom_fields, ...(entry.stop_loss_price != null ? { stop_loss: String(entry.stop_loss_price) } : {}) },
                })
              }}
              className="btn btn-outline btn-small"
              disabled={loading}
              title="Move to Holdings"
              style={{ padding: '2px 6px', fontSize: 13, lineHeight: 1 }}
            >
              📥
            </button>
          )}
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
            {!creatingList && !renamingList ? (
              <>
                <button
                  className="btn btn-outline"
                  style={{ fontSize: 12, padding: '4px 10px' }}
                  onClick={() => setCreatingList(true)}
                  disabled={loading}
                >
                  + New List
                </button>
                <button
                  className="btn btn-outline"
                  style={{ fontSize: 12, padding: '4px 10px' }}
                  onClick={() => { setRenamingList(true); setRenameListValue(selectedList) }}
                  disabled={loading}
                >
                  Rename
                </button>
                <label style={{ display: 'flex', alignItems: 'center', gap: 4, fontSize: 12, cursor: 'pointer', marginLeft: 8 }}>
                  <input
                    type="checkbox"
                    checked={selectedList === defaultList}
                    onChange={async (e) => {
                      if (e.target.checked) {
                        setDefaultList(selectedList)
                        await apiClient.updateConfig('default_watchlist', selectedList)
                      }
                    }}
                    disabled={loading || selectedList === defaultList}
                    style={{ width: 14, height: 14, cursor: 'pointer' }}
                  />
                  Default
                </label>
              </>
            ) : renamingList ? (
              <div style={{ display: 'flex', gap: 4 }}>
                <input
                  autoFocus
                  type="text"
                  value={renameListValue}
                  onChange={(e) => setRenameListValue(e.target.value)}
                  onKeyDown={(e) => { if (e.key === 'Enter') handleRenameList(); if (e.key === 'Escape') setRenamingList(false) }}
                  placeholder="New name"
                  style={{ padding: '4px 8px', borderRadius: 4, border: '1px solid #ccc', fontSize: 13, width: 130 }}
                  maxLength={40}
                />
                <button className="btn btn-primary" style={{ fontSize: 12, padding: '4px 10px' }} onClick={handleRenameList} disabled={!renameListValue.trim() || renameListValue.trim() === selectedList}>Rename</button>
                <button className="btn btn-outline" style={{ fontSize: 12, padding: '4px 10px' }} onClick={() => setRenamingList(false)}>Cancel</button>
              </div>
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
          <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', marginTop: 8 }}>
            <input
              type="number"
              value={newSymbolFields['breakthrough_price'] ?? ''}
              onChange={(e) => setNewSymbolFields((prev) => ({ ...prev, breakthrough_price: e.target.value }))}
              placeholder="Breakthrough Price"
              className="symbol-input"
              disabled={loading}
              style={{ flex: 1, minWidth: 120 }}
            />
            <input
              type="number"
              value={newSymbolFields['stop_loss_price'] ?? ''}
              onChange={(e) => setNewSymbolFields((prev) => ({ ...prev, stop_loss_price: e.target.value }))}
              placeholder="Stop Loss Price"
              className="symbol-input"
              disabled={loading}
              style={{ flex: 1, minWidth: 120 }}
            />
            <select
              value={newSymbolFields['sector'] ?? ''}
              onChange={(e) => setNewSymbolFields((prev) => ({ ...prev, sector: e.target.value }))}
              className="config-input"
              disabled={loading}
              title="Sector"
              style={{ flex: 1, minWidth: 140 }}
            >
              <option value="">Sector (optional)</option>
              {SECTORS.map((s) => (
                <option key={s} value={s}>{s}</option>
              ))}
            </select>
            {extraCustomFieldDefs.map((def) => (
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
        </form>
      </div>

      {error && <div className="alert alert-error">❌ {error}</div>}
      {success && <div className="alert alert-success">✓ {success}</div>}

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
            markers={(() => {
              const sym = selectedSymbol || chartSymbols[0]?.symbol
              const entry = symbols.find((s) => s.symbol === sym)
              const result: Array<{ price: number; label: string; mode: 'breakthrough' | 'stoploss'; color: string }> = []
              if (entry?.breakthrough_price) result.push({ price: entry.breakthrough_price, label: 'Breakthrough', mode: 'breakthrough', color: '#4caf50' })
              if (entry?.stop_loss_price) result.push({ price: entry.stop_loss_price, label: 'Stop Loss', mode: 'stoploss', color: '#e91e63' })
              return result.length > 0 ? result : undefined
            })()}
          />
        </div>
      )}

      <div className="manager-card">
        <div className="card-header">
          <h2>
            {selectedList} ({listSymbols.length})
            {pricesUpdatedAt && (
              <span style={{ fontSize: 12, fontWeight: 400, color: '#888', marginLeft: 10 }}>
                Prices updated: {new Date(pricesUpdatedAt).toLocaleString()}
                {listPrices.some((p) => p.last_updated && pricesUpdatedAt && p.last_updated > pricesUpdatedAt) ? ' *' : ''}
              </span>
            )}
          </h2>
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
          <div style={{ maxHeight: 500, overflowY: 'auto' }}>
            <ul className="symbols-list">{listSymbols.map(renderItem)}</ul>
          </div>
        )}
      </div>

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
            <div style={{ display: 'flex', flexDirection: 'column', gap: 4, marginBottom: 12 }}>
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
            <div style={{ display: 'flex', flexDirection: 'column', gap: 10, marginBottom: 20 }}>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 13, color: '#666' }}>Breakthrough Price</label>
                <input
                  type="number"
                  value={editCustomFields['breakthrough_price'] ?? ''}
                  onChange={(e) => setEditCustomFields((prev) => ({ ...prev, breakthrough_price: e.target.value }))}
                  className="symbol-input"
                  style={{ width: '100%' }}
                />
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 13, color: '#666' }}>Stop Loss Price</label>
                <input
                  type="number"
                  value={editCustomFields['stop_loss_price'] ?? ''}
                  onChange={(e) => setEditCustomFields((prev) => ({ ...prev, stop_loss_price: e.target.value }))}
                  className="symbol-input"
                  style={{ width: '100%' }}
                />
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 13, color: '#666' }}>Sector</label>
                <select
                  value={editCustomFields['sector'] ?? ''}
                  onChange={(e) => setEditCustomFields((prev) => ({ ...prev, sector: e.target.value }))}
                  className="symbol-input"
                  style={{ width: '100%' }}
                >
                  <option value="">— None —</option>
                  {(editCustomFields['sector'] ?? '') !== '' && !(SECTORS as readonly string[]).includes(editCustomFields['sector']) && (
                    <option value={editCustomFields['sector']}>{editCustomFields['sector']}</option>
                  )}
                  {SECTORS.map((s) => (
                    <option key={s} value={s}>{s}</option>
                  ))}
                </select>
              </div>
              {extraCustomFieldDefs.map((def) => (
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

      {analyzingSymbol && (
        <StockAnalysis
          symbol={analyzingSymbol}
          symbolName={symbolInfo[analyzingSymbol]?.long_name}
          onClose={() => setAnalyzingSymbol(null)}
        />
      )}
    </div>
  )
}
