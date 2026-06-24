import { useEffect, useMemo, useState } from 'react'
import { apiClient } from '../services/api'

const SUPPORTED_CURRENCIES = ['AUD', 'USD', 'GBP', 'EUR', 'JPY', 'CAD', 'HKD', 'SGD', 'NZD']

interface HoldingTransaction {
  id: number
  symbol: string
  transaction_type: 'purchase' | 'sale' | 'dividend'
  date: string
  quantity: number | null
  price: number | null
  amount: number | null
  brokerage: number | null
  notes: string | null
  created_at: string
  dividends_total: number
  currency: string
  original_price: number | null
  fx_rate: number | null
  custom_fields: Record<string, string>
}

interface DividendEvent {
  symbol: string
  ex_date: string
  payment_date: string | null
  amount: number
}

interface TransactionRow {
  key: string
  id: number | null
  symbol: string
  type: 'purchase' | 'sale' | 'dividend'
  date: string
  quantity: number | null
  price: number | null
  currency: string
  original_price: number | null
  amount: number | null
  brokerage: number | null
  notes: string | null
}

interface EditState {
  id: number
  symbol: string
  type: 'purchase' | 'sale' | 'dividend'
  date: string
  quantity: string
  currency: string
  price: string
  amount: string
  brokerage: string
  notes: string
}

type FilterType = 'all' | 'purchase' | 'sale' | 'dividend'

