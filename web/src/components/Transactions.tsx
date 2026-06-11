import { useEffect, useMemo, useState } from 'react'
import { apiClient } from '../services/api'

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
    setEditing({
      id: row.id,
      symbol: row.symbol,
      type: row.type,
      date: row.date,
      quantity: row.quantity !== null ? row.quantity.toString() : '',
      price: row.price !== null ? row.price.toString() : '',
      amount: row.amount !== null ? row.amount.toString() : '',
      brokerage: row.brokerage !== null ? row.brokerage.toString() : '',
      notes: row.notes ?? '',
    })
  }

  const handleSave = async () => {
    if (!editing) return
    try {
      setSaving(true)
      const updated = await apiClient.updateHoldingTransaction(editing.id, {
        symbol: editing.symbol,
        transaction_type: editing.type,
        date: editing.date,
        quantity: editing.quantity ? parseFloat(editing.quantity) : undefined,
        price: editing.price ? parseFloat(editing.price) : undefined,
        amount: editing.amount ? parseFloat(editing.amount) : undefined,
        brokerage: editing.brokerage ? parseFloat(editing.brokerage) : undefined,
        notes: editing.notes || undefined,
      })
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
                    <td><strong>{row.symbol}</strong></td>
                    <td>
                      <span style={{ color: typeColor(row.type), fontWeight: 600 }}>
                        {typeLabel(row.type)}
                      </span>
                    </td>
                    <td>{row.quantity !== null ? row.quantity.toFixed(2) : '—'}</td>
                    <td>{row.price !== null ? `$${row.price.toFixed(4)}` : '—'}</td>
                    <td>
                      {row.type === 'dividend' && row.amount !== null
                        ? `$${row.amount.toFixed(4)} per share`
                        : row.quantity !== null && row.price !== null
                        ? `$${(row.quantity * row.price).toFixed(2)}`
                        : '—'}
                    </td>
                    <td>{row.brokerage !== null ? `$${row.brokerage.toFixed(2)}` : '—'}</td>
                    <td style={{ color: '#888' }}>{row.notes || '—'}</td>
                    <td>
                      {row.id !== null && (
                        <button
                          className="btn btn-secondary btn-small"
                          onClick={() => startEdit(row)}
                        >
                          Edit
                        </button>
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
                    <div style={{ flex: 1, display: 'flex', flexDirection: 'column', gap: 4 }}>
                      <label style={{ fontSize: 13, color: '#666' }}>Price per share</label>
                      <input type="number" min="0" step="any" className="config-input" value={editing.price} onChange={(e) => setEditing({ ...editing, price: e.target.value })} />
                    </div>
                  </div>
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
