import { useEffect, useMemo, useState } from 'react'
import { apiClient, cashAccountCsvUrl, type CashAccount, type CashTransaction } from '../services/api'

/**
 * Kinds a person enters by hand. `trade_buy`/`trade_sell` are written by the
 * trade that settles them and the API refuses them here, so they are not
 * offered; `fx_out`/`fx_in` come in pairs from the transfer form.
 */
const MANUAL_KINDS: Array<{ value: string; label: string; hint: string }> = [
  { value: 'deposit', label: 'Deposit', hint: 'Money you added — excluded from growth' },
  { value: 'withdrawal', label: 'Withdrawal', hint: 'Money you took out — excluded from growth' },
  { value: 'opening_balance', label: 'Opening balance', hint: 'What the account held when you started tracking it' },
  { value: 'interest', label: 'Interest', hint: 'Counts as return' },
  { value: 'dividend', label: 'Dividend', hint: 'Counts as return' },
  { value: 'fee', label: 'Fee', hint: 'Counts as return, negative' },
  { value: 'adjustment', label: 'Adjustment', hint: 'Reconciliation against a statement' },
]

/** Kinds where a positive number entered means money leaving the account. */
const OUTGOING_KINDS = new Set(['withdrawal', 'fee'])

const CURRENCIES = ['AUD', 'USD', 'GBP', 'EUR', 'NZD', 'HKD', 'SGD', 'CAD', 'JPY']

function money(amount: number, currency: string): string {
  return `${amount < 0 ? '−' : ''}${currency} ${Math.abs(amount).toLocaleString('en-AU', {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  })}`
}

const today = () => new Date().toISOString().slice(0, 10)

