import { useEffect, useState } from 'react'
import {
  apiClient,
  type CorporateAction,
  type CorporateActionPayload,
  type MergerPlan,
} from '../services/api'
import { taxMoney as money } from '../utils/taxFormat'

// Thin client: the API checks the action against the holdings and works out
// the parcels it moves (POST /api/corporate-actions/preview). This dialog
// collects the action, shows that plan, and records or deletes actions.

interface Form {
  date: string
  from_symbol: string
  from_quantity: string
  to_symbol: string
  to_quantity: string
  rollover: boolean
  market_value: string
  notes: string
}

const EMPTY: Form = {
  date: '',
  from_symbol: '',
  from_quantity: '',
  to_symbol: '',
  to_quantity: '',
  rollover: true,
  market_value: '',
  notes: '',
}

function errorMessage(err: unknown, fallback: string): string {
  return err instanceof Error ? err.message : fallback
}

function quantity(value: number): string {
  return value.toLocaleString('en-AU', { maximumFractionDigits: 4 })
}

function toPayload(form: Form): CorporateActionPayload {
  return {
    kind: 'merger',
    date: form.date,
    from_symbol: form.from_symbol.trim().toUpperCase(),
    to_symbol: form.to_symbol.trim().toUpperCase(),
    from_quantity: Number(form.from_quantity),
    to_quantity: Number(form.to_quantity),
    rollover: form.rollover,
    market_value: form.rollover || form.market_value.trim() === '' ? null : Number(form.market_value),
    notes: form.notes.trim() || null,
  }
}

