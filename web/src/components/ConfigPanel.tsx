import { useState, useEffect } from 'react'
import { apiClient } from '../services/api'

interface ConfigPanelProps {
  onLoading: (loading: boolean) => void
  onConfigChanged?: () => void
}

interface ConfigItem {
  key: string
  value: string
  description: string
  type: 'string' | 'number'
}

// Only settings the daemon actually reads from the database belong here.
// Fetch schedule (FETCH_SCHEDULE_HOUR/MINUTE) and DATABASE_PATH are
// environment variables read at daemon startup and can't be changed from
// this screen.
const CONFIG_SCHEMA: ConfigItem[] = [
  {
    key: 'WATCHLIST_INTERVAL_SECS',
    value: '900',
    description: 'Interval between watchlist price updates (seconds, min 30) — applies from the next daemon cycle',
    type: 'number'
  }
]

export default function ConfigPanel({ onLoading, onConfigChanged }: ConfigPanelProps) {
  const [config, setConfig] = useState<Record<string, string>>({})
  const [editingKey, setEditingKey] = useState<string | null>(null)
  const [editingValue, setEditingValue] = useState('')
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [success, setSuccess] = useState<string | null>(null)
  const [manualSymbol, setManualSymbol] = useState('')
  const [manualPrice, setManualPrice] = useState('')
  const [editingPrices, setEditingPrices] = useState<Record<string, string>>({})
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
  const [dashboardLists, setDashboardLists] = useState<{ key: string; label: string; source: 'holdings' | 'watchlist' | 'both'; field_key: string; operator: 'above' | 'below' | 'pct_above' | 'pct_below'; limit: number; sort?: 'asc' | 'desc' }[]>([])
  const [editingWatchlistFieldIndex, setEditingWatchlistFieldIndex] = useState<number | null>(null)
  const [editingHoldingsFieldIndex, setEditingHoldingsFieldIndex] = useState<number | null>(null)
  const [newDashListLabel, setNewDashListLabel] = useState('')
  const [newDashListSource, setNewDashListSource] = useState<'holdings' | 'watchlist' | 'both'>('holdings')
  const [newDashListField, setNewDashListField] = useState('')
  const [newDashListOperator, setNewDashListOperator] = useState<'above' | 'below' | 'pct_above' | 'pct_below'>('above')
  const [newDashListLimit, setNewDashListLimit] = useState('15')
  const [newDashListSort, setNewDashListSort] = useState<'asc' | 'desc'>('asc')
  const [editingDashListIndex, setEditingDashListIndex] = useState<number | null>(null)

  useEffect(() => {
    loadConfig()
  }, [])

  const loadConfig = async () => {
    try {
      setLoading(true)
      setError(null)
      const data = await apiClient.getConfig()
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
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load configuration')
    } finally {
      setLoading(false)
      onLoading(false)
    }
  }

  const handleEdit = (key: string) => {
    setEditingKey(key)
    setEditingValue(config[key] || '')
    setError(null)
    setSuccess(null)
  }

  const handleSave = async (key: string) => {
    try {
      setLoading(true)
      setError(null)
      setSuccess(null)

      await apiClient.updateConfig(key, editingValue)
      setConfig({ ...config, [key]: editingValue })
      setEditingKey(null)
      setSuccess(`Updated ${key}`)
      onConfigChanged?.()

      // Clear success message after 3 seconds
      setTimeout(() => setSuccess(null), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to update configuration')
    } finally {
      setLoading(false)
      onLoading(false)
    }
  }

  const handleCancel = () => {
    setEditingKey(null)
    setEditingValue('')
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
          <div className="config-list">
            {CONFIG_SCHEMA.map(({ key, description, type }) => (
              <div key={key} className="config-item">
                <div className="config-info">
                  <label className="config-key">{key}</label>
                  <p className="config-description">{description}</p>
                </div>
                <div className="config-value-container">
                  {editingKey === key ? (
                    <div className="config-edit">
                      <input
                        type={type === 'number' ? 'number' : 'text'}
                        value={editingValue}
                        onChange={(e) => setEditingValue(e.target.value)}
                        className="config-input"
                        disabled={loading}
                        min={type === 'number' ? '1' : undefined}
                      />
                      <button
                        onClick={() => handleSave(key)}
                        className="btn btn-primary btn-small"
                        disabled={loading}
                      >
                        Save
                      </button>
                      <button
                        onClick={handleCancel}
                        className="btn btn-secondary btn-small"
                        disabled={loading}
                      >
                        Cancel
                      </button>
                    </div>
                  ) : (
                    <div className="config-display">
                      <span className="config-value">{config[key] || '—'}</span>
                      <button
                        onClick={() => handleEdit(key)}
                        className="btn btn-outline btn-small"
                        disabled={loading}
                      >
                        Edit
                      </button>
                    </div>
                  )}
                </div>
              </div>
            ))}
          </div>

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
              Define custom lists for the Dashboard that compare current stock price against a custom field value.
            </p>
            {dashboardLists.length > 0 && (
              <table className="holdings-table" style={{ marginBottom: 16 }}>
                <thead>
                  <tr>
                    <th>Label</th>
                    <th>Source</th>
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
                      <td style={{ color: '#888' }}>{dl.source}</td>
                      <td style={{ color: '#555' }}>{dl.field_key}</td>
                      <td>{{ above: 'Price above field', below: 'Price below field', pct_above: '% above price', pct_below: '% below price' }[dl.operator] ?? dl.operator}</td>
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
                            setDashboardLists(next)
                            setEditingDashListIndex(null)
                            await apiClient.updateConfig('dashboard_custom_lists', JSON.stringify(next))
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
                <label style={{ fontSize: 12, color: '#666' }}>Custom Field</label>
                <select
                  value={newDashListField}
                  onChange={(e) => setNewDashListField(e.target.value)}
                  className="config-input"
                >
                  <option value="">Select field...</option>
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
                  onChange={(e) => setNewDashListOperator(e.target.value as 'above' | 'below' | 'pct_above' | 'pct_below')}
                  className="config-input"
                >
                  <option value="above">Price above field</option>
                  <option value="below">Price below field</option>
                  <option value="pct_above">% above price</option>
                  <option value="pct_below">% below price</option>
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
                      j === editingDashListIndex ? { ...d, label: newDashListLabel.trim(), source: newDashListSource, field_key: newDashListField, operator: newDashListOperator, limit: parseInt(newDashListLimit) || 15, sort: newDashListSort } : d
                    )
                    setDashboardLists(next)
                    setEditingDashListIndex(null)
                    setNewDashListLabel('')
                    setNewDashListField('')
                    setNewDashListSort('asc')
                    await apiClient.updateConfig('dashboard_custom_lists', JSON.stringify(next))
                    onConfigChanged?.()
                  } else {
                    const key = newDashListLabel.trim().toLowerCase().replace(/\s+/g, '_').replace(/[^a-z0-9_]/g, '')
                    if (!key || dashboardLists.some((d) => d.key === key)) return
                    const next = [...dashboardLists, {
                      key,
                      label: newDashListLabel.trim(),
                      source: newDashListSource,
                      field_key: newDashListField,
                      operator: newDashListOperator,
                      limit: parseInt(newDashListLimit) || 15,
                      sort: newDashListSort,
                    }]
                    setDashboardLists(next)
                    setNewDashListLabel('')
                    setNewDashListField('')
                    setNewDashListSort('asc')
                    await apiClient.updateConfig('dashboard_custom_lists', JSON.stringify(next))
                    onConfigChanged?.()
                  }
                }}
              >
                {editingDashListIndex !== null ? 'Save' : 'Add List'}
              </button>
              {editingDashListIndex !== null && (
                <button
                  className="btn btn-outline"
                  onClick={() => { setEditingDashListIndex(null); setNewDashListLabel(''); setNewDashListField(''); setNewDashListSort('asc') }}
                >
                  Cancel
                </button>
              )}
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
                <strong>WATCHLIST_INTERVAL_SECS:</strong> How often intraday watchlist prices are updated (default 900 = 15 minutes). The daemon re-reads this each cycle, so no restart is needed.
              </li>
              <li>
                The daily fetch schedule (<code>FETCH_SCHEDULE_HOUR</code>/<code>FETCH_SCHEDULE_MINUTE</code>) and <code>DATABASE_PATH</code> are environment variables read when the daemon starts — change them in the daemon's launch configuration, not here.
              </li>
            </ul>
          </div>
        </>
      )}
    </div>
  )
}
