import { useEffect, useState } from 'react'
import { createPortal } from 'react-dom'
import {
  apiClient,
  downloadTaxReportCsv,
  TAX_SECTION_KEYS,
  type AccountTaxSettings,
  type IncomeKind,
  type PartialOwnership,
  type SymbolTaxSettings,
  type TaxColumnKind,
  type TaxReport as TaxReportData,
  type TaxReportParams,
  type TaxReportSection,
  type TaxSectionKey,
  type TaxStatementFigures,
  type TaxStatementRow,
  type TaxSummaryLine,
  type TaxTable,
  type TaxYearOption,
} from '../services/api'
import { formatTaxCell, isNumericTaxColumn, taxMoney as money } from '../utils/taxFormat'
import TaxReportPrint from './TaxReportPrint'

// Thin client: every figure, the year a payment falls in, and the copied
// text are worked out by the API (GET /api/tax/report). This screen chooses
// what to ask for, lays the answer out, and edits the inputs it depends on.

const SECTION_OPTIONS: { key: TaxSectionKey; label: string; hint: string }[] = [
  { key: 'cgt', label: 'Capital gains', hint: 'Every sale, parcel by parcel, with the 12-month discount' },
  { key: 'dividends', label: 'Australian dividends', hint: 'Franked and unfranked amounts, franking credits, TFN withheld' },
  { key: 'foreign', label: 'Foreign income', hint: 'Foreign dividends and the tax withheld from them' },
  { key: 'interest', label: 'Interest', hint: 'Interest credited to your cash accounts' },
  { key: 'trusts', label: 'Trust and ETF distributions', hint: 'Annual tax statements and AMIT cost base adjustments' },
  { key: 'deductions', label: 'Possible deductions', hint: 'Margin loan interest and account fees' },
  { key: 'checks', label: 'Needs attention', hint: 'Missing franking, unrecorded dividends, changes since the last export' },
  { key: 'planning', label: 'This year so far', hint: 'Unrealised gains, and when each parcel qualifies for the discount' },
]

const INCOME_KINDS: { value: IncomeKind; label: string }[] = [
  { value: 'company', label: 'Australian company' },
  { value: 'trust', label: 'Trust / ETF' },
  { value: 'foreign', label: 'Foreign' },
]

const STATEMENT_FIELDS: { key: Exclude<keyof TaxStatementFigures, 'notes'>; item: string; label: string }[] = [
  { key: 'non_primary_income', item: '13U', label: 'Non-primary production income' },
  { key: 'franked_distributions', item: '13C', label: 'Franked distributions' },
  { key: 'franking_credits', item: '13Q', label: 'Franking credits' },
  { key: 'tfn_withheld', item: '13R', label: 'TFN amounts withheld' },
  { key: 'total_capital_gains', item: '18H', label: 'Total current year capital gains' },
  { key: 'net_capital_gain', item: '18A', label: 'Net capital gain' },
  { key: 'assessable_foreign_income', item: '20E', label: 'Assessable foreign source income' },
  { key: 'other_net_foreign_income', item: '20M', label: 'Other net foreign source income' },
  { key: 'foreign_tax_offset', item: '20O', label: 'Foreign income tax offset' },
  { key: 'amit_increase', item: 'AMIT', label: 'Cost base increase (shortfall)' },
  { key: 'amit_decrease', item: 'AMIT', label: 'Cost base decrease (excess)' },
]

function errorMessage(err: unknown, fallback: string): string {
  return err instanceof Error ? err.message : fallback
}

/**
 * Copy text, falling back to a hidden textarea where the async clipboard API
 * is missing or refuses (an older Safari, or a page not served over https or
 * localhost). The text is already in hand, so the call stays inside the
 * click that asked for it — Safari rejects a clipboard write made after an
 * await.
 */
async function copyToClipboard(text: string): Promise<void> {
  if (navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text)
      return
    } catch {
      // fall through to the textarea route
    }
  }
  const area = document.createElement('textarea')
  area.value = text
  area.setAttribute('readonly', '')
  area.style.position = 'fixed'
  area.style.opacity = '0'
  document.body.appendChild(area)
  area.select()
  const copied = document.execCommand('copy')
  area.remove()
  if (!copied) throw new Error('The browser would not copy to the clipboard')
}

