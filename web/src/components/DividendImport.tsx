import { useState } from 'react'
import {
  apiClient,
  type DividendImportOptions,
  type DividendImportResult,
  type DividendImportRow,
} from '../services/api'

// Thin client: the API matches every row to a recorded dividend and says what
// it would change (POST /api/dividends/import). This dialog reads the file,
// shows that plan, and sends back the lines the user ticked.

const COLUMNS: { name: string; note: string }[] = [
  { name: 'symbol', note: 'required — ADH or ADH.AX' },
  { name: 'ex_date', note: 'required — YYYY-MM-DD, DD/MM/YYYY or 10-Mar-2026' },
  { name: 'payment_date', note: 'when it was paid' },
  { name: 'dividend_type', note: 'Interim, Final, Special…' },
  { name: 'dividend_per_share', note: '' },
  { name: 'units', note: 'shares held' },
  { name: 'unfranked_amount', note: '' },
  { name: 'franked_amount', note: '' },
  { name: 'total_amount', note: 'or dividend_per_share × units' },
  { name: 'franking_credit', note: '' },
  { name: 'withholding_tax', note: 'tax taken before payment' },
  { name: 'currency', note: 'AUD if left out' },
]

const TEMPLATE = `${COLUMNS.map((c) => c.name).join(',')}
ADH.AX,2026-03-10,2026-04-07,Interim,0.0550,750,0.00,41.25,41.25,17.68,,AUD
`

const STATUS_LABEL: Record<DividendImportRow['status'], string> = {
  update: 'Update',
  create: 'Record new',
  unchanged: 'No change',
  error: "Can't import",
}

function downloadTemplate() {
  const url = URL.createObjectURL(new Blob([TEMPLATE], { type: 'text/csv' }))
  const link = document.createElement('a')
  link.href = url
  link.download = 'dividend-import-template.csv'
  document.body.appendChild(link)
  link.click()
  link.remove()
  setTimeout(() => URL.revokeObjectURL(url), 1000)
}

function errorMessage(err: unknown, fallback: string): string {
  return err instanceof Error ? err.message : fallback
}

