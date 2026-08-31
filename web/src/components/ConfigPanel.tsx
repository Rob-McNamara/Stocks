import { useState, useEffect } from 'react'
import { apiClient } from '../services/api'
import { OVERLAYS } from './PriceChart'
import {
  CHART_DEFAULTS_KEY, CHART_HEIGHT_RANGE, FALLBACK_CHART_DEFAULTS, TIMEFRAMES,
  parseChartDefaults, type ChartDefaults, type ChartTimeframe,
} from '../utils/chartDefaults'
import { invalidateAppConfig } from '../utils/appConfig'
import {
  LAYOUT_WIDTH_KEY, LAYOUT_WIDTHS, FALLBACK_LAYOUT_WIDTH,
  parseLayoutWidth, applyLayoutWidth, type LayoutWidth,
} from '../utils/layout'

interface ConfigPanelProps {
  onLoading: (loading: boolean) => void
  onConfigChanged?: () => void
}


export default function ConfigPanel({ onLoading, onConfigChanged }: ConfigPanelProps) {
  const [config, setConfig] = useState<Record<string, string>>({})
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [success, setSuccess] = useState<string | null>(null)
  const [manualSymbol, setManualSymbol] = useState('')
  const [manualPrice, setManualPrice] = useState('')
  const [editingPrices, setEditingPrices] = useState<Record<string, string>>({})
  const [historyStart, setHistoryStart] = useState('')
  const [typeSymbol, setTypeSymbol] = useState('')
  const [typeValue, setTypeValue] = useState('ETF')
  const builtInWatchlistKeys = ['breakthrough_price', 'stop_loss_price', 'sector']
  const builtInHoldingsKeys = ['stop_loss', 'trailing_sell_pct', 'trailing_sell_date', 'sector']
  const [customFieldDefs, setCustomFieldDefs] = useState<{ key: string; label: string; type: 'text' | 'number' | 'date' }[]>([])
  const [newFieldLabel, setNewFieldLabel] = useState('')
  const [newFieldType, setNewFieldType] = useState<'text' | 'number' | 'date'>('text')
  const [holdingsFieldDefs, setHoldingsFieldDefs] = useState<{ key: string; label: string; type: 'text' | 'number' | 'date'; actions: string[] }[]>([])
  const [newHoldingsFieldLabel, setNewHoldingsFieldLabel] = useState('')
  const [newHoldingsFieldType, setNewHoldingsFieldType] = useState<'text' | 'number' | 'date'>('text')
  const [newHoldingsFieldActions, setNewHoldingsFieldActions] = useState<string[]>(['purchase'])
  const [dashboardLists, setDashboardLists] = useState<{ key: string; label: string; source: 'holdings' | 'watchlist' | 'both'; field_key: string; operator: 'above' | 'below' | 'pct_above' | 'pct_below' | 'days_above' | 'days_below' | 'volume_cross_pct'; compare?: 'price' | 'volume'; limit: number; sort?: 'asc' | 'desc' }[]>([])
  const [editingWatchlistFieldIndex, setEditingWatchlistFieldIndex] = useState<number | null>(null)
  const [editingHoldingsFieldIndex, setEditingHoldingsFieldIndex] = useState<number | null>(null)
  const [newDashListLabel, setNewDashListLabel] = useState('')
  const [newDashListSource, setNewDashListSource] = useState<'holdings' | 'watchlist' | 'both'>('holdings')
  const [newDashListField, setNewDashListField] = useState('')
  const [newDashListOperator, setNewDashListOperator] = useState<'above' | 'below' | 'pct_above' | 'pct_below' | 'days_above' | 'days_below' | 'volume_cross_pct'>('above')
  // Lists written before this existed have no `compare`; the server reads a
  // missing value as 'price', so the editor shows the same default.
  const [newDashListCompare, setNewDashListCompare] = useState<'price' | 'volume'>('price')

  // The API validates these definitions and can refuse the write. Without the
  // rollback the panel would keep showing a list the server never stored, and
  // without the banner the refusal would vanish entirely.
  const saveDashboardLists = async (next: typeof dashboardLists) => {
    const previous = dashboardLists
    setDashboardLists(next)
    try {
      setError(null)
      await apiClient.updateConfig('dashboard_custom_lists', JSON.stringify(next))
      onConfigChanged?.()
    } catch (err) {
      setDashboardLists(previous)
      setError(err instanceof Error ? err.message : 'Failed to save dashboard lists')
    }
  }
  const [newDashListLimit, setNewDashListLimit] = useState('15')
  const [newDashListSort, setNewDashListSort] = useState<'asc' | 'desc'>('asc')
  const [editingDashListIndex, setEditingDashListIndex] = useState<number | null>(null)
  const [chartDefaults, setChartDefaults] = useState<ChartDefaults>(FALLBACK_CHART_DEFAULTS)
  const [layoutWidth, setLayoutWidth] = useState<LayoutWidth>(FALLBACK_LAYOUT_WIDTH)

  const saveLayoutWidth = async (width: LayoutWidth) => {
    const previous = layoutWidth
    // Applied before the save so the change is visible while it is in flight;
    // reverted below if the write fails, rather than leaving the screen showing
    // a width that was never stored.
    setLayoutWidth(width)
    applyLayoutWidth(width)
    try {
      await apiClient.updateConfig(LAYOUT_WIDTH_KEY, width)
      invalidateAppConfig()
      setConfig((c) => ({ ...c, [LAYOUT_WIDTH_KEY]: width }))
      setSuccess('Layout width saved')
      setTimeout(() => setSuccess(null), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save layout width')
      setLayoutWidth(previous)
      applyLayoutWidth(previous)
    }
  }

  /**
   * Written as one JSON value so a change is atomic — four separate keys could
   * be half-saved, leaving the chart opening in a state nobody chose.
   */
  const saveChartDefaults = async (patch: Partial<ChartDefaults>) => {
    const next = { ...chartDefaults, ...patch }
    setChartDefaults(next)
    try {
      await apiClient.updateConfig(CHART_DEFAULTS_KEY, JSON.stringify(next))
      // Charts cache the config; drop it so the next one to mount reads this.
      invalidateAppConfig()
      setConfig((c) => ({ ...c, [CHART_DEFAULTS_KEY]: JSON.stringify(next) }))
      onConfigChanged?.()
      setSuccess('Chart defaults saved')
      setTimeout(() => setSuccess(null), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save chart defaults')
      setChartDefaults(chartDefaults)
    }
  }

  useEffect(() => {
    loadConfig()
  }, [])

  const loadConfig = async () => {
    try {
      setLoading(true)
      setError(null)
      const data = await apiClient.getConfig()
      setHistoryStart(data['portfolio_history_start'] ?? '')
      setConfig(data)
      try {
        setCustomFieldDefs((JSON.parse(data['watchlist_custom_fields'] ?? '[]') as typeof customFieldDefs).filter((d) => !builtInWatchlistKeys.includes(d.key)))
      } catch { setCustomFieldDefs([]) }
      try {
        setHoldingsFieldDefs((JSON.parse(data['holdings_custom_fields'] ?? '[]') as typeof holdingsFieldDefs).filter((d) => !builtInHoldingsKeys.includes(d.key)))
      } catch { setHoldingsFieldDefs([]) }
      try {
        setDashboardLists(JSON.parse(data['dashboard_custom_lists'] ?? '[]'))
      } catch { setDashboardLists([]) }
      setChartDefaults(parseChartDefaults(data[CHART_DEFAULTS_KEY], OVERLAYS.map((o) => o.id)))
      setLayoutWidth(parseLayoutWidth(data[LAYOUT_WIDTH_KEY]))
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load configuration')
    } finally {
      setLoading(false)
      onLoading(false)
    }
  }

  const manualPrices = Object.entries(config)
    .filter(([k, v]) => k.startsWith('manual_price_') && v !== '')
    .map(([k, v]) => ({ symbol: k.replace('manual_price_', ''), price: v }))

  const manualTypes = Object.entries(config)
    .filter(([k, v]) => k.startsWith('instrument_type_') && v !== '')
    .map(([k, v]) => ({ symbol: k.replace('instrument_type_', ''), type: v }))

  const handleSaveManualPrice = async () => {
    const sym = manualSymbol.trim().toUpperCase()
    const val = manualPrice.trim()
    if (!sym || !val || isNaN(parseFloat(val))) return
    try {
      setLoading(true)
      setError(null)
      await apiClient.updateConfig(`manual_price_${sym}`, val)
      setConfig((c) => ({ ...c, [`manual_price_${sym}`]: val }))
      setManualSymbol('')
      setManualPrice('')
      setSuccess(`Manual price set for ${sym}`)
      onConfigChanged?.()
      setTimeout(() => setSuccess(null), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save manual price')
    } finally {
      setLoading(false)
    }
  }

  const handleUpdateManualPrice = async (symbol: string) => {
    const val = (editingPrices[symbol] ?? '').trim()
    if (!val || isNaN(parseFloat(val))) return
    try {
      setLoading(true)
      setError(null)
      await apiClient.updateConfig(`manual_price_${symbol}`, val)
      setConfig((c) => ({ ...c, [`manual_price_${symbol}`]: val }))
      setEditingPrices((e) => { const next = { ...e }; delete next[symbol]; return next })
      setSuccess(`Manual price updated for ${symbol}`)
      onConfigChanged?.()
      setTimeout(() => setSuccess(null), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to update manual price')
    } finally {
      setLoading(false)
    }
  }

  const handleRemoveManualPrice = async (symbol: string) => {
    try {
      setLoading(true)
      setError(null)
      await apiClient.updateConfig(`manual_price_${symbol}`, '')
      setConfig((c) => {
        const next = { ...c }
        delete next[`manual_price_${symbol}`]
        return next
      })
      setSuccess(`Manual price removed for ${symbol}`)
      onConfigChanged?.()
      setTimeout(() => setSuccess(null), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to remove manual price')
    } finally {
      setLoading(false)
    }
  }

  const handleSaveManualType = async () => {
    const sym = typeSymbol.trim().toUpperCase()
    if (!sym || !typeValue) return
    try {
      setLoading(true)
      setError(null)
      await apiClient.updateConfig(`instrument_type_${sym}`, typeValue)
      setConfig((c) => ({ ...c, [`instrument_type_${sym}`]: typeValue }))
      setTypeSymbol('')
      setSuccess(`Instrument type set for ${sym}`)
      onConfigChanged?.()
      setTimeout(() => setSuccess(null), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save instrument type')
    } finally {
      setLoading(false)
    }
  }

  const handleRemoveManualType = async (symbol: string) => {
    try {
      setLoading(true)
      setError(null)
      await apiClient.updateConfig(`instrument_type_${symbol}`, '')
      setConfig((c) => {
        const next = { ...c }
        delete next[`instrument_type_${symbol}`]
        return next
      })
      setSuccess(`Instrument type override removed for ${symbol}`)
      onConfigChanged?.()
      setTimeout(() => setSuccess(null), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to remove instrument type')
    } finally {
      setLoading(false)
    }
  }

  return (
    <div className="config-panel">
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

      {loading && Object.keys(config).length === 0 ? (
        <p className="loading-text">Loading configuration...</p>
      ) : (
        <>
          <div className="manager-card" style={{ marginTop: 24 }}>
            <h2>Manual Prices</h2>
            <p style={{ color: '#666', fontSize: 14, marginBottom: 16 }}>
              Set prices manually for stocks that cannot be fetched automatically. These appear in blue on the Holdings screen.
            </p>

            {manualPrices.length > 0 && (
              <table className="holdings-table" style={{ marginBottom: 20 }}>
                <thead>
                  <tr>
                    <th>Symbol</th>
                    <th>Price ($)</th>
                    <th></th>
                  </tr>
                </thead>
                <tbody>
                  {manualPrices.map(({ symbol, price }) => (
                    <tr key={symbol}>
                      <td><strong>{symbol}</strong></td>
                      <td>
                        <input
                          type="number"
                          min="0"
                          step="any"
                          value={editingPrices[symbol] ?? price}
                          onChange={(e) => setEditingPrices((ep) => ({ ...ep, [symbol]: e.target.value }))}
                          className="config-input"
                          style={{ width: 100 }}
                          disabled={loading}
                        />
                      </td>
                      <td style={{ display: 'flex', gap: 6 }}>
                        <button
                          className="btn btn-primary btn-small"
                          onClick={() => handleUpdateManualPrice(symbol)}
                          disabled={loading || !(editingPrices[symbol] ?? '').trim() && editingPrices[symbol] === undefined}
                        >
                          Update
                        </button>
                        <button
                          className="btn btn-danger btn-small"
                          onClick={() => handleRemoveManualPrice(symbol)}
                          disabled={loading}
                        >
                          Remove
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}

            <div className="config-edit" style={{ alignItems: 'flex-end' }}>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 13, color: '#666' }}>Symbol</label>
                <input
                  type="text"
                  value={manualSymbol}
                  onChange={(e) => setManualSymbol(e.target.value.toUpperCase())}
                  placeholder="e.g. ETPMPM.AX"
                  className="config-input"
                  disabled={loading}
                  maxLength={12}
                />
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 13, color: '#666' }}>Price ($)</label>
                <input
                  type="number"
                  min="0"
                  step="any"
                  value={manualPrice}
                  onChange={(e) => setManualPrice(e.target.value)}
                  placeholder="e.g. 4.25"
                  className="config-input"
                  disabled={loading}
                />
              </div>
              <button
                className="btn btn-primary btn-small"
                onClick={handleSaveManualPrice}
                disabled={loading || !manualSymbol || !manualPrice}
              >
                Set Price
              </button>
            </div>
          </div>

          <div className="manager-card" style={{ marginTop: 24 }}>
            <h2>Instrument Type Overrides</h2>
            <p style={{ color: '#666', fontSize: 14, marginBottom: 16 }}>
              Override the instrument type provided by Yahoo Finance. Used to control which grid a stock appears in on the Holdings screen.
            </p>

            {manualTypes.length > 0 && (
              <table className="holdings-table" style={{ marginBottom: 20 }}>
                <thead>
                  <tr>
                    <th>Symbol</th>
                    <th>Type</th>
                    <th></th>
                  </tr>
                </thead>
                <tbody>
                  {manualTypes.map(({ symbol, type }) => (
                    <tr key={symbol}>
                      <td><strong>{symbol}</strong></td>
                      <td>{type}</td>
                      <td>
                        <button
                          className="btn btn-danger btn-small"
                          onClick={() => handleRemoveManualType(symbol)}
                          disabled={loading}
                        >
                          Remove
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}

            <div className="config-edit" style={{ alignItems: 'flex-end' }}>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 13, color: '#666' }}>Symbol</label>
                <input
                  type="text"
                  value={typeSymbol}
                  onChange={(e) => setTypeSymbol(e.target.value.toUpperCase())}
                  placeholder="e.g. VTS.AX"
                  className="config-input"
                  disabled={loading}
                  maxLength={12}
                />
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 13, color: '#666' }}>Type</label>
                <select
                  value={typeValue}
                  onChange={(e) => setTypeValue(e.target.value)}
                  className="config-input"
                  disabled={loading}
                >
                  <option value="ETF">ETF</option>
                  <option value="EQUITY">EQUITY</option>
                  <option value="MUTUALFUND">MUTUALFUND</option>
                </select>
              </div>
              <button
                className="btn btn-primary btn-small"
                onClick={handleSaveManualType}
                disabled={loading || !typeSymbol}
              >
                Set Type
              </button>
            </div>
          </div>

          <div className="config-card">
            <h2>Watchlist Custom Fields</h2>
            <p style={{ fontSize: 13, color: '#666', marginBottom: 16 }}>
              Define extra fields to record against each watchlist symbol. These appear automatically when adding or editing a symbol.
            </p>
            {customFieldDefs.length > 0 && (
              <table className="holdings-table" style={{ marginBottom: 16 }}>
                <thead>
                  <tr>
                    <th>Label</th>
                    <th>Type</th>
                    <th></th>
                  </tr>
                </thead>
                <tbody>
                  {customFieldDefs.map((def, i) => (
                    <tr key={def.key}>
                      <td>{def.label}</td>
                      <td style={{ color: '#888' }}>{def.type}</td>
                      <td style={{ display: 'flex', gap: 6 }}>
                        <button
                          className="btn btn-outline btn-small"
                          onClick={() => {
                            setEditingWatchlistFieldIndex(i)
                            setNewFieldLabel(def.label)
                            setNewFieldType(def.type)
                          }}
                        >
                          Edit
                        </button>
                        <button
                          className="btn btn-danger btn-small"
                          onClick={async () => {
                            const next = customFieldDefs.filter((_, j) => j !== i)
                            setCustomFieldDefs(next)
                            setEditingWatchlistFieldIndex(null)
                            await apiClient.updateConfig('watchlist_custom_fields', JSON.stringify(next))
                            onConfigChanged?.()
                          }}
                        >
                          Remove
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
            <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', alignItems: 'flex-end' }}>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 12, color: '#666' }}>Field label</label>
                <input
                  type="text"
                  value={newFieldLabel}
                  onChange={(e) => setNewFieldLabel(e.target.value)}
                  placeholder="e.g. Target Price"
                  className="config-input"
                  style={{ width: 180 }}
                />
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 12, color: '#666' }}>Type</label>
                <select
                  value={newFieldType}
                  onChange={(e) => setNewFieldType(e.target.value as 'text' | 'number' | 'date')}
                  className="config-input"
                >
                  <option value="text">Text</option>
                  <option value="number">Number</option>
                  <option value="date">Date</option>
                </select>
              </div>
              <button
                className="btn btn-primary"
                disabled={!newFieldLabel.trim()}
                onClick={async () => {
                  if (editingWatchlistFieldIndex !== null) {
                    const next = customFieldDefs.map((d, j) =>
                      j === editingWatchlistFieldIndex ? { ...d, label: newFieldLabel.trim(), type: newFieldType } : d
                    )
                    setCustomFieldDefs(next)
                    setEditingWatchlistFieldIndex(null)
                    setNewFieldLabel('')
                    await apiClient.updateConfig('watchlist_custom_fields', JSON.stringify(next))
                    onConfigChanged?.()
                  } else {
                    const key = newFieldLabel.trim().toLowerCase().replace(/\s+/g, '_').replace(/[^a-z0-9_]/g, '')
                    if (!key || customFieldDefs.some((d) => d.key === key) || builtInWatchlistKeys.includes(key)) return
                    const next = [...customFieldDefs, { key, label: newFieldLabel.trim(), type: newFieldType }]
                    setCustomFieldDefs(next)
                    setNewFieldLabel('')
                    await apiClient.updateConfig('watchlist_custom_fields', JSON.stringify(next))
                    onConfigChanged?.()
                  }
                }}
              >
                {editingWatchlistFieldIndex !== null ? 'Save' : 'Add Field'}
              </button>
              {editingWatchlistFieldIndex !== null && (
                <button
                  className="btn btn-outline"
                  onClick={() => { setEditingWatchlistFieldIndex(null); setNewFieldLabel('') }}
                >
                  Cancel
                </button>
              )}
            </div>
          </div>

          <div className="config-card">
            <h2>Holdings Custom Fields</h2>
            <p style={{ fontSize: 13, color: '#666', marginBottom: 16 }}>
              Define extra fields to record against each holdings transaction. Choose which transaction types each field applies to.
            </p>
            {holdingsFieldDefs.length > 0 && (
              <table className="holdings-table" style={{ marginBottom: 16 }}>
                <thead>
                  <tr>
                    <th>Label</th>
                    <th>Type</th>
                    <th>Actions</th>
                    <th></th>
                  </tr>
                </thead>
                <tbody>
                  {holdingsFieldDefs.map((def, i) => (
                    <tr key={def.key}>
                      <td>{def.label}</td>
                      <td style={{ color: '#888' }}>{def.type}</td>
                      <td style={{ color: '#555' }}>{def.actions.join(', ')}</td>
                      <td style={{ display: 'flex', gap: 6 }}>
                        <button
                          className="btn btn-outline btn-small"
                          onClick={() => {
                            setEditingHoldingsFieldIndex(i)
                            setNewHoldingsFieldLabel(def.label)
                            setNewHoldingsFieldType(def.type)
                            setNewHoldingsFieldActions([...def.actions])
                          }}
                        >
                          Edit
                        </button>
                        <button
                          className="btn btn-danger btn-small"
                          onClick={async () => {
                            const next = holdingsFieldDefs.filter((_, j) => j !== i)
                            setHoldingsFieldDefs(next)
                            setEditingHoldingsFieldIndex(null)
                            await apiClient.updateConfig('holdings_custom_fields', JSON.stringify(next))
                            onConfigChanged?.()
                          }}
                        >
                          Remove
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
            <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', alignItems: 'flex-end' }}>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 12, color: '#666' }}>Field label</label>
                <input
                  type="text"
                  value={newHoldingsFieldLabel}
                  onChange={(e) => setNewHoldingsFieldLabel(e.target.value)}
                  placeholder="e.g. Target Price"
                  className="config-input"
                  style={{ width: 180 }}
                />
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 12, color: '#666' }}>Type</label>
                <select
                  value={newHoldingsFieldType}
                  onChange={(e) => setNewHoldingsFieldType(e.target.value as 'text' | 'number' | 'date')}
                  className="config-input"
                >
                  <option value="text">Text</option>
                  <option value="number">Number</option>
                  <option value="date">Date</option>
                </select>
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 12, color: '#666' }}>Actions</label>
                <div style={{ display: 'flex', gap: 8 }}>
                  {['purchase', 'sale', 'dividend'].map((action) => (
                    <label key={action} style={{ display: 'flex', alignItems: 'center', gap: 4, fontSize: 13, cursor: 'pointer' }}>
                      <input
                        type="checkbox"
                        checked={newHoldingsFieldActions.includes(action)}
                        onChange={(e) => {
                          if (e.target.checked) {
                            setNewHoldingsFieldActions((prev) => [...prev, action])
                          } else {
                            setNewHoldingsFieldActions((prev) => prev.filter((a) => a !== action))
                          }
                        }}
                      />
                      {action}
                    </label>
                  ))}
                </div>
              </div>
              <button
                className="btn btn-primary"
                disabled={!newHoldingsFieldLabel.trim() || newHoldingsFieldActions.length === 0}
                onClick={async () => {
                  if (editingHoldingsFieldIndex !== null) {
                    const next = holdingsFieldDefs.map((d, j) =>
                      j === editingHoldingsFieldIndex ? { ...d, label: newHoldingsFieldLabel.trim(), type: newHoldingsFieldType, actions: [...newHoldingsFieldActions] } : d
                    )
                    setHoldingsFieldDefs(next)
                    setEditingHoldingsFieldIndex(null)
                    setNewHoldingsFieldLabel('')
                    setNewHoldingsFieldActions(['purchase'])
                    await apiClient.updateConfig('holdings_custom_fields', JSON.stringify(next))
                    onConfigChanged?.()
                  } else {
                    const key = newHoldingsFieldLabel.trim().toLowerCase().replace(/\s+/g, '_').replace(/[^a-z0-9_]/g, '')
                    if (!key || holdingsFieldDefs.some((d) => d.key === key) || builtInHoldingsKeys.includes(key)) return
                    const next = [...holdingsFieldDefs, { key, label: newHoldingsFieldLabel.trim(), type: newHoldingsFieldType, actions: [...newHoldingsFieldActions] }]
                    setHoldingsFieldDefs(next)
                    setNewHoldingsFieldLabel('')
                    setNewHoldingsFieldActions(['purchase'])
                    await apiClient.updateConfig('holdings_custom_fields', JSON.stringify(next))
                    onConfigChanged?.()
                  }
                }}
              >
                {editingHoldingsFieldIndex !== null ? 'Save' : 'Add Field'}
              </button>
              {editingHoldingsFieldIndex !== null && (
                <button
                  className="btn btn-outline"
                  onClick={() => { setEditingHoldingsFieldIndex(null); setNewHoldingsFieldLabel(''); setNewHoldingsFieldActions(['purchase']) }}
                >
                  Cancel
                </button>
              )}
            </div>
          </div>

          <div className="config-card">
            <h2>Dashboard Lists</h2>
            <p style={{ fontSize: 13, color: '#666', marginBottom: 16 }}>
              Define custom lists for the Dashboard that compare a stock's current price or volume against a custom field value.
            </p>
            {dashboardLists.length > 0 && (
              <table className="holdings-table" style={{ marginBottom: 16 }}>
                <thead>
                  <tr>
                    <th>Label</th>
                    <th>Source</th>
                    <th>Compare</th>
                    <th>Field</th>
                    <th>Condition</th>
                    <th>Sort</th>
                    <th>Limit</th>
                    <th></th>
                  </tr>
                </thead>
                <tbody>
                  {dashboardLists.map((dl, i) => (
                    <tr key={dl.key}>
                      <td>{dl.label}</td>
                      <td style={{ color: '#888' }}>{dl.compare === 'volume' ? 'Volume' : 'Price'}</td>
                      <td style={{ color: '#555' }}>{dl.field_key}</td>
                      <td>{{ above: 'Above field', below: 'Below field', pct_above: '% above compared value', pct_below: '% below compared value', days_above: 'Days above field', days_below: 'Days below field', volume_cross_pct: 'Volume % on cross above' }[dl.operator] ?? dl.operator}</td>
                      <td style={{ color: '#888' }}>{dl.sort === 'desc' ? 'Desc' : 'Asc'}</td>
                      <td>{dl.limit}</td>
                      <td style={{ display: 'flex', gap: 6 }}>
                        <button
                          className="btn btn-outline btn-small"
                          onClick={() => {
                            setEditingDashListIndex(i)
                            setNewDashListLabel(dl.label)
                            setNewDashListSource(dl.source)
                            setNewDashListField(dl.field_key)
                            setNewDashListOperator(dl.operator)
                            setNewDashListCompare(dl.compare ?? 'price')
                            setNewDashListLimit(dl.limit.toString())
                            setNewDashListSort(dl.sort ?? 'asc')
                          }}
                        >
                          Edit
                        </button>
                        <button
                          className="btn btn-danger btn-small"
                          onClick={async () => {
                            const next = dashboardLists.filter((_, j) => j !== i)
                            setEditingDashListIndex(null)
                            await saveDashboardLists(next)
                          }}
                        >
                          Remove
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
            <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', alignItems: 'flex-end' }}>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 12, color: '#666' }}>Label</label>
                <input
                  type="text"
                  value={newDashListLabel}
                  onChange={(e) => setNewDashListLabel(e.target.value)}
                  placeholder="e.g. Above Target"
                  className="config-input"
                  style={{ width: 160 }}
                />
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 12, color: '#666' }}>Source</label>
                <select
                  value={newDashListSource}
                  onChange={(e) => setNewDashListSource(e.target.value as 'holdings' | 'watchlist' | 'both')}
                  className="config-input"
                >
                  <option value="holdings">Holdings</option>
                  <option value="watchlist">Watchlist</option>
                  <option value="both">Both</option>
                </select>
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 12, color: '#666' }}>Compare</label>
                <select
                  value={newDashListCompare}
                  onChange={(e) => setNewDashListCompare(e.target.value as 'price' | 'volume')}
                  className="config-input"
                  title="Which figure is measured against the field"
                >
                  <option value="price">Price</option>
                  <option value="volume">Volume</option>
                </select>
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 12, color: '#666' }}>Field</label>
                <select
                  value={newDashListField}
                  onChange={(e) => setNewDashListField(e.target.value)}
                  className="config-input"
                >
                  <option value="">Select field...</option>
                  {/* Indicators are derived from stored closes, so they apply to
                      holdings and watchlist symbols alike — unlike the entries
                      below, which only exist in one of the two tables. */}
                  <option value="indicator:sma50">Indicator: 50-Day SMA</option>
                  <option value="indicator:sma150">Indicator: 150-Day SMA</option>
                  <option value="indicator:ema40w">Indicator: 40-Week EMA</option>
                  <option value="holdings:stop_loss">Holdings: Stop Loss Price</option>
                  <option value="holdings:trailing_sell_pct">Holdings: Trailing Sell %</option>
                  {holdingsFieldDefs.map((f) => (
                    <option key={`h_${f.key}`} value={`holdings:${f.key}`}>Holdings: {f.label}</option>
                  ))}
                  <option value="watchlist:breakthrough_price">Watchlist: Breakthrough Price</option>
                  <option value="watchlist:stop_loss_price">Watchlist: Stop Loss Price</option>
                  {customFieldDefs.map((f) => (
                    <option key={`w_${f.key}`} value={`watchlist:${f.key}`}>Watchlist: {f.label}</option>
                  ))}
                </select>
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 12, color: '#666' }}>Condition</label>
                <select
                  value={newDashListOperator}
                  onChange={(e) => setNewDashListOperator(e.target.value as 'above' | 'below' | 'pct_above' | 'pct_below' | 'days_above' | 'days_below' | 'volume_cross_pct')}
                  className="config-input"
                >
                  <option value="above">Above field</option>
                  <option value="below">Below field</option>
                  <option value="pct_above">% above compared value</option>
                  <option value="pct_below">% below compared value</option>
                  {/* Crossover conditions date the crossing instead of
                      measuring the gap, so they rank on days or on the volume
                      behind it rather than on the percentage difference. */}
                  <option value="days_above">Days above field</option>
                  <option value="days_below">Days below field</option>
                  <option value="volume_cross_pct">Volume % on cross above</option>
                </select>
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 12, color: '#666' }}>Sort</label>
                <select
                  value={newDashListSort}
                  onChange={(e) => setNewDashListSort(e.target.value as 'asc' | 'desc')}
                  className="config-input"
                >
                  <option value="asc">Ascending</option>
                  <option value="desc">Descending</option>
                </select>
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 12, color: '#666' }}>Limit</label>
                <input
                  type="number"
                  value={newDashListLimit}
                  onChange={(e) => setNewDashListLimit(e.target.value)}
                  min="1"
                  max="50"
                  className="config-input"
                  style={{ width: 60 }}
                />
              </div>
              <button
                className="btn btn-primary"
                disabled={!newDashListLabel.trim() || !newDashListField}
                onClick={async () => {
                  if (editingDashListIndex !== null) {
                    const next = dashboardLists.map((d, j) =>
                      j === editingDashListIndex ? { ...d, label: newDashListLabel.trim(), source: newDashListSource, field_key: newDashListField, operator: newDashListOperator, compare: newDashListCompare, limit: parseInt(newDashListLimit) || 15, sort: newDashListSort } : d
                    )
                    setEditingDashListIndex(null)
                    setNewDashListLabel('')
                    setNewDashListField('')
                    setNewDashListSort('asc')
                    setNewDashListCompare('price')
                    await saveDashboardLists(next)
                  } else {
                    const key = newDashListLabel.trim().toLowerCase().replace(/\s+/g, '_').replace(/[^a-z0-9_]/g, '')
                    if (!key || dashboardLists.some((d) => d.key === key)) return
                    const next = [...dashboardLists, {
                      key,
                      label: newDashListLabel.trim(),
                      source: newDashListSource,
                      field_key: newDashListField,
                      operator: newDashListOperator,
                      compare: newDashListCompare,
                      limit: parseInt(newDashListLimit) || 15,
                      sort: newDashListSort,
                    }]
                    setNewDashListLabel('')
                    setNewDashListField('')
                    setNewDashListSort('asc')
                    setNewDashListCompare('price')
                    await saveDashboardLists(next)
                  }
                }}
              >
                {editingDashListIndex !== null ? 'Save' : 'Add List'}
              </button>
              {editingDashListIndex !== null && (
                <button
                  className="btn btn-outline"
                  onClick={() => { setEditingDashListIndex(null); setNewDashListLabel(''); setNewDashListField(''); setNewDashListSort('asc'); setNewDashListCompare('price') }}
                >
                  Cancel
                </button>
              )}
            </div>
          </div>

          <div className="manager-card" style={{ marginTop: 24 }}>
            <h2>Layout</h2>
            <p style={{ color: '#666', fontSize: 14, marginBottom: 16 }}>
              How much of the window the app uses. Tables, charts and card grids all
              expand into whatever width you allow.
            </p>
            <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
              {LAYOUT_WIDTHS.map(([value, label, description]) => (
                <label key={value} style={{ display: 'flex', alignItems: 'flex-start', gap: 8, fontSize: 14 }}>
                  <input
                    type="radio"
                    name="layout-width"
                    checked={layoutWidth === value}
                    onChange={() => void saveLayoutWidth(value)}
                    style={{ marginTop: 3 }}
                  />
                  <span>
                    <strong>{label}</strong>
                    <span style={{ display: 'block', color: '#666', fontSize: 12 }}>{description}</span>
                  </span>
                </label>
              ))}
            </div>
          </div>

          <div className="manager-card" style={{ marginTop: 24 }}>
            <h2>Portfolio History Start</h2>
            <p style={{ color: '#666', fontSize: 14, marginBottom: 16 }}>
              Earliest date the Portfolio Value chart will show. A holding cannot be
              valued before its first stored price, so the years before that draw the
              stock line flat at zero — cash alone, dressed up as portfolio history.
              Setting a date cuts that stretch off and measures growth from there.
              Leave it empty to show everything back to your first transaction.
            </p>
            <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
              <input
                type="date"
                value={historyStart}
                onChange={(e) => setHistoryStart(e.target.value)}
                className="config-input"
              />
              <button
                className="btn btn-primary"
                onClick={async () => {
                  try {
                    setError(null)
                    await apiClient.updateConfig('portfolio_history_start', historyStart)
                    setSuccess(historyStart ? `Portfolio history starts from ${historyStart}` : 'Portfolio history shows everything')
                    setTimeout(() => setSuccess(null), 3000)
                    onConfigChanged?.()
                  } catch (err) {
                    setError(err instanceof Error ? err.message : 'Failed to save the history start date')
                  }
                }}
              >
                Save
              </button>
              {historyStart && (
                <button
                  className="btn btn-outline"
                  onClick={async () => {
                    try {
                      setError(null)
                      setHistoryStart('')
                      await apiClient.updateConfig('portfolio_history_start', '')
                      setSuccess('Portfolio history shows everything')
                      setTimeout(() => setSuccess(null), 3000)
                      onConfigChanged?.()
                    } catch (err) {
                      setError(err instanceof Error ? err.message : 'Failed to clear the history start date')
                    }
                  }}
                >
                  Clear
                </button>
              )}
            </div>
          </div>

          <div className="manager-card" style={{ marginTop: 24 }}>
            <h2>Stock Chart Defaults</h2>
            <p style={{ color: '#666', fontSize: 14, marginBottom: 16 }}>
              How the Stock Chart opens, wherever it appears. Changing it here does not
              disturb a chart already on screen — the settings apply the next time one loads.
            </p>
            <div className="config-list" style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
              <div style={{ display: 'flex', gap: 16, alignItems: 'flex-end', flexWrap: 'wrap' }}>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                  <label style={{ fontSize: 12, color: '#666' }}>Period</label>
                  <select
                    className="config-input"
                    value={chartDefaults.timeframe}
                    onChange={(e) => void saveChartDefaults({ timeframe: e.target.value as ChartTimeframe })}
                  >
                    {TIMEFRAMES.map(([value, label]) => (
                      <option key={value} value={value}>{label}</option>
                    ))}
                  </select>
                </div>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                  <label style={{ fontSize: 12, color: '#666' }}>Style</label>
                  <select
                    className="config-input"
                    value={chartDefaults.chartType}
                    onChange={(e) => void saveChartDefaults({ chartType: e.target.value as 'line' | 'candle' })}
                  >
                    <option value="line">Line</option>
                    <option value="candle">Candles</option>
                  </select>
                </div>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                  <label style={{ fontSize: 12, color: '#666' }}>Height</label>
                  <input
                    type="number"
                    className="config-input"
                    style={{ width: 90 }}
                    min={CHART_HEIGHT_RANGE.min}
                    max={CHART_HEIGHT_RANGE.max}
                    step={20}
                    value={chartDefaults.height}
                    onChange={(e) => setChartDefaults((d) => ({ ...d, height: Number(e.target.value) }))}
                    onBlur={(e) => void saveChartDefaults({ height: Number(e.target.value) })}
                    title="Opening height in pixels — drag the bottom edge of a chart to change it for that view only"
                  />
                </div>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                  <label style={{ fontSize: 12, color: '#666' }}>Bars</label>
                  <select
                    className="config-input"
                    value={chartDefaults.barInterval}
                    onChange={(e) => void saveChartDefaults({ barInterval: e.target.value as 'day' | 'week' })}
                  >
                    <option value="day">Day</option>
                    <option value="week">Week</option>
                  </select>
                </div>
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
                <label style={{ fontSize: 12, color: '#666' }}>Moving averages shown on open</label>
                <div style={{ display: 'flex', gap: 14, flexWrap: 'wrap' }}>
                  {OVERLAYS.map((o) => (
                    <label key={o.id} style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 13 }}>
                      <input
                        type="checkbox"
                        checked={chartDefaults.overlays.includes(o.id)}
                        onChange={(e) => void saveChartDefaults({
                          overlays: e.target.checked
                            ? [...chartDefaults.overlays, o.id]
                            : chartDefaults.overlays.filter((id) => id !== o.id),
                        })}
                      />
                      {/* The swatch is the same colour the chart draws, so the
                          choice here is recognisable on the chart itself. */}
                      <span style={{ width: 10, height: 10, borderRadius: 2, background: o.color, display: 'inline-block' }} />
                      {o.label}
                    </label>
                  ))}
                </div>
              </div>
            </div>
          </div>

          <div className="manager-card" style={{ marginTop: 24 }}>
            <h2>AI Stock Analysis</h2>
            <p style={{ color: '#666', fontSize: 14, marginBottom: 16 }}>
              Configure the AI provider for stock analysis. The API key is stored in the database.
            </p>
            <div className="config-list" style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
              <div style={{ display: 'flex', gap: 8, alignItems: 'flex-end', flexWrap: 'wrap' }}>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                  <label style={{ fontSize: 12, color: '#666' }}>Provider</label>
                  <select
                    value={config['ai_provider'] ?? 'anthropic'}
                    onChange={async (e) => {
                      await apiClient.updateConfig('ai_provider', e.target.value)
                      setConfig((c) => ({ ...c, ai_provider: e.target.value }))
                      onConfigChanged?.()
                    }}
                    className="config-input"
                  >
                    <option value="anthropic">Anthropic (Claude)</option>
                    <option value="openai">OpenAI (GPT)</option>
                  </select>
                </div>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 4, flex: 1, minWidth: 200 }}>
                  <label style={{ fontSize: 12, color: '#666' }}>API Key</label>
                  <input
                    type="password"
                    value={config['ai_api_key'] ?? ''}
                    onChange={(e) => setConfig((c) => ({ ...c, ai_api_key: e.target.value }))}
                    onBlur={async (e) => {
                      if (e.target.value) {
                        await apiClient.updateConfig('ai_api_key', e.target.value)
                        setConfig((c) => ({ ...c, ai_api_key_configured: 'true' }))
                        onConfigChanged?.()
                        setSuccess('API key saved')
                        setTimeout(() => setSuccess(null), 3000)
                      }
                    }}
                    placeholder={config['ai_api_key_configured'] === 'true' ? '•••••••• (configured — enter to replace)' : 'Enter API key...'}
                    className="config-input"
                  />
                </div>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                  <label style={{ fontSize: 12, color: '#666' }}>Model</label>
                  <input
                    type="text"
                    value={config['ai_model'] ?? 'claude-sonnet-4-20250514'}
                    onChange={(e) => setConfig((c) => ({ ...c, ai_model: e.target.value }))}
                    onBlur={async (e) => {
                      await apiClient.updateConfig('ai_model', e.target.value || 'claude-sonnet-4-20250514')
                      onConfigChanged?.()
                    }}
                    placeholder="claude-sonnet-4-20250514"
                    className="config-input"
                    style={{ width: 250 }}
                  />
                </div>
              </div>
              {(config['ai_api_key'] || config['ai_api_key_configured'] === 'true') && (
                <p style={{ fontSize: 12, color: '#4caf50', margin: 0 }}>API key is configured</p>
              )}
            </div>
          </div>

          <div className="config-card info-card">
            <h3>⚙️ Configuration Notes</h3>
            <ul>
              <li>
                Intraday prices are managed by the API server: refreshed on app startup (<code>POST /api/v1/refresh</code>, debounced) and via the Update Prices buttons.
              </li>
              <li>
                The price daemon only stores daily ASX closes and is configured entirely by environment variables (<code>STOCK_SYMBOLS</code>, <code>FETCH_SCHEDULE_HOUR</code>/<code>FETCH_SCHEDULE_MINUTE</code>, <code>DATABASE_PATH</code>) — change them in its launch configuration.
              </li>
            </ul>
          </div>
        </>
      )}
    </div>
  )
}