export default function CorporateActions({ symbols, onClose, onChanged }: {
  /** Symbols in the ledger, offered for both sides */
  symbols: string[]
  onClose: () => void
  onChanged: () => void
}) {
  const [actions, setActions] = useState<CorporateAction[]>([])
  const [form, setForm] = useState<Form>(EMPTY)
  // The plan and the exact form it was checked for: recording waits until
  // what is on screen has been checked.
  const [checked, setChecked] = useState<{ key: string; plan: MergerPlan } | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)

  const reload = async () => {
    setActions(await apiClient.getCorporateActions())
  }

  useEffect(() => {
    let cancelled = false
    apiClient
      .getCorporateActions()
      .then((list) => { if (!cancelled) setActions(list) })
      .catch((err) => { if (!cancelled) setError(errorMessage(err, 'Failed to load corporate actions')) })
    return () => { cancelled = true }
  }, [])

  const key = JSON.stringify(toPayload(form))
  const plan = checked?.key === key ? checked.plan : null
  const set = (patch: Partial<Form>) => setForm({ ...form, ...patch })

  const check = async () => {
    setBusy(true)
    setError(null)
    setNotice(null)
    try {
      const result = await apiClient.previewCorporateAction(toPayload(form))
      setChecked({ key, plan: result.plan })
    } catch (err) {
      setChecked(null)
      setError(errorMessage(err, 'Failed to check the merger'))
    } finally {
      setBusy(false)
    }
  }

  const record = async () => {
    setBusy(true)
    setError(null)
    try {
      const { action } = await apiClient.addCorporateAction(toPayload(form))
      await reload()
      setForm(EMPTY)
      setChecked(null)
      setNotice(`Recorded: ${quantity(action.from_quantity)} ${action.from_symbol} became ${quantity(action.to_quantity)} ${action.to_symbol} on ${action.date}.`)
      onChanged()
    } catch (err) {
      setError(errorMessage(err, 'Failed to record the merger'))
    } finally {
      setBusy(false)
    }
  }

  const remove = async (action: CorporateAction) => {
    if (!confirm(`Delete the ${action.from_symbol} → ${action.to_symbol} merger? The holdings go back to how they were before it.`)) return
    setBusy(true)
    setError(null)
    try {
      await apiClient.deleteCorporateAction(action.id)
      await reload()
      setNotice(`Deleted the ${action.from_symbol} → ${action.to_symbol} merger.`)
      onChanged()
    } catch (err) {
      setError(errorMessage(err, 'Failed to delete the merger'))
    } finally {
      setBusy(false)
    }
  }

  const complete = form.date && form.from_symbol.trim() && form.to_symbol.trim()
    && Number(form.from_quantity) > 0 && Number(form.to_quantity) > 0
    && (form.rollover || Number(form.market_value) > 0)

  return (
    <div className="modal-overlay">
      <div className="modal-card corporate-card" role="dialog" aria-label="Corporate actions">
        <div className="modal-header">
          <h3 style={{ margin: 0 }}>Corporate actions</h3>
        </div>
        <div className="modal-body">
          {error && <div className="alert alert-error">❌ {error}</div>}
          {notice && <div className="alert alert-success">✓ {notice}</div>}

          <section className="tax-section">
            <h4>Recorded</h4>
            {actions.length === 0 ? (
              <p className="tax-muted">None yet.</p>
            ) : (
              <div className="holdings-table-wrapper">
                <table className="holdings-table tax-detail-table">
                  <thead>
                    <tr><th>Date</th><th>Merger</th><th className="num">Shares</th><th>Roll-over</th><th>Notes</th><th></th></tr>
                  </thead>
                  <tbody>
                    {actions.map((a) => (
                      <tr key={a.id}>
                        <td>{a.date}</td>
                        <td><strong>{a.from_symbol}</strong> → <strong>{a.to_symbol}</strong></td>
                        <td className="num">{quantity(a.from_quantity)} → {quantity(a.to_quantity)}</td>
                        <td>{a.rollover ? 'Yes' : `No — received ${money.format(a.market_value ?? 0)}`}</td>
                        <td>{a.notes ?? ''}</td>
                        <td>
                          <button className="btn btn-danger btn-small" onClick={() => remove(a)} disabled={busy}>Delete</button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </section>

          <section className="tax-section">
            <h4>Record a merger or takeover paid in shares</h4>
            <p className="tax-muted" style={{ marginTop: 0 }}>
              The old shares end on the implementation date and the new ones begin. Use the number of new shares actually
              received (after rounding), from the holding statement.
            </p>
            <datalist id="corporate-symbols">
              {symbols.map((s) => <option key={s} value={s} />)}
            </datalist>
            <div className="corporate-grid">
              <label>
                <span>Implementation date</span>
                <input className="config-input" type="date" value={form.date} onChange={(e) => set({ date: e.target.value })} />
              </label>
              <span />
              <label>
                <span>Shares given up — symbol</span>
                <input className="config-input" list="corporate-symbols" value={form.from_symbol} placeholder="BKW.AX"
                  onChange={(e) => set({ from_symbol: e.target.value })} />
              </label>
              <label>
                <span>How many</span>
                <input className="config-input" type="number" min="0" step="any" value={form.from_quantity}
                  onChange={(e) => set({ from_quantity: e.target.value })} />
              </label>
              <label>
                <span>Shares received — symbol</span>
                <input className="config-input" list="corporate-symbols" value={form.to_symbol} placeholder="SOL.AX"
                  onChange={(e) => set({ to_symbol: e.target.value })} />
              </label>
              <label>
                <span>How many</span>
                <input className="config-input" type="number" min="0" step="any" value={form.to_quantity}
                  onChange={(e) => set({ to_quantity: e.target.value })} />
              </label>
            </div>
            <label className="tax-check">
              <input type="checkbox" checked={form.rollover} onChange={(e) => set({ rollover: e.target.checked })} />
              <span>
                <strong>Scrip-for-scrip roll-over chosen</strong>
                <span className="tax-muted"> — no capital gain now: the new shares keep the old ones' cost base and purchase dates</span>
              </span>
            </label>
            {!form.rollover && (
              <label className="tax-field">
                <span>Market value of the shares received (AUD) — what the old shares were disposed of for</span>
                <input className="config-input" type="number" min="0" step="0.01" value={form.market_value}
                  onChange={(e) => set({ market_value: e.target.value })} />
              </label>
            )}
            <label className="tax-field">
              <span>Notes</span>
              <input className="config-input" type="text" value={form.notes} onChange={(e) => set({ notes: e.target.value })} />
            </label>

            {plan && (
              <div className="corporate-plan">
                <p>
                  {quantity(plan.held)} {form.from_symbol.trim().toUpperCase()} held on {form.date}.{' '}
                  {form.rollover
                    ? `These parcels move to ${form.to_symbol.trim().toUpperCase()}, keeping their cost and purchase date:`
                    : `These parcels are disposed of, and the new shares start on ${form.date}:`}
                </p>
                <table className="holdings-table tax-detail-table">
                  <thead>
                    <tr><th>Bought</th><th className="num">Given up</th><th className="num">Received</th><th className="num">Cost base (AUD)</th><th className="num">Per new share</th></tr>
                  </thead>
                  <tbody>
                    {plan.parcels.map((p) => (
                      <tr key={`${p.purchase_id}-${p.acquired}`}>
                        <td>{p.acquired}</td>
                        <td className="num">{quantity(p.given_up)}</td>
                        <td className="num">{quantity(p.received)}</td>
                        <td className="num">{money.format(p.cost)}</td>
                        <td className="num">{money.format(p.cost / p.received)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </section>
        </div>
        <div className="modal-footer">
          <button className="btn btn-secondary" onClick={onClose} disabled={busy}>Close</button>
          <button className="btn btn-secondary" onClick={check} disabled={busy || !complete}>Check</button>
          <button className="btn btn-primary" onClick={record} disabled={busy || !plan}>
            {busy ? 'Working…' : 'Record merger'}
          </button>
        </div>
      </div>
    </div>
  )
}
