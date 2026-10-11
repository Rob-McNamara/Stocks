import type { TaxReport, TaxTable } from '../services/api'
import { formatTaxCell, isNumericTaxColumn, taxMoney } from '../utils/taxFormat'

/**
 * The report laid out for paper: every table open, nothing to click, and a
 * closing worksheet with a box for each figure the app doesn't know yet.
 *
 * Rendered outside the app's root (see TaxReport), so printing hides the app
 * whole rather than fighting the dialog's scrolling and fixed positioning.
 * Hidden on screen by the stylesheet; shown only by `@media print`.
 */
function PrintTable({ table }: { table: TaxTable }) {
  return (
    <>
      <h3>{table.title}</h3>
      {table.rows.length === 0 ? (
        <p className="tax-print-muted">None.</p>
      ) : (
        <table>
          <thead>
            <tr>
              {table.columns.map((c) => (
                <th key={c.label} className={isNumericTaxColumn(c.kind) ? 'num' : undefined}>{c.label}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {table.rows.map((row, r) => (
              <tr key={r}>
                {row.map((cell, c) => {
                  const kind = table.columns[c].kind
                  return (
                    <td key={c} className={kind === 'entry' ? 'tax-entry' : isNumericTaxColumn(kind) ? 'num' : undefined}>
                      {formatTaxCell(cell, kind)}
                    </td>
                  )
                })}
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </>
  )
}

export default function TaxReportPrint({ report }: { report: TaxReport }) {
  return (
    <div className="tax-print">
      <h1>Tax report — {report.label} income year</h1>
      <p>
        {report.start} to {report.end} · {report.entity === 'individual' ? 'Individual' : report.entity} return ·
        amounts in AUD unless a column says otherwise · generated {report.generated_at}
      </p>
      <p className="tax-print-muted">Worked out from the records in the Stocks app. Check every figure before lodging.</p>
      {report.ownership.map((line) => <p key={line}><strong>{line}</strong></p>)}

      {report.checks.length > 0 && (
        <>
          <h2>Needs attention</h2>
          <ul>
            {report.checks.map((check, i) => (
              <li key={i}>{check.severity === 'warn' ? '⚠ ' : ''}{check.message}</li>
            ))}
          </ul>
        </>
      )}

      {report.sections.map((section) => (
        <section key={section.key}>
          <h2>{section.title}</h2>
          <table className="tax-print-summary">
            <tbody>
              {section.summary.map((line, i) => (
                <tr key={i}>
                  <td className="tax-print-item">{line.item ?? ''}</td>
                  <td>{line.label}</td>
                  <td className="num">{line.value ?? (line.amount !== null ? taxMoney.format(line.amount) : '')}</td>
                  <td className="tax-print-muted">{line.note ?? ''}</td>
                </tr>
              ))}
            </tbody>
          </table>
          {section.tables.map((table) => <PrintTable key={table.title} table={table} />)}
          {section.notes.map((note, i) => <p key={i} className="tax-print-muted">Note: {note}</p>)}
        </section>
      ))}

      {report.worksheet.length > 0 && (
        <section className="tax-print-worksheet">
          <h2>To complete by hand</h2>
          <p>
            What the app doesn't know yet. Write it in from the statements, then enter it in the app and generate the
            report again — or give this page to whoever prepares the return.
          </p>
          {report.worksheet.map((table) => <PrintTable key={table.title} table={table} />)}
        </section>
      )}
    </div>
  )
}
