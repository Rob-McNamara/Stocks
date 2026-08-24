import { useEffect, useMemo, useRef, useState } from 'react'
import { apiClient, type HoldingTransactionPayload, type LedgerRow , type CashAccount } from '../services/api'
import { settlementAccountsFor, settlesByConversion } from '../utils/cash'

const SUPPORTED_CURRENCIES = ['AUD', 'USD', 'GBP', 'EUR', 'JPY', 'CAD', 'HKD', 'SGD', 'NZD']

type SortColumn = 'date' | 'symbol' | 'amount' | 'brokerage' | 'cashAccount' | 'notes'

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
  /** Settlement account, or null when the row settles against no account */
  cash_account_id: number | null
  /** true for dividend_events rows (per-share amount); false for manual dividend transactions (total dollars) */
  perShare: boolean
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
  /** Empty string means the trade settles against no account. */
  cash_account_id: string
  custom_fields: Record<string, string>
}

interface HoldingsFieldDef {
  key: string
  label: string
  type: 'text' | 'number' | 'date'
  actions: string[]
}

type FilterType = 'all' | 'purchase' | 'sale' | 'dividend'

// Thin client: the transaction/dividend-event merge (dedupe by symbol+date,
// first-purchase filter, per-share flag) is computed by the API server
// (GET /api/transactions/ledger).

