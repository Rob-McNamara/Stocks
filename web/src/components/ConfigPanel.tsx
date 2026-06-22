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

const CONFIG_SCHEMA: ConfigItem[] = [
  {
    key: 'FETCH_INTERVAL_SECS',
    value: '3600',
    description: 'Interval between scheduled ASX closing price fetches (seconds)',
    type: 'number'
  },
  {
    key: 'WATCHLIST_INTERVAL_SECS',
    value: '600',
    description: 'Interval between watchlist price updates (seconds)',
    type: 'number'
  },
  {
    key: 'DATABASE_PATH',
    value: './stocks.db',
    description: 'Path to SQLite database file',
    type: 'string'
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
  const [customFieldDefs, setCustomFieldDefs] = useState<{ key: string; label: string; type: 'text' | 'number' | 'date' }[]>([])
  const [newFieldLabel, setNewFieldLabel] = useState('')
  const [newFieldType, setNewFieldType] = useState<'text' | 'number' | 'date'>('text')

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
        setCustomFieldDefs(JSON.parse(data['watchlist_custom_fields'] ?? '[]'))
      } catch { setCustomFieldDefs([]) }
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
                      <td>
                        <button
                          className="btn btn-danger btn-small"
                          onClick={async () => {
                            const next = customFieldDefs.filter((_, j) => j !== i)
                            setCustomFieldDefs(next)
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
                  const key = newFieldLabel.trim().toLowerCase().replace(/\s+/g, '_').replace(/[^a-z0-9_]/g, '')
                  if (!key || customFieldDefs.some((d) => d.key === key)) return
                  const next = [...customFieldDefs, { key, label: newFieldLabel.trim(), type: newFieldType }]
                  setCustomFieldDefs(next)
                  setNewFieldLabel('')
                  await apiClient.updateConfig('watchlist_custom_fields', JSON.stringify(next))
                  onConfigChanged?.()
                }}
              >
                Add Field
              </button>
            </div>
          </div>

          <div className="config-card info-card">
            <h3>⚙️ Configuration Notes</h3>
            <ul>
              <li>
                <strong>FETCH_INTERVAL_SECS:</strong> How often the daemon fetches ASX closing prices (usually 3600 = 1 hour)
              </li>
              <li>
                <strong>WATCHLIST_INTERVAL_SECS:</strong> How often intraday prices are updated (usually 600 = 10 minutes)
              </li>
              <li>
                <strong>DATABASE_PATH:</strong> Where the SQLite database is stored (don't change this unless you know what you're doing)
              </li>
              <li>Changes take effect on the next daemon cycle</li>
              <li>For immediate changes, restart the daemon</li>
            </ul>
          </div>
        </>
      )}
    </div>
  )
}