export default function DividendImport({ onClose, onImported }: { onClose: () => void; onImported: () => void }) {
  const [fileName, setFileName] = useState<string | null>(null)
  const [csv, setCsv] = useState<string | null>(null)
  const [options, setOptions] = useState<DividendImportOptions>({ shortfall_is_withholding: false })
  const [plan, setPlan] = useState<DividendImportResult | null>(null)
  const [selected, setSelected] = useState<Set<number>>(new Set())
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [done, setDone] = useState<string | null>(null)

  const preview = async (text: string, opts: DividendImportOptions) => {
    setBusy(true)
    setError(null)
    setDone(null)
    try {
      const result = await apiClient.importDividends(text, opts)
      setPlan(result)
      setSelected(new Set(result.rows.filter((r) => r.selected).map((r) => r.line)))
    } catch (err) {
      setPlan(null)
      setError(errorMessage(err, 'Failed to read the file'))
    } finally {
      setBusy(false)
    }
  }

  const chooseFile = async (file: File | undefined) => {
    if (!file) return
    const text = await file.text()
    setFileName(file.name)
    setCsv(text)
    await preview(text, options)
  }

  const toggleOption = async (value: boolean) => {
    const next = { ...options, shortfall_is_withholding: value }
    setOptions(next)
    if (csv) await preview(csv, next)
  }

  const apply = async () => {
    if (!csv || !plan) return
    setBusy(true)
    setError(null)
    try {
      const result = await apiClient.importDividends(csv, options, { lines: [...selected] })
      setPlan(result)
      setSelected(new Set())
      setDone(`Updated ${result.updated} dividend${result.updated === 1 ? '' : 's'} and recorded ${result.created} new.`)
      onImported()
    } catch (err) {
      setError(errorMessage(err, 'The import failed; nothing was changed'))
    } finally {
      setBusy(false)
    }
  }

  const selectable = (row: DividendImportRow) => !row.applied && (row.status === 'update' || row.status === 'create')
  const toggle = (line: number) => {
    const next = new Set(selected)
    if (next.has(line)) next.delete(line)
    else next.add(line)
    setSelected(next)
  }
  const allSelectable = plan?.rows.filter(selectable) ?? []

  return (
    <div className="modal-overlay">
      <div className="modal-card import-card" role="dialog" aria-label="Import dividends">
        <div className="modal-header">
          <h3 style={{ margin: 0 }}>Import dividends</h3>
        </div>
        <div className="modal-body">
          {error && <div className="alert alert-error">❌ {error}</div>}
          {done && <div className="alert alert-success">✓ {done}</div>}

          <div className="import-file">
            <label className="btn btn-secondary btn-small import-choose">
              {fileName ? 'Choose another file…' : 'Choose a CSV file…'}
              <input
                type="file"
                accept=".csv,text/csv"
                aria-label="Dividend CSV file"
                onChange={(e) => chooseFile(e.target.files?.[0])}
                disabled={busy}
              />
            </label>
            {fileName && <span className="tax-muted">{fileName}</span>}
            <button type="button" className="tax-link" onClick={downloadTemplate}>Download a template</button>
          </div>

          {!plan && (
            <details className="import-help" open={!fileName}>
              <summary>The file's columns</summary>
              <p className="tax-muted">
                One row per dividend, with a header row. Columns may come in any order and only symbol and ex_date are
                required; anything left out keeps what is recorded. Matching is by symbol and ex-date, allowing a few days'
                difference, so a statement can correct the date.
              </p>
              <ul>
                {COLUMNS.map((c) => <li key={c.name}><code>{c.name}</code>{c.note && <span className="tax-muted"> — {c.note}</span>}</li>)}
              </ul>
            </details>
          )}

          {plan && (
            <>
              <label className="tax-check">
                <input
                  type="checkbox"
                  checked={options.shortfall_is_withholding}
                  disabled={busy}
                  onChange={(e) => toggleOption(e.target.checked)}
                />
                <span>
                  <strong>Lower totals are tax withheld</strong>
                  <span className="tax-muted">
                    {' '}— where a statement's total is below what is recorded, keep the recorded gross and record the gap as tax
                    withheld. A broker shows funds paid after foreign tax at the net amount. Symbols with a standing withholding
                    rate are read this way already, when the gap is no more than that rate.
                  </span>
                </span>
              </label>

              <p className="import-summary">
                {plan.counts.update} to update · {plan.counts.create} to record new · {plan.counts.unchanged} unchanged ·{' '}
                {plan.counts.error} can't be imported
                {allSelectable.length > 0 && (
                  <>
                    <button type="button" className="tax-link" onClick={() => setSelected(new Set(allSelectable.map((r) => r.line)))}>select all</button>
                    <button type="button" className="tax-link" onClick={() => setSelected(new Set())}>none</button>
                  </>
                )}
              </p>

              <div className="holdings-table-wrapper">
                <table className="holdings-table tax-detail-table import-table">
                  <thead>
                    <tr>
                      <th></th>
                      <th className="num">Line</th>
                      <th>Symbol</th>
                      <th>Ex-date</th>
                      <th>What happens</th>
                      <th>Changes</th>
                    </tr>
                  </thead>
                  <tbody>
                    {plan.rows.map((row) => (
                      <tr key={row.line} className={`import-${row.status}${row.applied ? ' import-applied' : ''}`}>
                        <td>
                          {selectable(row) && (
                            <input
                              type="checkbox"
                              aria-label={`Import line ${row.line}`}
                              checked={selected.has(row.line)}
                              onChange={() => toggle(row.line)}
                            />
                          )}
                        </td>
                        <td className="num">{row.line}</td>
                        <td><strong>{row.symbol}</strong></td>
                        <td>{row.ex_date}</td>
                        <td>{row.applied ? 'Done' : STATUS_LABEL[row.status]}</td>
                        <td className="import-changes">
                          {row.error && <div className="import-error">{row.error}</div>}
                          {row.changes.map((c) => (
                            <div key={c.field}>
                              <span className="tax-muted">{c.field}:</span> {c.from ?? '—'} → <strong>{c.to ?? '—'}</strong>
                            </div>
                          ))}
                          {row.warnings.map((w) => <div key={w} className="tax-guess">⚠ {w}</div>)}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </>
          )}
        </div>
        <div className="modal-footer">
          <button className="btn btn-secondary" onClick={onClose} disabled={busy}>Close</button>
          {plan && (
            <button className="btn btn-primary" onClick={apply} disabled={busy || selected.size === 0}>
              {busy ? 'Working…' : `Apply ${selected.size} selected`}
            </button>
          )}
        </div>
      </div>
    </div>
  )
}