export default function CashManager({ onLoading, onCashChanged }: { onLoading: (loading: boolean) => void; onCashChanged?: () => void }) {
  const [accounts, setAccounts] = useState<CashAccount[]>([])
  const [transactions, setTransactions] = useState<CashTransaction[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [success, setSuccess] = useState<string | null>(null)
  const [filterAccount, setFilterAccount] = useState<string>('')

  // New account
  const [newName, setNewName] = useState('')
  const [newCurrency, setNewCurrency] = useState('AUD')
  const [newRate, setNewRate] = useState('')
  const [newInPortfolio, setNewInPortfolio] = useState(true)

  // New transaction
  const [txAccount, setTxAccount] = useState('')
  const [txDate, setTxDate] = useState(today())
  const [txKind, setTxKind] = useState('deposit')
  const [txAmount, setTxAmount] = useState('')
  const [txNotes, setTxNotes] = useState('')

  // Inline row edit
  const [editingId, setEditingId] = useState<number | null>(null)
  const [editDate, setEditDate] = useState('')
  const [editKind, setEditKind] = useState('deposit')
  const [editAmount, setEditAmount] = useState('')
  const [editNotes, setEditNotes] = useState('')
  const [editAccount, setEditAccount] = useState('')

  // Transfer
  const [showTransfer, setShowTransfer] = useState(false)
  const [fromAccount, setFromAccount] = useState('')
  const [toAccount, setToAccount] = useState('')
  const [fromAmount, setFromAmount] = useState('')
  const [toAmount, setToAmount] = useState('')
  const [transferDate, setTransferDate] = useState(today())

  useEffect(() => {
    loadAll()
  }, [])

  useEffect(() => {
    apiClient
      .getCashTransactions(filterAccount ? Number(filterAccount) : undefined)
      .then(setTransactions)
      .catch((err) => setError(err instanceof Error ? err.message : 'Failed to load transactions'))
  }, [filterAccount])

  const loadAll = async () => {
    try {
      setLoading(true)
      onLoading(true)
      setError(null)
      const [accountRows, txRows] = await Promise.all([
        apiClient.getCashAccounts(),
        apiClient.getCashTransactions(filterAccount ? Number(filterAccount) : undefined),
      ])
      setAccounts(accountRows)
      setTransactions(txRows)
      if (!txAccount && accountRows.length > 0) setTxAccount(String(accountRows[0].id))
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load cash accounts')
    } finally {
      setLoading(false)
      onLoading(false)
    }
  }

  /** Refresh after any write, and tell the app so the Dashboard re-values. */
  const refresh = async (message: string) => {
    setSuccess(message)
    setTimeout(() => setSuccess(null), 3000)
    await loadAll()
    onCashChanged?.()
  }

  const totalAud = useMemo(
    () => accounts.filter((a) => a.include_in_portfolio).reduce((sum, a) => sum + (a.balance_aud ?? 0), 0),
    [accounts],
  )

  const handleAddAccount = async () => {
    try {
      setError(null)
      await apiClient.addCashAccount({
        name: newName,
        currency: newCurrency,
        interest_rate: newRate ? Number(newRate) : undefined,
        include_in_portfolio: newInPortfolio,
      })
      setNewName('')
      setNewRate('')
      await refresh(`Added ${newName}`)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to add account')
    }
  }

  const handleDeleteAccount = async (account: CashAccount) => {
    if (!confirm(`Delete ${account.name}? This cannot be undone.`)) return
    try {
      setError(null)
      await apiClient.deleteCashAccount(account.id)
      await refresh(`Deleted ${account.name}`)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to delete account')
    }
  }

  const handleAddTransaction = async () => {
    const magnitude = Number(txAmount)
    if (!txAccount || !Number.isFinite(magnitude) || magnitude === 0) {
      setError('Choose an account and enter a non-zero amount')
      return
    }
    // The form takes a positive number and the kind decides the direction, so
    // nobody has to remember that a withdrawal is stored negative.
    const signed = OUTGOING_KINDS.has(txKind) ? -Math.abs(magnitude) : magnitude
    try {
      setError(null)
      await apiClient.addCashTransaction({
        account_id: Number(txAccount),
        date: txDate,
        amount: signed,
        kind: txKind,
        notes: txNotes || undefined,
      })
      setTxAmount('')
      setTxNotes('')
      await refresh('Transaction recorded')
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to record transaction')
    }
  }

  const handleTransfer = async () => {
    try {
      setError(null)
      await apiClient.addCashTransfer({
        from_account_id: Number(fromAccount),
        to_account_id: Number(toAccount),
        date: transferDate,
        from_amount: Math.abs(Number(fromAmount)),
        to_amount: Math.abs(Number(toAmount)),
      })
      setFromAmount('')
      setToAmount('')
      setShowTransfer(false)
      await refresh('Transfer recorded')
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to record transfer')
    }
  }

  const startEdit = (tx: CashTransaction) => {
    setEditingId(tx.id)
    setEditDate(tx.date)
    setEditKind(tx.kind)
    setEditAccount(String(tx.account_id))
    setEditNotes(tx.notes ?? '')
    // Shown as a positive number with the kind carrying the direction, exactly
    // as it is entered, so a withdrawal never reads as "-250" in the field.
    setEditAmount(String(Math.abs(tx.amount)))
  }

  const handleSaveEdit = async () => {
    const magnitude = Number(editAmount)
    if (!Number.isFinite(magnitude) || magnitude === 0) {
      setError('Enter a non-zero amount')
      return
    }
    const signed = OUTGOING_KINDS.has(editKind) ? -Math.abs(magnitude) : magnitude
    try {
      setError(null)
      await apiClient.updateCashTransaction(editingId!, {
        account_id: Number(editAccount),
        date: editDate,
        amount: signed,
        kind: editKind,
        notes: editNotes || undefined,
      })
      setEditingId(null)
      await refresh('Transaction updated')
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to update transaction')
    }
  }

  const handleDeleteTransaction = async (tx: CashTransaction) => {
    const extra = tx.transfer_group_id ? ' Both sides of the conversion will be removed.' : ''
    if (!confirm(`Delete this ${tx.kind} entry?${extra}`)) return
    try {
      setError(null)
      await apiClient.deleteCashTransaction(tx.id)
      await refresh('Transaction deleted')
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to delete transaction')
    }
  }

  const currencyOf = (accountId: number) => accounts.find((a) => a.id === accountId)?.currency ?? ''
  const fromCurrency = fromAccount ? currencyOf(Number(fromAccount)) : ''
  const toCurrency = toAccount ? currencyOf(Number(toAccount)) : ''
  const impliedRate =
    Number(fromAmount) > 0 && Number(toAmount) > 0 ? Number(toAmount) / Number(fromAmount) : null

  if (loading) return <p className="loading-text">Loading cash accounts...</p>

  return (
    <div className="holdings-manager">
      {error && <div className="alert alert-error">{error}</div>}
      {success && <div className="alert alert-success">{success}</div>}

      <div className="manager-card">
        <div className="card-header">
          <h2>Cash Accounts</h2>
          <span className="chart-detail">
            {accounts.length === 0 ? 'None yet' : `Total in portfolio: $${totalAud.toLocaleString('en-AU', { maximumFractionDigits: 2 })} AUD`}
          </span>
        </div>

        {accounts.length > 0 && (
          <div className="holdings-table-wrapper">
            <table className="holdings-table compact">
              <thead>
                <tr>
                  <th>Account</th>
                  <th>Currency</th>
                  <th>Balance</th>
                  <th>In AUD</th>
                  <th>Interest</th>
                  <th>Counted</th>
                  <th>Entries</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {accounts.map((account) => (
                  <tr key={account.id}>
                    <td><strong>{account.name}</strong></td>
                    <td>{account.currency}</td>
                    <td style={{ fontWeight: 600, color: account.balance < 0 ? '#c62828' : undefined }}>
                      {money(account.balance, account.currency)}
                    </td>
                    <td>{account.balance_aud === null ? '—' : `$${account.balance_aud.toLocaleString('en-AU', { maximumFractionDigits: 2 })}`}</td>
                    <td>{account.interest_rate === null ? '—' : `${account.interest_rate}%`}</td>
                    <td title={account.include_in_portfolio ? 'Counts toward portfolio value and growth' : 'Tracked but not treated as invested capital'}>
                      {account.include_in_portfolio ? 'Yes' : 'No'}
                    </td>
                    <td>{account.transaction_count}</td>
                    <td style={{ display: 'flex', gap: 6 }}>
                      {/* A plain link, not a fetch: the server names the file
                          and marks it an attachment, so the browser saves it
                          without the page having to assemble a blob. */}
                      <a
                        className="btn btn-secondary btn-small"
                        href={cashAccountCsvUrl(account.id)}
                        title={account.transaction_count > 0
                          ? `Download ${account.name} as CSV with a running balance`
                          : 'No transactions to export'}
                        style={account.transaction_count === 0
                          ? { pointerEvents: 'none', opacity: 0.5 }
                          : undefined}
                      >
                        Export CSV
                      </a>
                      <button
                        className="btn btn-danger btn-small"
                        onClick={() => handleDeleteAccount(account)}
                        title={account.transaction_count > 0 ? 'Delete its transactions first' : 'Delete this account'}
                        disabled={account.transaction_count > 0}
                      >
                        Delete
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        <div className="add-symbol-form" style={{ marginTop: 16 }}>
          <div className="form-group">
            <input
              className="symbol-input"
              placeholder="Account name (e.g. CommSec AUD)"
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
            />
            <select className="symbol-input" value={newCurrency} onChange={(e) => setNewCurrency(e.target.value)}>
              {CURRENCIES.map((c) => <option key={c} value={c}>{c}</option>)}
            </select>
            <input
              className="symbol-input"
              type="number"
              step="0.01"
              placeholder="Interest rate % (optional)"
              value={newRate}
              onChange={(e) => setNewRate(e.target.value)}
            />
            <label style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 13, whiteSpace: 'nowrap' }}>
              <input type="checkbox" checked={newInPortfolio} onChange={(e) => setNewInPortfolio(e.target.checked)} />
              Count in portfolio
            </label>
            <button className="btn btn-primary" onClick={handleAddAccount} disabled={!newName.trim()}>
              Add Account
            </button>
          </div>
        </div>
      </div>

      {accounts.length > 0 && (
        <div className="manager-card" style={{ marginTop: 24 }}>
          <div className="card-header">
            <h2>Record a Transaction</h2>
            <button className="btn btn-secondary btn-small" onClick={() => setShowTransfer((v) => !v)}>
              {showTransfer ? 'Cancel transfer' : 'Move between accounts'}
            </button>
          </div>

          {showTransfer ? (
            <div className="add-symbol-form">
              <div className="form-group">
                <select className="symbol-input" value={fromAccount} onChange={(e) => setFromAccount(e.target.value)}>
                  <option value="">From account…</option>
                  {accounts.map((a) => <option key={a.id} value={a.id}>{a.name} ({a.currency})</option>)}
                </select>
                <input
                  className="symbol-input"
                  type="number"
                  step="0.01"
                  placeholder={fromCurrency ? `Amount out (${fromCurrency})` : 'Amount out'}
                  value={fromAmount}
                  onChange={(e) => setFromAmount(e.target.value)}
                />
                <select className="symbol-input" value={toAccount} onChange={(e) => setToAccount(e.target.value)}>
                  <option value="">To account…</option>
                  {accounts.map((a) => <option key={a.id} value={a.id}>{a.name} ({a.currency})</option>)}
                </select>
                <input
                  className="symbol-input"
                  type="number"
                  step="0.01"
                  placeholder={toCurrency ? `Amount in (${toCurrency})` : 'Amount in'}
                  value={toAmount}
                  onChange={(e) => setToAmount(e.target.value)}
                />
                <input className="symbol-input" type="date" value={transferDate} onChange={(e) => setTransferDate(e.target.value)} />
                <button
                  className="btn btn-primary"
                  onClick={handleTransfer}
                  disabled={!fromAccount || !toAccount || fromAccount === toAccount || !fromAmount || !toAmount}
                >
                  Transfer
                </button>
              </div>
              <p className="chart-detail" style={{ margin: 0 }}>
                Both sides are recorded together, so a conversion never leaves money stranded.
                {impliedRate !== null && fromCurrency && toCurrency && fromCurrency !== toCurrency && (
                  <> Implied rate: 1 {fromCurrency} = {impliedRate.toFixed(4)} {toCurrency}.</>
                )}
              </p>
            </div>
          ) : (
            <div className="add-symbol-form">
              <div className="form-group">
                <select className="symbol-input" value={txAccount} onChange={(e) => setTxAccount(e.target.value)}>
                  {accounts.map((a) => <option key={a.id} value={a.id}>{a.name} ({a.currency})</option>)}
                </select>
                <select className="symbol-input" value={txKind} onChange={(e) => setTxKind(e.target.value)}>
                  {MANUAL_KINDS.map((k) => <option key={k.value} value={k.value}>{k.label}</option>)}
                </select>
                <input
                  className="symbol-input"
                  type="number"
                  step="0.01"
                  placeholder={OUTGOING_KINDS.has(txKind) ? 'Amount leaving' : 'Amount'}
                  value={txAmount}
                  onChange={(e) => setTxAmount(e.target.value)}
                />
                <input className="symbol-input" type="date" value={txDate} onChange={(e) => setTxDate(e.target.value)} />
                <input
                  className="symbol-input"
                  placeholder="Notes (optional)"
                  value={txNotes}
                  onChange={(e) => setTxNotes(e.target.value)}
                />
                <button className="btn btn-primary" onClick={handleAddTransaction} disabled={!txAmount}>
                  Record
                </button>
              </div>
              <p className="chart-detail" style={{ margin: 0 }}>
                {MANUAL_KINDS.find((k) => k.value === txKind)?.hint}
                {OUTGOING_KINDS.has(txKind) && ' — enter a positive number; the direction is set by the type.'}
              </p>
            </div>
          )}
        </div>
      )}

      <div className="manager-card" style={{ marginTop: 24 }}>
        <div className="card-header">
          <h2>Ledger</h2>
          <select className="symbol-input" style={{ maxWidth: 240 }} value={filterAccount} onChange={(e) => setFilterAccount(e.target.value)}>
            <option value="">All accounts</option>
            {accounts.map((a) => <option key={a.id} value={a.id}>{a.name}</option>)}
          </select>
        </div>

        {transactions.length === 0 ? (
          <p className="empty-text">No cash transactions yet.</p>
        ) : (
          <div className="holdings-table-wrapper resizable-table-wrapper">
            <table className="holdings-table">
              <thead style={{ position: 'sticky', top: 0, zIndex: 1 }}>
                <tr style={{ background: '#fff' }}>
                  <th>Date</th>
                  <th>Account</th>
                  <th>Type</th>
                  <th>Amount</th>
                  <th>Notes</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {transactions.map((tx) => {
                  const fromTrade = tx.holding_tx_id !== null
                  const fromTransfer = tx.transfer_group_id !== null
                  // Both legs of a conversion move together, and a trade owns
                  // its settlement, so neither can be edited row by row.
                  const locked = fromTrade || fromTransfer
                  const lockReason = fromTrade
                    ? 'Written by the trade it settles — edit the trade instead'
                    : 'One leg of a transfer — delete it and record the transfer again'

                  if (editingId === tx.id) {
                    return (
                      <tr key={tx.id} style={{ background: '#f4f7ff' }}>
                        <td>
                          <input className="symbol-input" type="date" value={editDate} onChange={(e) => setEditDate(e.target.value)} />
                        </td>
                        <td>
                          <select className="symbol-input" value={editAccount} onChange={(e) => setEditAccount(e.target.value)}>
                            {accounts.map((a) => <option key={a.id} value={a.id}>{a.name}</option>)}
                          </select>
                        </td>
                        <td>
                          <select className="symbol-input" value={editKind} onChange={(e) => setEditKind(e.target.value)}>
                            {MANUAL_KINDS.map((k) => <option key={k.value} value={k.value}>{k.label}</option>)}
                          </select>
                        </td>
                        <td>
                          <input
                            className="symbol-input"
                            type="number"
                            step="0.01"
                            value={editAmount}
                            onChange={(e) => setEditAmount(e.target.value)}
                            title={OUTGOING_KINDS.has(editKind) ? 'Positive number; the type makes it an outgoing' : undefined}
                          />
                        </td>
                        <td>
                          <input className="symbol-input" value={editNotes} onChange={(e) => setEditNotes(e.target.value)} placeholder="Notes" />
                        </td>
                        <td style={{ whiteSpace: 'nowrap' }}>
                          <button className="btn btn-primary btn-small" onClick={handleSaveEdit}>Save</button>
                          <button className="btn btn-secondary btn-small" style={{ marginLeft: 6 }} onClick={() => setEditingId(null)}>Cancel</button>
                        </td>
                      </tr>
                    )
                  }

                  return (
                    <tr key={tx.id}>
                      <td>{tx.date}</td>
                      <td>{tx.account_name}</td>
                      <td>
                        {tx.kind.replace(/_/g, ' ')}
                        {locked && (
                          <span
                            title={lockReason}
                            style={{ fontSize: 10, marginLeft: 6, padding: '1px 4px', borderRadius: 3, background: '#eef1f7', color: '#5a6478' }}
                          >
                            {fromTrade ? 'from trade' : 'transfer'}
                          </span>
                        )}
                      </td>
                      <td style={{ fontWeight: 600, color: tx.amount < 0 ? '#c62828' : '#2e7d32' }}>
                        {money(tx.amount, tx.currency)}
                      </td>
                      <td>{tx.notes ?? '—'}</td>
                      <td style={{ whiteSpace: 'nowrap' }}>
                        <button
                          className="btn btn-secondary btn-small"
                          onClick={() => startEdit(tx)}
                          disabled={locked}
                          title={locked ? lockReason : 'Edit this entry'}
                        >
                          Edit
                        </button>
                        <button
                          className="btn btn-danger btn-small"
                          style={{ marginLeft: 6 }}
                          onClick={() => handleDeleteTransaction(tx)}
                          disabled={fromTrade}
                          title={fromTrade ? 'Delete the trade instead' : fromTransfer ? 'Deletes both legs of the transfer' : 'Delete this entry'}
                        >
                          Delete
                        </button>
                      </td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  )
}
