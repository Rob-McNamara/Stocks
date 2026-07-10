import { useState, useEffect } from 'react'
import { apiClient, type EnrichedWatchlistItem } from '../services/api'
import { SECTORS } from '../utils/sectors'
import PriceChart from './PriceChart'
import StockAnalysis from './StockAnalysis'

// Thin client: SMA/crossover/volume indicators are computed by the API server
// (GET /api/watchlist/enriched) — one request replaces the N price-history
// calls this screen used to make.

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
  // Server-driven sector list from /api/meta; static list is the offline fallback
  const [sectorOptions, setSectorOptions] = useState<string[]>([...SECTORS])

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

  // A "Move to Holdings" transaction was saved. The server removed the
  // memberships atomically (POST /api/holdings/from-watchlist) — just resync.
  useEffect(() => {
    if (!removeSymbolRequest) return
    onRemoveSymbolConsumed?.()
    const resync = async () => {
      try {
        if (selectedSymbol === removeSymbolRequest) setSelectedSymbol('')
        const [listsData, enriched] = await Promise.all([
          apiClient.getWatchlistLists(),
          apiClient.getWatchlistEnriched(),
        ])
        setLists(listsData.length > 0 ? listsData : ['Default'])
        applyEnriched(enriched)
        setSuccess(`${removeSymbolRequest} moved to Holdings and removed from watchlist`)
        setTimeout(() => setSuccess(null), 3000)
      } catch (err) {
        setError(err instanceof Error ? err.message : 'Failed to refresh watchlist')
      }
    }
    resync()
  }, [removeSymbolRequest])

  /** Map the enriched server response into component state. */
  const applyEnriched = (data: { items: EnrichedWatchlistItem[]; prices_updated_at: string | null }) => {
    const items = data.items
    setSymbols(items.map((i) => ({
      id: i.id,
      symbol: i.symbol,
      list_name: i.list_name,
      added_at: i.added_at,
      notes: i.notes,
      breakthrough_price: i.breakthrough_price,
      stop_loss_price: i.stop_loss_price,
      custom_fields: i.custom_fields,
    })))
    const seen = new Set<string>()
    const prices: CurrentPrice[] = []
    const infoMap: Record<string, { instrument_type: string | null; long_name: string | null; currency: string | null }> = {}
    items.forEach((i) => {
      infoMap[i.symbol] = { instrument_type: i.instrument_type, long_name: i.long_name, currency: i.currency }
      if (seen.has(i.symbol)) return
      seen.add(i.symbol)
      prices.push({
        symbol: i.symbol,
        price: i.price,
        change: i.change,
        change_percent: i.change_percent,
        volume: i.volume,
        last_updated: i.last_updated ?? '',
        price_date: i.price_date,
        sma50: i.indicators?.sma50 ?? null,
        sma150: i.indicators?.sma150 ?? null,
        sma50Trend: i.indicators?.sma50_trend ?? null,
        sma150Trend: i.indicators?.sma150_trend ?? null,
        daysSince50SMA: i.indicators?.days_since_50sma ?? null,
        volumePct50SMA: i.indicators?.volume_pct_50sma ?? null,
        daysSince150SMA: i.indicators?.days_since_150sma ?? null,
        volumePct150SMA: i.indicators?.volume_pct_150sma ?? null,
        volumeChangePct: i.indicators?.volume_change_pct ?? null,
      })
    })
    setAllPrices(prices)
    setSymbolInfo(infoMap)
    if (data.prices_updated_at) setPricesUpdatedAt(data.prices_updated_at)
  }

  const loadWatchlistData = async () => {
    try {
      setLoading(true)
      setError(null)
      apiClient.getMeta().then((m) => { if (m.sectors?.length) setSectorOptions(m.sectors) }).catch(() => {})
      const [listsData, enriched, configData] = await Promise.all([
        apiClient.getWatchlistLists(),
        apiClient.getWatchlistEnriched(),
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
      setLists(effectiveLists)
      applyEnriched(enriched)
      if (enriched.items.length && !selectedSymbol) {
        setSelectedSymbol(enriched.items[0].symbol)
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
      // Fetch a live price for the new symbol (populates the server cache),
      // then reload the enriched watchlist
      await apiClient.getCurrentPrices([result.symbol]).catch(() => [])
      const [listsData, enriched] = await Promise.all([
        apiClient.getWatchlistLists(),
        apiClient.getWatchlistEnriched(),
      ])
      setLists(listsData.length > 0 ? listsData : ['Default'])
      applyEnriched(enriched)
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
      // Live fetch has updated the server cache — reload enriched rows
      const enriched = await apiClient.getWatchlistEnriched()
      applyEnriched(enriched)
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
      if (selectedSymbol === symbol) setSelectedSymbol('')
      setSuccess(`Removed ${symbol} from "${selectedList}"`)
      // Refresh lists (list may now be empty and removed from DB)
      const [listsData, enriched] = await Promise.all([
        apiClient.getWatchlistLists(),
        apiClient.getWatchlistEnriched(),
      ])
      setLists(listsData.length > 0 ? listsData : ['Default'])
      applyEnriched(enriched)
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
      const { breakthrough_price: bpStr, stop_loss_price: slStr, ...cfFields } = editCustomFields
      const parsePrice = (s: string | undefined) => {
        const n = parseFloat(s ?? '')
        return Number.isNaN(n) ? null : n
      }
      // Memberships, notes and fields are applied in one transactional call
      await apiClient.updateWatchlistSymbolLists(editingSymbol, {
        lists: lists.filter((l) => editListChecks[l]),
        notes: editNotes.trim() || null,
        breakthrough_price: parsePrice(bpStr),
        stop_loss_price: parsePrice(slStr),
        custom_fields: cfFields,
      })
      setEditingSymbol(null)
      const [listsData, enriched] = await Promise.all([
        apiClient.getWatchlistLists(),
        apiClient.getWatchlistEnriched(),
      ])
      setLists(listsData.length > 0 ? listsData : ['Default'])
      applyEnriched(enriched)
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
              {sectorOptions.map((s) => (
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
                  {(editCustomFields['sector'] ?? '') !== '' && !sectorOptions.includes(editCustomFields['sector']) && (
                    <option value={editCustomFields['sector']}>{editCustomFields['sector']}</option>
                  )}
                  {sectorOptions.map((s) => (
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