export default function Transactions({ onLoading, holdingsVersion }: { onLoading: (loading: boolean) => void; holdingsVersion?: number }) {
  const [transactions, setTransactions] = useState<HoldingTransaction[]>([])
  const [dividends, setDividends] = useState<DividendEvent[]>([])
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [filter, setFilter] = useState<FilterType>('all')
  const [symbolFilter, setSymbolFilter] = useState('')
  const [editing, setEditing] = useState<EditState | null>(null)
  const [editFxRate, setEditFxRate] = useState<number | null>(null)
  const [editFxDate, setEditFxDate] = useState<string | null>(null)
  const [editFxLoading, setEditFxLoading] = useState(false)

  useEffect(() => {
    const load = async () => {
      try {
        setLoading(true)
        setError(null)
        onLoading(true)
        const [txData, divData] = await Promise.all([
          apiClient.getHoldings(),
          apiClient.getDividends(),
        ])
        setTransactions(txData)
        setDividends(divData)
      } catch (err) {
        setError(err instanceof Error ? err.message : 'Failed to load transactions')
      } finally {
        setLoading(false)
        onLoading(false)
      }
    }
    load()
  }, [holdingsVersion])

  useEffect(() => {
    if (!editing || editing.currency === 'AUD' || editing.type === 'dividend') {
      setEditFxRate(null)
      setEditFxDate(null)
      return
    }
    setEditFxLoading(true)
    apiClient.getFxRateForDate(editing.currency, editing.date).then((result) => {
      if (result) {
        setEditFxRate(result.rate)
        setEditFxDate(result.date)
      } else {
        setEditFxRate(null)
        setEditFxDate(null)
      }
    }).finally(() => setEditFxLoading(false))
  }, [editing?.currency, editing?.date])

  const rows = useMemo((): TransactionRow[] => {
    // Track the earliest purchase date per symbol
    const firstPurchaseDate: Record<string, string> = {}
    transactions.forEach((tx) => {
      if (tx.transaction_type === 'purchase') {
        if (!firstPurchaseDate[tx.symbol] || tx.date < firstPurchaseDate[tx.symbol]) {
          firstPurchaseDate[tx.symbol] = tx.date
        }
      }
    })

    const txRows: TransactionRow[] = transactions.map((tx) => ({
      key: `tx-${tx.id}`,
      id: tx.id,
      symbol: tx.symbol,
      type: tx.transaction_type,
      date: tx.date,
      quantity: tx.quantity,
      price: tx.price,
      currency: tx.currency || 'AUD',
      original_price: tx.original_price ?? null,
      amount: tx.amount,
      brokerage: tx.brokerage,
      notes: tx.notes,
    }))

    const divRows: TransactionRow[] = dividends
      .filter((d) => firstPurchaseDate[d.symbol] && d.ex_date >= firstPurchaseDate[d.symbol])
      .map((d, i) => ({
        key: `div-${d.symbol}-${d.ex_date}-${i}`,
        id: null,
        symbol: d.symbol,
        type: 'dividend' as const,
        date: d.ex_date,
        quantity: null,
        price: null,
        currency: 'AUD',
        original_price: null,
        amount: d.amount,
        brokerage: null,
        notes: d.payment_date ? `Payment: ${new Date(d.payment_date).toLocaleDateString()}` : null,
      }))

    return [...txRows, ...divRows]
      .filter((r) => filter === 'all' || r.type === filter)
      .filter((r) => !symbolFilter || r.symbol.includes(symbolFilter.toUpperCase()))
      .sort((a, b) => b.date.localeCompare(a.date))
  }, [transactions, dividends, filter, symbolFilter])

  const startEdit = (row: TransactionRow) => {
    if (row.id === null) return
    const tx = transactions.find((t) => t.id === row.id)
    const currency = tx?.currency || 'AUD'
    const displayPrice = currency !== 'AUD' && tx?.original_price != null
      ? tx.original_price.toString()
      : (row.price !== null ? row.price.toString() : '')
    setEditFxRate(tx?.fx_rate ?? null)
    setEditFxDate(currency !== 'AUD' ? row.date : null)
    setEditing({
      id: row.id,
      symbol: row.symbol,
      type: row.type,
      date: row.date,
      quantity: row.quantity !== null ? row.quantity.toString() : '',
      currency,
      price: displayPrice,
      amount: row.amount !== null ? row.amount.toString() : '',
      brokerage: row.brokerage !== null ? row.brokerage.toString() : '',
      notes: row.notes ?? '',
    })
  }

  const handleDelete = async (id: number) => {
    if (!confirm('Delete this transaction?')) return
    try {
      setSaving(true)
      await apiClient.removeHoldingTransaction(id)
      setTransactions((prev) => prev.filter((tx) => tx.id !== id))
      if (editing?.id === id) setEditing(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to delete transaction')
    } finally {
      setSaving(false)
    }
  }

  const handleSave = async () => {
    if (!editing) return
    try {
      setSaving(true)
      const payload: Record<string, unknown> = {
        symbol: editing.symbol,
        transaction_type: editing.type,
        date: editing.date,
        amount: editing.amount ? parseFloat(editing.amount) : undefined,
        brokerage: editing.brokerage ? parseFloat(editing.brokerage) : undefined,
        notes: editing.notes || undefined,
      }
      if (editing.type === 'purchase' || editing.type === 'sale') {
        payload.quantity = editing.quantity ? parseFloat(editing.quantity) : undefined
        if (editing.currency !== 'AUD' && editFxRate) {
          const originalPrice = parseFloat(editing.price)
          payload.currency = editing.currency
          payload.original_price = originalPrice
          payload.fx_rate = editFxRate
          payload.price = originalPrice * editFxRate
        } else {
          payload.currency = 'AUD'
          payload.price = editing.price ? parseFloat(editing.price) : undefined
        }
      }
      // Preserve existing custom_fields so they aren't deleted by the backend
      const existingTx = transactions.find((tx) => tx.id === editing.id)
      if (existingTx?.custom_fields && Object.keys(existingTx.custom_fields).length > 0) {
        payload.custom_fields = existingTx.custom_fields
      }
      const updated = await apiClient.updateHoldingTransaction(editing.id, payload)
      setTransactions((prev) => prev.map((tx) => tx.id === editing.id ? updated : tx))
      setEditing(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save transaction')
    } finally {
      setSaving(false)
    }
  }

  const typeLabel = (type: string) => {
    if (type === 'purchase') return 'Purchase'
    if (type === 'sale') return 'Sale'
    return 'Dividend'
  }

  const typeColor = (type: string) => {
    if (type === 'purchase') return '#2196f3'
    if (type === 'sale') return '#f44336'
    return '#4caf50'
  }

  return (
    <div className="transactions-screen">
      <div className="manager-card">
        <div className="card-header">
          <h2>Transactions</h2>
          <span style={{ color: '#888', fontSize: 14 }}>{rows.length} record{rows.length !== 1 ? 's' : ''}</span>
        </div>

        <div style={{ display: 'flex', gap: 8, marginBottom: 16, flexWrap: 'wrap' }}>
          {(['all', 'purchase', 'sale', 'dividend'] as FilterType[]).map((f) => (
            <button
              key={f}
              className={`sma-button ${filter === f ? 'active' : ''}`}
              onClick={() => setFilter(f)}
            >
              {f === 'all' ? 'All' : typeLabel(f) + 's'}
            </button>
          ))}
          <input
            type="text"
            value={symbolFilter}
            onChange={(e) => setSymbolFilter(e.target.value)}
            placeholder="Filter by symbol…"
            className="symbol-input"
            style={{ marginLeft: 'auto', maxWidth: 180 }}
          />
        </div>

        {error && <div className="alert alert-error">❌ {error}</div>}

        {loading ? (
          <p className="loading-text">Loading transactions...</p>
        ) : rows.length === 0 ? (
          <p className="empty-text">No transactions found.</p>
        ) : (
          <div className="holdings-table-wrapper">
            <table className="holdings-table">
              <thead>
                <tr>
                  <th>Date</th>
                  <th>Symbol</th>
                  <th>Type</th>
                  <th>Quantity</th>
                  <th>Price</th>
                  <th>Amount</th>
                  <th>Brokerage</th>
                  <th>Notes</th>
                  <th></th>
                </tr>
              </thead>
              <tbody>
                {rows.map((row) => (
                  <tr key={row.key}>
                    <td>{new Date(row.date).toLocaleDateString()}</td>
                    <td>
                      <div style={{ display: 'flex', alignItems: 'center', gap: 4 }}>
                        <strong>{row.symbol}</strong>
                        {row.currency !== 'AUD' && (
                          <span style={{ fontSize: 10, fontWeight: 600, padding: '1px 4px', borderRadius: 3, background: '#fff3e0', color: '#e65100' }}>
                            {row.currency}
                          </span>
                        )}
                      </div>
                    </td>
                    <td>
                      <span style={{ color: typeColor(row.type), fontWeight: 600 }}>
                        {typeLabel(row.type)}
                      </span>
                    </td>
                    <td>{row.quantity !== null ? row.quantity.toFixed(2) : '—'}</td>
                    <td>
                      {row.price !== null ? `$${row.price.toFixed(4)}` : '—'}
                      {row.currency !== 'AUD' && row.original_price !== null && (
                        <span style={{ fontSize: 10, color: '#888', marginLeft: 4 }}>
                          ({row.currency} {row.original_price.toFixed(2)})
                        </span>
                      )}
                    </td>
                    <td>
                      {row.type === 'dividend' && row.amount !== null
                        ? `$${row.amount.toFixed(4)} per share`
                        : row.quantity !== null && row.price !== null
                        ? `$${(row.quantity * row.price).toFixed(2)}`
                        : '—'}
                    </td>
                    <td>{row.brokerage !== null ? `$${row.brokerage.toFixed(2)}` : '—'}</td>
                    <td style={{ color: '#888' }}>{row.notes || '—'}</td>
                    <td style={{ display: 'flex', gap: 6 }}>
                      {row.id !== null && (
                        <>
                          <button
                            className="btn btn-secondary btn-small"
                            onClick={() => startEdit(row)}
                            disabled={saving}
                          >
                            Edit
                          </button>
                          <button
                            className="btn btn-danger btn-small"
                            onClick={() => handleDelete(row.id!)}
                            disabled={saving}
                          >
                            Delete
                          </button>
                        </>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>

      {editing && (
        <div style={{ position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.4)', display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 1000 }}>
          <div style={{ background: '#fff', borderRadius: 12, padding: 28, minWidth: 380, maxWidth: 480, width: '100%', boxShadow: '0 8px 32px rgba(0,0,0,0.18)' }}>
            <h3 style={{ marginBottom: 18 }}>Edit {typeLabel(editing.type)} — {editing.symbol}</h3>
            <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 13, color: '#666' }}>Date</label>
                <input type="date" className="config-input" value={editing.date} onChange={(e) => setEditing({ ...editing, date: e.target.value })} />
              </div>
              {(editing.type === 'purchase' || editing.type === 'sale') && (
                <>
                  <div style={{ display: 'flex', gap: 10 }}>
                    <div style={{ flex: 1, display: 'flex', flexDirection: 'column', gap: 4 }}>
                      <label style={{ fontSize: 13, color: '#666' }}>Quantity</label>
                      <input type="number" min="0" step="any" className="config-input" value={editing.quantity} onChange={(e) => setEditing({ ...editing, quantity: e.target.value })} />
                    </div>
                    <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                      <label style={{ fontSize: 13, color: '#666' }}>Currency</label>
                      <select
                        className="config-input"
                        style={{ minWidth: 80 }}
                        value={editing.currency}
                        onChange={(e) => setEditing({ ...editing, currency: e.target.value })}
                      >
                        {SUPPORTED_CURRENCIES.map((c) => <option key={c} value={c}>{c}</option>)}
                      </select>
                    </div>
                  </div>
                  <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                    <label style={{ fontSize: 13, color: '#666' }}>
                      Price per share ({editing.currency !== 'AUD' ? editing.currency : 'AUD'})
                    </label>
                    <input type="number" min="0" step="any" className="config-input" value={editing.price} onChange={(e) => setEditing({ ...editing, price: e.target.value })} />
                  </div>
                  {editing.currency !== 'AUD' && (
                    <div style={{ fontSize: 13, color: '#666', display: 'flex', alignItems: 'center', gap: 8 }}>
                      {editFxLoading && <span>Fetching {editing.currency}/AUD rate…</span>}
                      {!editFxLoading && editFxRate && editing.price && !isNaN(parseFloat(editing.price)) && (
                        <>
                          <span>Rate: 1 {editing.currency} = {editFxRate.toFixed(4)} AUD{editFxDate ? ` (${editFxDate})` : ''}</span>
                          <span style={{ fontWeight: 600, color: '#333' }}>
                            → AUD {(parseFloat(editing.price) * editFxRate).toFixed(4)} per share
                          </span>
                        </>
                      )}
                      {!editFxLoading && !editFxRate && (
                        <span style={{ color: '#e53935' }}>Could not fetch {editing.currency}/AUD rate for {editing.date}</span>
                      )}
                    </div>
                  )}
                  <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                    <label style={{ fontSize: 13, color: '#666' }}>Brokerage</label>
                    <input type="number" min="0" step="0.01" className="config-input" value={editing.brokerage} onChange={(e) => setEditing({ ...editing, brokerage: e.target.value })} />
                  </div>
                </>
              )}
              {editing.type === 'dividend' && (
                <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                  <label style={{ fontSize: 13, color: '#666' }}>Amount</label>
                  <input type="number" min="0" step="0.01" className="config-input" value={editing.amount} onChange={(e) => setEditing({ ...editing, amount: e.target.value })} />
                </div>
              )}
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 13, color: '#666' }}>Notes</label>
                <input type="text" className="config-input" value={editing.notes} onChange={(e) => setEditing({ ...editing, notes: e.target.value })} />
              </div>
            </div>
            <div style={{ display: 'flex', gap: 10, marginTop: 22, justifyContent: 'flex-end' }}>
              <button className="btn btn-secondary" onClick={() => setEditing(null)} disabled={saving}>Cancel</button>
              <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
                {saving ? 'Saving…' : 'Save'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