function parseOptional(value: string): number | null {
  const trimmed = value.trim()
  if (trimmed === '') return null
  const n = Number(trimmed)
  return Number.isFinite(n) ? n : null
}

function SummaryLines({ lines }: { lines: TaxSummaryLine[] }) {
  return (
    <table className="tax-summary">
      <tbody>
        {lines.map((line, i) => (
          <tr key={i} className={line.item ? 'tax-summary-labelled' : undefined}>
            <td className="tax-summary-item">{line.item ?? ''}</td>
            <td>{line.label}</td>
            <td className={`tax-summary-amount${line.amount !== null && line.amount < 0 ? ' negative' : ''}`}>
              {line.value ?? (line.amount !== null ? money.format(line.amount) : '')}
            </td>
            <td className="tax-summary-note">{line.note ?? ''}</td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}

function ReportTable({ table, open }: { table: TaxTable; open?: boolean }) {
  const numeric = (kind: TaxColumnKind) => isNumericTaxColumn(kind)
  return (
    <details className="tax-table" open={open ?? (table.rows.length > 0 && table.rows.length <= 12)}>
      <summary>
        {table.title} <span className="tax-muted">({table.rows.length} row{table.rows.length === 1 ? '' : 's'})</span>
      </summary>
      {table.rows.length === 0 ? (
        <p className="tax-muted">None.</p>
      ) : (
        <div className="holdings-table-wrapper">
          <table className="holdings-table tax-detail-table">
            <thead>
              <tr>
                {table.columns.map((c) => (
                  <th key={c.label} className={numeric(c.kind) ? 'num' : undefined}>{c.label}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {table.rows.map((row, r) => (
                <tr key={r}>
                  {row.map((cell, c) => {
                    const kind = table.columns[c].kind
                    const negative = kind === 'money' && typeof cell === 'number' && cell < 0
                    return (
                      <td key={c} className={`${numeric(kind) ? 'num' : ''}${negative ? ' negative' : ''}${kind === 'entry' ? ' tax-entry' : ''}`}>
                        {formatTaxCell(cell, kind)}
                      </td>
                    )
                  })}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </details>
  )
}

function SymbolSettingsRow({ row, onSave, saving }: {
  row: SymbolTaxSettings
  onSave: (symbol: string, kind: IncomeKind, franking: number | null, withholding: number | null, share: number | null) => void
  saving: boolean
}) {
  const [kind, setKind] = useState<IncomeKind>(row.income_kind)
  const [franking, setFranking] = useState(row.franking_pct?.toString() ?? '')
  const [withholding, setWithholding] = useState(row.withholding_pct?.toString() ?? '')
  const [share, setShare] = useState(row.share_pct?.toString() ?? '')
  const dirty = kind !== row.income_kind
    || parseOptional(franking) !== row.franking_pct
    || parseOptional(withholding) !== row.withholding_pct
    || parseOptional(share) !== row.share_pct
  const excluded = row.share_pct === 0
  return (
    <tr className={excluded ? 'tax-excluded' : undefined}>
      <td>
        <strong>{row.symbol}</strong>
        {row.name && <div className="tax-muted">{row.name}</div>}
      </td>
      <td>{row.currency}</td>
      <td>
        <select className="config-input" value={kind} onChange={(e) => setKind(e.target.value as IncomeKind)} aria-label={`${row.symbol} income type`}>
          {INCOME_KINDS.map((k) => <option key={k.value} value={k.value}>{k.label}</option>)}
        </select>
        {!row.income_kind_set && <div className="tax-guess">worked out — confirm</div>}
      </td>
      <td>
        <input className="config-input tax-pct" type="number" min="0" max="100" step="any" value={franking}
          onChange={(e) => setFranking(e.target.value)} placeholder="—" aria-label={`${row.symbol} franking %`} />
      </td>
      <td>
        <input className="config-input tax-pct" type="number" min="0" max="100" step="any" value={withholding}
          onChange={(e) => setWithholding(e.target.value)} placeholder="—" aria-label={`${row.symbol} withholding %`} />
      </td>
      <td>
        <input className="config-input tax-pct" type="number" min="0" max="100" step="any" value={share}
          onChange={(e) => setShare(e.target.value)} placeholder="100" aria-label={`${row.symbol} your share %`} />
        {excluded && <div className="tax-guess">left out</div>}
      </td>
      <td>
        <button
          className="btn btn-secondary btn-small"
          disabled={saving || !(dirty || !row.income_kind_set)}
          onClick={() => onSave(row.symbol, kind, parseOptional(franking), parseOptional(withholding), parseOptional(share))}
        >
          {dirty || row.income_kind_set ? 'Save' : 'Confirm'}
        </button>
      </td>
    </tr>
  )
}

function AccountShareRow({ row, onSave, saving }: {
  row: AccountTaxSettings
  onSave: (id: number, share: number | null) => void
  saving: boolean
}) {
  const [share, setShare] = useState(row.share_pct?.toString() ?? '')
  const dirty = parseOptional(share) !== row.share_pct
  return (
    <tr>
      <td><strong>{row.name}</strong></td>
      <td>{row.currency}</td>
      <td className="num">{money.format(row.interest_aud)}</td>
      <td>
        <input className="config-input tax-pct" type="number" min="0" max="100" step="any" value={share}
          onChange={(e) => setShare(e.target.value)} placeholder="100" aria-label={`${row.name} your share %`} />
      </td>
      <td>
        <button className="btn btn-secondary btn-small" disabled={saving || !dirty} onClick={() => onSave(row.id, parseOptional(share))}>
          Save
        </button>
      </td>
    </tr>
  )
}

function StatementForm({ row, fy, label, onSave, onDelete, onCancel, saving }: {
  row: TaxStatementRow
  fy: number
  label: string
  onSave: (figures: TaxStatementFigures) => void
  onDelete: () => void
  onCancel: () => void
  saving: boolean
}) {
  const [values, setValues] = useState<Record<string, string>>(() => {
    const initial: Record<string, string> = {}
    for (const f of STATEMENT_FIELDS) initial[f.key] = row.statement?.[f.key]?.toString() ?? ''
    initial.notes = row.statement?.notes ?? ''
    return initial
  })
  const submit = () => {
    const figures: TaxStatementFigures = { notes: values.notes.trim() || null }
    for (const f of STATEMENT_FIELDS) figures[f.key] = parseOptional(values[f.key])
    onSave(figures)
  }
  return (
    <div className="tax-statement-form">
      <div className="tax-statement-heading">
        <strong>{row.symbol}</strong> — {label} annual tax statement
        <span className="tax-muted"> · copy the figures from its Part A summary</span>
      </div>
      <div className="tax-statement-grid">
        {STATEMENT_FIELDS.map((f) => (
          <label key={f.key}>
            <span><span className="tax-summary-item">{f.item}</span> {f.label}</span>
            <input className="config-input" type="number" step="any" value={values[f.key]}
              onChange={(e) => setValues({ ...values, [f.key]: e.target.value })} />
          </label>
        ))}
        <label className="tax-statement-notes">
          <span>Notes</span>
          <input className="config-input" type="text" value={values.notes} onChange={(e) => setValues({ ...values, notes: e.target.value })} />
        </label>
      </div>
      <div className="tax-actions">
        {row.statement && (
          <button className="btn btn-danger btn-small" onClick={onDelete} disabled={saving}>Delete</button>
        )}
        <span style={{ flex: 1 }} />
        <button className="btn btn-secondary btn-small" onClick={onCancel} disabled={saving}>Cancel</button>
        <button className="btn btn-primary btn-small" onClick={submit} disabled={saving}>
          {saving ? 'Saving…' : `Save ${fy ? label : ''} statement`}
        </button>
      </div>
    </div>
  )
}

export default function TaxReport({ onClose }: { onClose: () => void }) {
  const [years, setYears] = useState<TaxYearOption[]>([])
  const [fy, setFy] = useState<number | null>(null)
  const [selected, setSelected] = useState<Set<TaxSectionKey>>(() => new Set(TAX_SECTION_KEYS))
  const [detail, setDetail] = useState<'full' | 'summary'>('full')
  const [losses, setLosses] = useState('')
  const [report, setReport] = useState<TaxReportData | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [editingStatement, setEditingStatement] = useState<string | null>(null)
  const [ownership, setOwnership] = useState<PartialOwnership[]>([])

  useEffect(() => {
    let cancelled = false
    apiClient
      .getTaxYears()
      .then((data) => {
        if (cancelled) return
        setYears(data.years)
        setFy(data.default_fy)
        setOwnership(data.ownership ?? [])
      })
      .catch((err) => {
        if (!cancelled) setError(errorMessage(err, 'Failed to load the income years'))
      })
    return () => { cancelled = true }
  }, [])

  const year = years.find((y) => y.fy === fy)
  // The planning view only exists for the year still in progress.
  const available = (key: TaxSectionKey) => key !== 'planning' || year?.in_progress === true

  const params = (): TaxReportParams | null => {
    if (fy === null) return null
    return {
      fy,
      sections: TAX_SECTION_KEYS.filter((k) => selected.has(k) && available(k)),
      detail,
      lossesCarriedIn: parseOptional(losses),
    }
  }

  const generate = async (successNotice?: string) => {
    const p = params()
    if (!p) return
    if (p.sections.length === 0) {
      setError('Tick at least one thing to include.')
      return
    }
    setBusy(true)
    setError(null)
    try {
      setReport(await apiClient.getTaxReport(p))
      setNotice(successNotice ?? null)
    } catch (err) {
      setError(errorMessage(err, 'Failed to generate the tax report'))
    } finally {
      setBusy(false)
    }
  }

  const recordExport = (what: string) => {
    if (!report) return
    apiClient
      .recordTaxExport(report.fy)
      .then(() => setNotice(what))
      .catch(() => setNotice(`${what} — but the export couldn't be recorded, so later changes to this year won't be flagged.`))
  }

  const copy = (text: string, what: string) => {
    copyToClipboard(text)
      .then(() => recordExport(what))
      .catch((err) => setError(errorMessage(err, 'Copy failed')))
  }

  const download = async () => {
    const p = params()
    if (!p) return
    setBusy(true)
    setError(null)
    try {
      await downloadTaxReportCsv(p)
      recordExport('Downloaded the CSV')
    } catch (err) {
      setError(errorMessage(err, 'Download failed'))
    } finally {
      setBusy(false)
    }
  }

  const saveSymbol = async (symbol: string, kind: IncomeKind, franking: number | null, withholding: number | null, share: number | null) => {
    setBusy(true)
    setError(null)
    try {
      await apiClient.updateSymbolTaxSettings(symbol, {
        income_kind: kind,
        franking_pct: franking,
        dividend_withholding_pct: withholding,
        tax_share_pct: share,
      })
    } catch (err) {
      setError(errorMessage(err, `Failed to save ${symbol}`))
      setBusy(false)
      return
    }
    await generate(`Saved ${symbol} and updated the report`)
  }

  const saveAccountShare = async (id: number, share: number | null) => {
    const name = report?.accounts.find((a) => a.id === id)?.name ?? 'the account'
    setBusy(true)
    setError(null)
    try {
      await apiClient.updateAccountTaxShare(id, share)
    } catch (err) {
      setError(errorMessage(err, `Failed to save ${name}`))
      setBusy(false)
      return
    }
    await generate(`Saved your share of ${name} and updated the report`)
  }

  const confirmAllGuesses = async () => {
    if (!report) return
    const guessed = report.symbols.filter((s) => !s.income_kind_set)
    setBusy(true)
    setError(null)
    try {
      for (const s of guessed) {
        await apiClient.updateSymbolTaxSettings(s.symbol, { income_kind: s.income_kind })
      }
    } catch (err) {
      setError(errorMessage(err, 'Failed to confirm the income types'))
      setBusy(false)
      return
    }
    await generate(`Confirmed ${guessed.length} income type${guessed.length === 1 ? '' : 's'}`)
  }

  const saveStatement = async (symbol: string, figures: TaxStatementFigures) => {
    if (!report) return
    setBusy(true)
    setError(null)
    try {
      await apiClient.saveTaxStatement(symbol, report.fy, figures)
    } catch (err) {
      setError(errorMessage(err, `Failed to save the ${symbol} statement`))
      setBusy(false)
      return
    }
    setEditingStatement(null)
    await generate(`Saved the ${symbol} statement and updated the report`)
  }

  const deleteStatement = async (symbol: string) => {
    if (!report || !confirm(`Delete the ${report.label} statement for ${symbol}?`)) return
    setBusy(true)
    setError(null)
    try {
      await apiClient.deleteTaxStatement(symbol, report.fy)
    } catch (err) {
      setError(errorMessage(err, `Failed to delete the ${symbol} statement`))
      setBusy(false)
      return
    }
    setEditingStatement(null)
    await generate(`Deleted the ${symbol} statement`)
  }

  const toggle = (key: TaxSectionKey) => {
    const next = new Set(selected)
    if (next.has(key)) next.delete(key)
    else next.add(key)
    setSelected(next)
  }

  const guessedCount = report?.symbols.filter((s) => !s.income_kind_set).length ?? 0
  const missingStatements = report?.statements.filter((s) => !s.statement).length ?? 0

  return (
    <div className="modal-overlay">
      <div className="modal-card tax-report-card" role="dialog" aria-label="Tax report">
        <div className="modal-header tax-report-header">
          <h3 style={{ margin: 0 }}>
            Tax report{report ? ` — ${report.label}` : ''}
          </h3>
          {report && (
            <div className="tax-actions">
              <button className="btn btn-secondary btn-small" onClick={() => copy(report.text, 'Copied the whole report')} disabled={busy}>Copy all</button>
              <button className="btn btn-secondary btn-small" onClick={download} disabled={busy}>Download CSV</button>
              <button className="btn btn-secondary btn-small" onClick={() => window.print()} disabled={busy}>Print</button>
              <button className="btn btn-secondary btn-small" onClick={() => { setReport(null); setNotice(null) }} disabled={busy}>Options</button>
            </div>
          )}
        </div>

        <div className="modal-body">
          {error && <div className="alert alert-error">❌ {error}</div>}
          {notice && <div className="alert alert-success">✓ {notice}</div>}

          {!report ? (
            <div className="tax-options">
              <label className="tax-field">
                <span>Income year</span>
                <select className="config-input" value={fy ?? ''} onChange={(e) => setFy(Number(e.target.value))} disabled={years.length === 0}>
                  {years.map((y) => (
                    <option key={y.fy} value={y.fy}>
                      {y.label} (1 Jul {y.fy - 1} – 30 Jun {y.fy}){y.in_progress ? ' — in progress' : ''}
                    </option>
                  ))}
                </select>
              </label>

              <fieldset className="tax-fieldset">
                <legend>
                  Include
                  <button type="button" className="tax-link" onClick={() => setSelected(new Set(TAX_SECTION_KEYS))}>all</button>
                  <button type="button" className="tax-link" onClick={() => setSelected(new Set())}>none</button>
                </legend>
                {SECTION_OPTIONS.map((option) => (
                  <label key={option.key} className={`tax-check${available(option.key) ? '' : ' disabled'}`}>
                    <input
                      type="checkbox"
                      checked={selected.has(option.key) && available(option.key)}
                      disabled={!available(option.key)}
                      onChange={() => toggle(option.key)}
                    />
                    <span>
                      <strong>{option.label}</strong>
                      <span className="tax-muted"> — {available(option.key) ? option.hint : 'only for the year in progress'}</span>
                    </span>
                  </label>
                ))}
              </fieldset>

              <fieldset className="tax-fieldset">
                <legend>Detail</legend>
                <label className="tax-check">
                  <input type="radio" name="tax-detail" checked={detail === 'full'} onChange={() => setDetail('full')} />
                  <span><strong>Every line</strong><span className="tax-muted"> — each parcel, payment and fee</span></span>
                </label>
                <label className="tax-check">
                  <input type="radio" name="tax-detail" checked={detail === 'summary'} onChange={() => setDetail('summary')} />
                  <span><strong>Totals only</strong></span>
                </label>
              </fieldset>

              {ownership.length > 0 && (
                <div className="tax-muted">
                  Not all yours (change under the symbol and account settings in the report):{' '}
                  {ownership.map((o) => `${o.name} ${o.share_pct === 0 ? 'left out' : `${o.share_pct}%`}`).join(', ')}
                </div>
              )}

              <label className="tax-field">
                <span>Net capital losses carried forward from earlier years (AUD)</span>
                <input className="config-input" type="number" min="0" step="0.01" value={losses}
                  onChange={(e) => setLosses(e.target.value)} placeholder="Worked out from the app's records" />
                <span className="tax-muted">Leave blank unless last year's return carried a different figure.</span>
              </label>
            </div>
          ) : (
            <div className="tax-results">
              <p className="tax-muted" style={{ marginTop: 0 }}>
                {report.start} to {report.end} · generated {report.generated_at}
                {report.last_exported_at && ` · last exported ${report.last_exported_at.slice(0, 10)}`}
                {report.in_progress && ' · year in progress'}
              </p>
              {report.ownership.map((line) => (
                <p key={line} className="tax-ownership">{line}</p>
              ))}

              {report.checks.length > 0 && (
                <section className="tax-section tax-attention">
                  <h4>Needs attention</h4>
                  <ul>
                    {report.checks.map((check, i) => (
                      <li key={i} className={`tax-check-${check.severity}`}>
                        <span aria-hidden="true">{check.severity === 'warn' ? '⚠' : 'ℹ'}</span> {check.message}
                      </li>
                    ))}
                  </ul>
                </section>
              )}

              {report.symbols.length > 0 && (
                <details className="tax-section tax-panel" open={guessedCount > 0}>
                  <summary>
                    <strong>Symbol tax settings</strong>
                    <span className="tax-muted"> — {report.symbols.length} symbol{report.symbols.length === 1 ? '' : 's'} with income this year{guessedCount > 0 ? `, ${guessedCount} worked out` : ''}</span>
                  </summary>
                  <p className="tax-muted">
                    The income type decides where a symbol's payments go on the return. Franking % and withholding % are standing
                    defaults; a figure entered on a dividend itself always wins. Your share is 100% unless set: 0 leaves a holding
                    out (your partner's, taxed on their return), 50 suits one held jointly.
                  </p>
                  {guessedCount > 0 && (
                    <button className="btn btn-secondary btn-small" onClick={confirmAllGuesses} disabled={busy} style={{ marginBottom: 8 }}>
                      Confirm all {guessedCount} worked-out types as shown
                    </button>
                  )}
                  <div className="holdings-table-wrapper">
                    <table className="holdings-table tax-detail-table">
                      <thead>
                        <tr><th>Symbol</th><th>Currency</th><th>Income type</th><th>Franking %</th><th>Withholding %</th><th>Your share %</th><th></th></tr>
                      </thead>
                      <tbody>
                        {report.symbols.map((row) => (
                          <SymbolSettingsRow
                            key={`${row.symbol}|${row.income_kind}|${row.income_kind_set}|${row.franking_pct}|${row.withholding_pct}|${row.share_pct}`}
                            row={row}
                            onSave={saveSymbol}
                            saving={busy}
                          />
                        ))}
                      </tbody>
                    </table>
                  </div>
                </details>
              )}

              {report.accounts.length > 0 && (
                <details className="tax-section tax-panel" open={report.accounts.some((a) => a.share_pct !== null)}>
                  <summary>
                    <strong>Cash account shares</strong>
                    <span className="tax-muted"> — {report.accounts.length} account{report.accounts.length === 1 ? '' : 's'} with interest or fees this year</span>
                  </summary>
                  <p className="tax-muted">
                    Interest on a joint account is taxed to each holder in proportion: set 50 for one you share equally. Fees and
                    interest charged on the account are counted at the same share.
                  </p>
                  <div className="holdings-table-wrapper">
                    <table className="holdings-table tax-detail-table">
                      <thead>
                        <tr><th>Account</th><th>Currency</th><th className="num">Interest this year (AUD)</th><th>Your share %</th><th></th></tr>
                      </thead>
                      <tbody>
                        {report.accounts.map((row) => (
                          <AccountShareRow key={`${row.id}|${row.share_pct}`} row={row} onSave={saveAccountShare} saving={busy} />
                        ))}
                      </tbody>
                    </table>
                  </div>
                </details>
              )}

              {report.statements.length > 0 && (
                <details className="tax-section tax-panel" open={missingStatements > 0}>
                  <summary>
                    <strong>Annual tax statements</strong>
                    <span className="tax-muted"> — {report.statements.length - missingStatements} of {report.statements.length} entered</span>
                  </summary>
                  <div className="holdings-table-wrapper">
                    <table className="holdings-table tax-detail-table">
                      <thead>
                        <tr><th>Symbol</th><th className="num">Distributions</th><th className="num">Cash (AUD)</th><th>Statement</th><th></th></tr>
                      </thead>
                      <tbody>
                        {report.statements.map((row) => (
                          <tr key={row.symbol}>
                            <td><strong>{row.symbol}</strong>{row.name && <div className="tax-muted">{row.name}</div>}</td>
                            <td className="num">{row.distributions}</td>
                            <td className="num">{money.format(row.cash_aud)}</td>
                            <td>{row.statement ? 'Entered' : <span className="tax-guess">Missing</span>}</td>
                            <td>
                              <button className="btn btn-secondary btn-small" onClick={() => setEditingStatement(row.symbol)} disabled={busy}>
                                {row.statement ? 'Edit' : 'Enter'}
                              </button>
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                  {editingStatement && (() => {
                    const row = report.statements.find((s) => s.symbol === editingStatement)
                    if (!row) return null
                    return (
                      <StatementForm
                        key={row.symbol}
                        row={row}
                        fy={report.fy}
                        label={report.label}
                        saving={busy}
                        onSave={(figures) => saveStatement(row.symbol, figures)}
                        onDelete={() => deleteStatement(row.symbol)}
                        onCancel={() => setEditingStatement(null)}
                      />
                    )
                  })()}
                </details>
              )}

              {report.sections.map((section: TaxReportSection) => (
                <section key={section.key} className="tax-section">
                  <div className="tax-section-header">
                    <h4>{section.title}</h4>
                    <button className="btn btn-secondary btn-small" onClick={() => copy(section.text, `Copied ${section.title.toLowerCase()}`)}>Copy</button>
                  </div>
                  <SummaryLines lines={section.summary} />
                  {section.tables.map((table) => <ReportTable key={table.title} table={table} />)}
                  {section.notes.length > 0 && (
                    <ul className="tax-notes">
                      {section.notes.map((note, i) => <li key={i}>{note}</li>)}
                    </ul>
                  )}
                </section>
              ))}

              {report.worksheet.length > 0 && (
                <section className="tax-section">
                  <div className="tax-section-header">
                    <h4>To complete by hand</h4>
                    <span className="tax-muted">printed after the report, with a box for each figure</span>
                  </div>
                  {report.worksheet.map((table) => <ReportTable key={table.title} table={table} open={false} />)}
                </section>
              )}
            </div>
          )}
        </div>

        <div className="modal-footer">
          <button className="btn btn-secondary" onClick={onClose} disabled={busy && !report}>Close</button>
          {!report && (
            <button className="btn btn-primary" onClick={() => generate()} disabled={busy || fy === null}>
              {busy ? 'Generating…' : 'Generate'}
            </button>
          )}
        </div>
      </div>
      {report && createPortal(<TaxReportPrint report={report} />, document.body)}
    </div>
  )
}
