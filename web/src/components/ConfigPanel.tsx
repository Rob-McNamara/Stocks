import { useState, useEffect } from 'react'
import { apiClient } from '../services/api'

interface ConfigPanelProps {
  onLoading: (loading: boolean) => void
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

export default function ConfigPanel({ onLoading }: ConfigPanelProps) {
  const [config, setConfig] = useState<Record<string, string>>({})
  const [editingKey, setEditingKey] = useState<string | null>(null)
  const [editingValue, setEditingValue] = useState('')
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [success, setSuccess] = useState<string | null>(null)
  const [manualSymbol, setManualSymbol] = useState('')
  const [manualPrice, setManualPrice] = useState('')

  useEffect(() => {
    loadConfig()
  }, [])

  const loadConfig = async () => {
    try {
      setLoading(true)
      setError(null)
      const data = await apiClient.getConfig()
      setConfig(data)
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
      setTimeout(() => setSuccess(null), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save manual price')
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
      setTimeout(() => setSuccess(null), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to remove manual price')
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
                    <th>Price</th>
                    <th></th>
                  </tr>
                </thead>
                <tbody>
                  {manualPrices.map(({ symbol, price }) => (
                    <tr key={symbol}>
                      <td><strong>{symbol}</strong></td>
                      <td>${parseFloat(price).toFixed(4)}</td>
                      <td>
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