export default function Transactions({ onLoading, holdingsVersion }: { onLoading: (loading: boolean) => void; holdingsVersion?: number }) {
  const [ledger, setLedger] = useState<LedgerRow[]>([])
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [filter, setFilter] = useState<FilterType>('all')
  const [symbolFilter, setSymbolFilter] = useState('')
  const [editing, setEditing] = useState<EditState | null>(null)
  const [cashAccounts, setCashAccounts] = useState<CashAccount[]>([])
  const [editFxRate, setEditFxRate] = useState<number | null>(null)
  const [editFxDate, setEditFxDate] = useState<string | null>(null)
  const [editFxLoading, setEditFxLoading] = useState(false)
  // FX rate stored on the transaction being edited — reused as long as the
  // user doesn't change currency or date, so editing other fields never
  // silently rewrites the historical rate.
  const editFxSeed = useRef<{ currency: string; date: string; rate: number | null } | null>(null)
  const [holdingsFieldDefs, setHoldingsFieldDefs] = useState<HoldingsFieldDef[]>([])
  // Server-driven currency list from /api/meta; static list is the offline fallback
  const [currencyOptions, setCurrencyOptions] = useState<string[]>([...SUPPORTED_CURRENCIES])

  const loadLedger = async () => {
    const data = await apiClient.getTransactionsLedger()
    setLedger(data.rows)
    // Settlement accounts for the edit modal's picker. Failure just hides it,
    // so an older API keeps the rest of the screen working.
    apiClient.getCashAccounts?.().then(setCashAccounts).catch(() => setCashAccounts([]))
  }

  useEffect(() => {
    const load = async () => {
      try {
        setLoading(true)
        setError(null)
        onLoading(true)
        const [, meta] = await Promise.all([
          loadLedger(),
          apiClient.getMeta(),
        ])
        setHoldingsFieldDefs((meta.holdings_custom_fields ?? []) as HoldingsFieldDef[])
        if (meta.currencies?.length) setCurrencyOptions(meta.currencies)
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
    const seed = editFxSeed.current
    if (seed && seed.rate != null && seed.currency === editing.currency && seed.date === editing.date) {
      setEditFxRate(seed.rate)
      setEditFxDate(editing.date)
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
  }, [editing?.currency, editing?.date, editing?.type])

  const [sortColumn, setSortColumn] = useState<SortColumn | null>(null)
  const [sortDirection, setSortDirection] = useState<'asc' | 'desc'>('asc')

  const handleSort = (column: SortColumn) => {
    if (sortColumn === column) {
      setSortDirection((d) => (d === 'asc' ? 'desc' : 'asc'))
    } else {
      setSortColumn(column)
      setSortDirection('asc')
    }
  }

  const sortIndicator = (column: SortColumn) => {
    if (sortColumn !== column) return ' ↕'
    return sortDirection === 'asc' ? ' ↑' : ' ↓'
  }

  const rows = useMemo((): TransactionRow[] => {
    return ledger
      .filter((r) => filter === 'all' || r.transaction_type === filter)
      .filter((r) => !symbolFilter || r.symbol.includes(symbolFilter.toUpperCase()))
      .map((r) => ({
        key: r.key,
        id: r.id,
        symbol: r.symbol,
        type: r.transaction_type,
        date: r.date,
        quantity: r.quantity,
        price: r.price,
        currency: r.currency || 'AUD',
        original_price: r.original_price,
        amount: r.amount,
        brokerage: r.brokerage,
        notes: r.per_share && r.payment_date ? `Payment: ${new Date(r.payment_date).toLocaleDateString()}` : r.notes,
        cash_account_id: r.cash_account_id,
        perShare: r.per_share,
      }))
  }, [ledger, filter, symbolFilter])

  /**
   * Sorted client-side: the ledger arrives whole, so there is nothing hidden
   * below a row limit for a re-query to reveal. (The Dashboard's stop-loss
   * table sorts on the server precisely because it *is* truncated.)
   */
  const sortedRows = useMemo((): TransactionRow[] => {
    if (!sortColumn) return rows
    const dir = sortDirection === 'asc' ? 1 : -1

    // The Amount column shows a dividend's own total but a trade's quantity x
    // price, so sorting has to read the same figure the eye does.
    const amountOf = (r: TransactionRow): number | null => {
      if (r.type === 'dividend') return r.amount
      return r.quantity !== null && r.price !== null ? r.quantity * r.price : null
    }
    const accountOf = (r: TransactionRow): string =>
      r.id === null ? 'not recorded' : (cashAccounts.find((a) => a.id === r.cash_account_id)?.name ?? '')

    const value = (r: TransactionRow): string | number | null => {
      switch (sortColumn) {
        case 'date': return r.date
        case 'symbol': return r.symbol
        case 'amount': return amountOf(r)
        case 'brokerage': return r.brokerage
        case 'cashAccount': return accountOf(r)
        case 'notes': return r.notes ?? ''
      }
    }

    return [...rows].sort((a, b) => {
      const av = value(a)
      const bv = value(b)
      // Empty cells sort last in both directions — a blank is absence, not a
      // low value, and burying rows under a run of them helps nobody.
      const aEmpty = av === null || av === ''
      const bEmpty = bv === null || bv === ''
      if (aEmpty || bEmpty) return aEmpty && bEmpty ? 0 : aEmpty ? 1 : -1
      if (typeof av === 'string' && typeof bv === 'string') return av.localeCompare(bv) * dir
      return ((av as number) - (bv as number)) * dir
    })
  }, [rows, sortColumn, sortDirection, cashAccounts])

  const startEdit = (row: TransactionRow) => {
    if (row.id === null) return
    const tx = ledger.find((t) => t.id === row.id)
    const currency = tx?.currency || 'AUD'
    const displayPrice = currency !== 'AUD' && tx?.original_price != null
      ? tx.original_price.toString()
      : (row.price !== null ? row.price.toString() : '')
    editFxSeed.current = { currency, date: row.date, rate: tx?.fx_rate ?? null }
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
      cash_account_id: tx?.cash_account_id != null ? String(tx.cash_account_id) : '',
      custom_fields: tx?.custom_fields ?? {},
    })
  }

  const handleDelete = async (id: number) => {
    if (!confirm('Delete this transaction?')) return
    try {
      setSaving(true)
      await apiClient.removeHoldingTransaction(id)
      await loadLedger()
      if (editing?.id === id) setEditing(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to delete transaction')
    } finally {
      setSaving(false)
    }
  }

  const handleSave = async () => {
    if (!editing) return
    // Never silently re-save a foreign-currency transaction as AUD
    if ((editing.type === 'purchase' || editing.type === 'sale') && editing.currency !== 'AUD' && !editFxRate) {
      setError(`No ${editing.currency}/AUD exchange rate available for ${editing.date} — cannot save. Retry, or pick a different date.`)
      return
    }
    try {
      setSaving(true)
      const payload: HoldingTransactionPayload = {
        symbol: editing.symbol,
        transaction_type: editing.type,
        date: editing.date,
        amount: editing.amount ? parseFloat(editing.amount) : undefined,
        brokerage: editing.brokerage ? parseFloat(editing.brokerage) : undefined,
        notes: editing.notes || undefined,
        // null, not undefined: clearing the picker must detach the trade from
        // the ledger rather than leave its old settlement leg in place.
        cash_account_id: editing.cash_account_id ? Number(editing.cash_account_id) : null,
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
      if (Object.keys(editing.custom_fields).length > 0) {
        payload.custom_fields = editing.custom_fields
      }
      await apiClient.updateHoldingTransaction(editing.id, payload)
      await loadLedger()
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

  /**
   * Three states, not two — the distinction matters.
   *
   * A row with no `id` is not a stored transaction at all: it is a dividend the
   * app derived from a fetched event, so it can never have an account until it
   * is recorded. Showing a bare dash there reads as "unlinked, go and link it",
   * which is not a thing the user can do.
   */
  const renderCashAccount = (row: TransactionRow) => {
    if (row.id === null) {
      return (
        <span style={{ color: '#b0741c', fontSize: 12 }} title="Derived from a fetched dividend event — record it as a transaction to settle it to an account">
          not recorded
        </span>
      )
    }
    const account = cashAccounts.find((a) => a.id === row.cash_account_id)
    if (!account) {
      return <span style={{ color: '#999' }} title="This transaction does not move cash">—</span>
    }
    return (
      <span title={`${account.name} (${account.currency})`}>
        {account.name}
      </span>
    )
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
                  <th className="sortable-header" onClick={() => handleSort('date')}>Date{sortIndicator('date')}</th>
                  <th className="sortable-header" onClick={() => handleSort('symbol')}>Symbol{sortIndicator('symbol')}</th>
                  <th>Type</th>
                  <th>Quantity</th>
                  <th>Price</th>
                  <th className="sortable-header" onClick={() => handleSort('amount')}>Amount{sortIndicator('amount')}</th>
                  <th className="sortable-header" onClick={() => handleSort('brokerage')}>Brokerage{sortIndicator('brokerage')}</th>
                  <th className="sortable-header" onClick={() => handleSort('cashAccount')}>Cash Account{sortIndicator('cashAccount')}</th>
                  <th className="sortable-header" onClick={() => handleSort('notes')}>Notes{sortIndicator('notes')}</th>
                  <th></th>
                </tr>
              </thead>
              <tbody>
                {sortedRows.map((row) => (
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
                        ? row.perShare
                          ? `$${row.amount.toFixed(4)} per share`
                          : `$${row.amount.toFixed(2)}`
                        : row.quantity !== null && row.price !== null
                        ? `$${(row.quantity * row.price).toFixed(2)}`
                        : '—'}
                    </td>
                    <td>{row.brokerage !== null ? `$${row.brokerage.toFixed(2)}` : '—'}</td>
                    <td>{renderCashAccount(row)}</td>
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
        <div className="modal-overlay">
          <div className="modal-card" style={{ borderRadius: 12, minWidth: 380, maxWidth: 480, width: '100%' }}>
            <div className="modal-header">
              <h3 style={{ margin: 0 }}>Edit {typeLabel(editing.type)} — {editing.symbol}</h3>
            </div>
            <div className="modal-body">
            <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 13, color: '#666' }}>Date</label>
                <input type="date" className="config-input" value={editing.date} onChange={(e) => setEditing({ ...editing, date: e.target.value })} />
              </div>
              {cashAccounts.length > 0 && (() => {
                const matching = settlementAccountsFor(cashAccounts, editing.currency)
                const selected = matching.some((a) => String(a.id) === editing.cash_account_id) ? editing.cash_account_id : ''
                const converts = settlesByConversion(matching.find((a) => String(a.id) === selected), editing.currency)
                return (
                  <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                    <label style={{ fontSize: 13, color: '#666' }}>Settles from</label>
                    <select
                      className="config-input"
                      value={selected}
                      disabled={matching.length === 0}
                      onChange={(e) => setEditing({ ...editing, cash_account_id: e.target.value })}
                    >
                      <option value="">
                        {matching.length === 0
                          ? `No ${editing.currency} cash account`
                          : 'No account — leaves cash untouched'}
                      </option>
                      {matching.map((a) => (
                        <option key={a.id} value={a.id}>{a.name} — {a.currency} {a.balance.toLocaleString('en-AU', { maximumFractionDigits: 2 })}</option>
                      ))}
                    </select>
                    <span style={{ fontSize: 12, color: '#888' }}>
                      {converts
                        ? `AUD leaves this account, converted from ${editing.currency} at the trade\u2019s rate.`
                        : selected
                          ? 'Saving rewrites this trade\u2019s cash movement.'
                          : 'Attaching an account makes this trade draw on cash instead of counting as a contribution.'}
                    </span>
                  </div>
                )
              })()}
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
                        {currencyOptions.map((c) => <option key={c} value={c}>{c}</option>)}
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
              {holdingsFieldDefs.filter((def) => def.actions.includes(editing.type)).map((def) => (
                <div key={def.key} style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                  <label style={{ fontSize: 13, color: '#666' }}>{def.label}</label>
                  <input
                    type={def.type}
                    className="config-input"
                    value={editing.custom_fields[def.key] ?? ''}
                    onChange={(e) => setEditing({ ...editing, custom_fields: { ...editing.custom_fields, [def.key]: e.target.value } })}
                  />
                </div>
              ))}
            </div>
            </div>
            <div className="modal-footer" style={{ gap: 10 }}>
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
