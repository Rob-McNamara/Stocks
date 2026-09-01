import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { apiClient, type HindsightRow, type HindsightPoint, type HindsightStatus } from '../services/api'

// Thin client: every figure here is computed by the API (GET /api/hindsight),
// including the FIFO cost of each sale and the six later prices, so this screen
// and the Holdings screen cannot disagree about the same trade.

const WINDOWS = [
  { key: 'week1', label: '+1 week' },
  { key: 'week6', label: '+6 weeks' },
  { key: 'month3', label: '+3 months' },
  { key: 'peak', label: 'Peak since' },
  { key: 'low', label: 'Low since' },
  { key: 'current', label: 'Now' },
] as const

/**
 * What an empty cell means. Kept as words rather than symbols because the whole
 * point of the three statuses is that they are *different* — a reader who
 * cannot tell "not yet" from "never" learns nothing from either.
 */
const EMPTY_LABEL: Record<HindsightStatus, string> = {
  ok: '',
  pending: 'to come',
  delisted: 'delisted',
  no_data: 'no data',
}

const EMPTY_HINT: Record<HindsightStatus, string> = {
  ok: '',
  pending: 'This date has not arrived yet — it will fill in on its own.',
  delisted: 'The symbol stopped trading before this point could be reached.',
  no_data: 'The date has passed but no price bar is close enough to answer for it.',
}

/**
 * Green means the decision worked out. For the six windows that inverts the
 * usual convention on purpose: a price that *fell* after you sold is money you
 * kept, so it is the good outcome and reads green.
 */
function outcomeColor(delta: number | null): string {
  if (delta === null || delta === 0) return '#666'
  return delta > 0 ? '#f44336' : '#4caf50'
}

/** Never squeeze the table smaller than this, however short the window. */
const MIN_TABLE_HEIGHT = 240

type WindowKey = (typeof WINDOWS)[number]['key']
type SortState = { key: WindowKey; dir: 'desc' | 'asc' }

/**
 * Cycle a header: unsorted → biggest first → smallest first → back to the
 * default. The third step matters because the default order is meaningful
 * rather than arbitrary — most recent sale first — so there has to be a way
 * back to it short of reloading the screen.
 */
function nextSort(current: SortState | null, key: WindowKey): SortState | null {
  if (current?.key !== key) return { key, dir: 'desc' }
  return current.dir === 'desc' ? { key, dir: 'asc' } : null
}

function money(value: number, currency: string): string {
  // Currency-aware rather than a bare $, because the rows are native and an
  // AUD figure sitting beside a USD one with the same glyph invites addition.
  try {
    return new Intl.NumberFormat('en-AU', {
      style: 'currency',
      currency,
      currencyDisplay: 'narrowSymbol',
      minimumFractionDigits: 2,
      maximumFractionDigits: 2,
    }).format(value)
  } catch {
    // An unknown or malformed currency code must not take the screen down.
    return `${value.toFixed(2)} ${currency}`
  }
}

function signed(value: number, currency: string): string {
  return `${value > 0 ? '+' : value < 0 ? '−' : ''}${money(Math.abs(value), currency)}`
}

function PointCell({ point, currency }: { point: HindsightPoint | undefined; currency: string }) {
  // Defensive: a backend that predates a field must degrade to an empty cell
  // rather than taking the whole screen down.
  const status = point?.status ?? 'no_data'
  if (!point || point.value === null || point.value === undefined) {
    return (
      <td style={{ color: '#999', fontStyle: 'italic', fontSize: 12 }} title={EMPTY_HINT[status] ?? ''}>
        {EMPTY_LABEL[status] ?? 'no data'}
      </td>
    )
  }

  const delta = point.delta ?? null
  const hint = [
    `${money(point.value, currency)} on ${point.date ?? 'an unknown date'}`,
    status === 'delisted' ? 'Final price — the symbol no longer trades.' : '',
  ]
    .filter(Boolean)
    .join(' · ')

  return (
    <td title={hint} style={{ whiteSpace: 'nowrap' }}>
      <div style={{ color: outcomeColor(delta), fontWeight: 600 }}>
        {point.pct === null || point.pct === undefined
          ? money(point.value, currency)
          : `${point.pct > 0 ? '+' : point.pct < 0 ? '−' : ''}${Math.abs(point.pct).toFixed(1)}%`}
      </div>
      {delta !== null && (
        <div style={{ fontSize: 12, color: '#777' }}>{signed(delta, currency)}</div>
      )}
      {status === 'delisted' && (
        <div style={{ fontSize: 11, color: '#999', fontStyle: 'italic' }}>final</div>
      )}
    </td>
  )
}

export default function Hindsight({
  onLoading,
  holdingsVersion,
}: {
  onLoading: (loading: boolean) => void
  holdingsVersion?: number
}) {
  const [rows, setRows] = useState<HindsightRow[]>([])
  const [sort, setSort] = useState<SortState | null>(null)
  const wrapperRef = useRef<HTMLDivElement>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  /**
   * Bound the table so it scrolls inside its own box instead of scrolling the
   * screen out from under its header.
   *
   * A sticky header sticks to its scroll container, and the wrapper is already
   * one — `overflow-x: auto` makes `overflow-y` compute to `auto` as well.
   * Left unbounded it never actually scrolls, so the header never sticks;
   * `.app-content` scrolls instead and carries the header away with the card.
   *
   * Measured against `.app-content` rather than the window because that is the
   * element that actually scrolls here — the page itself never does. Its bottom
   * edge is the only honest limit, and it already accounts for the header, the
   * tab bar and the footer without this having to know their heights. A CSS
   * calc would have to guess at all three, and at the caption, which rewraps as
   * the window narrows.
   */
  const fit = useCallback(() => {
    const el = wrapperRef.current
    if (!el) return
    // No box at all while the screen sits behind an inactive tab, where the
    // panel is display:none and there is nothing to measure.
    if (el.getBoundingClientRect().top <= 0) return
    const scroller = el.closest('.app-content')
    if (!scroller) return

    // Derived from the overflow rather than from the heights of the app header,
    // the tab bar, the card padding and the caption — four numbers this has no
    // business knowing, one of which changes as the caption rewraps.
    //
    // The constraint is released before measuring, so every call starts from
    // the table's natural height. Measuring the constrained element instead
    // only ever shrinks it: `scrollHeight - clientHeight` cannot go negative,
    // so a table once squeezed to the floor by a small window would stay there
    // when the window grew again. Releasing first is what lets it grow back.
    //
    // Both reads happen inside a layout effect, so the reflow is synchronous
    // and resolved before the browser paints — the released state is never seen.
    el.style.maxHeight = ''
    const natural = el.clientHeight
    const overrun = scroller.scrollHeight - scroller.clientHeight
    el.style.maxHeight = `${Math.max(MIN_TABLE_HEIGHT, natural - overrun)}px`
  }, [])

  // Deliberately no dependency array: this has to run after *every* render,
  // because switching to this tab re-renders the parent without changing
  // anything this component owns. That render is the moment the panel stops
  // being display:none and there is finally a box to measure. An
  // IntersectionObserver would be the tidier trigger, but it does not fire
  // reliably everywhere, and a measurement that silently never runs leaves the
  // table unbounded — the exact bug this is fixing.
  useLayoutEffect(fit)

  useEffect(() => {
    window.addEventListener('resize', fit)
    return () => window.removeEventListener('resize', fit)
  }, [fit])

  useEffect(() => {
    const load = async () => {
      try {
        setLoading(true)
        setError(null)
        onLoading(true)
        setRows(await apiClient.getHindsight())
      } catch (err) {
        setError(err instanceof Error ? err.message : 'Failed to load hindsight')
      } finally {
        setLoading(false)
        onLoading(false)
      }
    }
    load()
  }, [holdingsVersion])

  /**
   * Sorted on the percentage, never the cash figure. The rows are native by
   * design, so ranking a USD amount against an AUD one would be ordering the
   * table by numbers that cannot be compared. The percentage is
   * currency-neutral, and it is also the figure the cell shows first.
   */
  const sortedRows = useMemo(() => {
    if (!sort) return rows
    const pctOf = (r: HindsightRow) => r.points?.[sort.key]?.pct ?? null
    return [...rows].sort((a, b) => {
      const av = pctOf(a)
      const bv = pctOf(b)
      // A window still to come, or one the symbol never reached, has no figure
      // at all. Those sink to the bottom whichever way the column is pointing —
      // an absent value is not a small one.
      if (av === null && bv === null) return b.sale_date.localeCompare(a.sale_date)
      if (av === null) return 1
      if (bv === null) return -1
      if (av !== bv) return sort.dir === 'desc' ? bv - av : av - bv
      // Ties fall back to the default order so the sort is deterministic.
      return b.sale_date.localeCompare(a.sale_date)
    })
  }, [rows, sort])

  /**
   * Totalled per currency and never across them. The rows are native by
   * design, so one sum over a mixed list would be a number with no meaning.
   */
  const totals = useMemo(() => {
    const byCurrency = new Map<string, { held: number; counted: number }>()
    for (const row of rows) {
      const delta = row.points?.current?.delta
      if (delta === null || delta === undefined) continue
      const entry = byCurrency.get(row.currency) ?? { held: 0, counted: 0 }
      entry.held += delta
      entry.counted += 1
      byCurrency.set(row.currency, entry)
    }
    return [...byCurrency.entries()].sort((a, b) => a[0].localeCompare(b[0]))
  }, [rows])

  return (
    <div className="hindsight">
      <div className="manager-card">
        <div className="card-header">
          <h2>Hindsight</h2>
          {totals.length > 0 && (
            <div style={{ display: 'flex', gap: 16, alignItems: 'center', flexWrap: 'wrap' }}>
              <span style={{ fontSize: 13, color: '#666' }}>Had you held everything to today:</span>
              {totals.map(([currency, { held, counted }]) => (
                <span
                  key={currency}
                  style={{ fontWeight: 600, fontSize: 15, color: outcomeColor(held) }}
                  title={`Across ${counted} ${currency} sale${counted !== 1 ? 's' : ''} with a current price`}
                >
                  {currency} {signed(held, currency)}
                </span>
              ))}
            </div>
          )}
        </div>

        <p style={{ color: '#666', fontSize: 14, marginBottom: 16 }}>
          Every sale, priced at six later moments. Each figure compares that moment
          against what the shares actually fetched, so{' '}
          <strong style={{ color: '#4caf50' }}>green means selling was the right call</strong> — the
          price fell afterwards — and{' '}
          <strong style={{ color: '#f44336' }}>red is money left on the table</strong>. Price only:
          dividends belong to whoever held the shares, and are on the Sold Stocks screen. Figures are
          in each stock&rsquo;s own currency and are never converted, so totals are kept apart.
        </p>

        {error && <div className="alert alert-error">❌ {error}</div>}

        {loading ? (
          <p className="loading-text">Loading hindsight...</p>
        ) : rows.length === 0 ? (
          <p className="empty-text">No sales recorded yet — nothing to second-guess.</p>
        ) : (
          <div className="holdings-table-wrapper" ref={wrapperRef}>
            <table className="holdings-table">
              <thead>
                <tr>
                  <th>Symbol</th>
                  <th>Sold</th>
                  <th>Qty</th>
                  <th>Sale</th>
                  <th title="FIFO cost of the shares this sale consumed, brokerage included">Cost</th>
                  <th title="What the trade itself made, measured from the purchase">Trade</th>
                  {WINDOWS.map((w) => {
                    const active = sort?.key === w.key
                    return (
                      <th
                        key={w.key}
                        aria-sort={active ? (sort.dir === 'desc' ? 'descending' : 'ascending') : 'none'}
                        style={{ padding: 0 }}
                      >
                        <button
                          type="button"
                          onClick={() => setSort((prev) => nextSort(prev, w.key))}
                          title={`Sort by ${w.label}${active && sort.dir === 'asc' ? ' — click again for the default order' : ''}`}
                          style={{
                            // A plain button so the column is reachable by
                            // keyboard and shows focus, without looking like one.
                            display: 'flex',
                            alignItems: 'center',
                            gap: 4,
                            width: '100%',
                            padding: '12px 14px',
                            background: 'none',
                            border: 'none',
                            font: 'inherit',
                            color: 'inherit',
                            cursor: 'pointer',
                            textAlign: 'left',
                          }}
                        >
                          {w.label}
                          <span style={{ fontSize: 10, color: active ? '#444' : '#bbb' }}>
                            {active ? (sort.dir === 'desc' ? '▼' : '▲') : '⇅'}
                          </span>
                        </button>
                      </th>
                    )
                  })}
                </tr>
              </thead>
              <tbody>
                {sortedRows.map((row, i) => (
                  <tr key={`${row.symbol}-${row.sale_date}-${i}`}>
                    <td>
                      <strong>{row.symbol}</strong>
                      {row.delisted_on && (
                        <span
                          style={{ marginLeft: 6, fontSize: 11, color: '#999', fontStyle: 'italic' }}
                          title={`Delisted ${row.delisted_on}`}
                        >
                          delisted
                        </span>
                      )}
                      {row.currency !== 'AUD' && (
                        <span style={{ marginLeft: 6, fontSize: 11, color: '#777' }}>{row.currency}</span>
                      )}
                    </td>
                    <td style={{ whiteSpace: 'nowrap' }}>{row.sale_date}</td>
                    <td>{row.quantity.toLocaleString('en-AU')}</td>
                    <td style={{ whiteSpace: 'nowrap' }}>{money(row.sale_price, row.currency)}</td>
                    <td style={{ whiteSpace: 'nowrap', color: row.purchase_price == null ? '#999' : undefined }}>
                      {row.purchase_price == null ? '—' : money(row.purchase_price, row.currency)}
                    </td>
                    <td
                      style={{
                        whiteSpace: 'nowrap',
                        fontWeight: 600,
                        // The trade's own outcome keeps the ordinary reading:
                        // a profit is green. Only the six windows invert.
                        color:
                          row.realised_pct == null ? '#999' : row.realised_pct >= 0 ? '#4caf50' : '#f44336',
                      }}
                    >
                      {row.realised_pct == null
                        ? '—'
                        : `${row.realised_pct >= 0 ? '+' : '−'}${Math.abs(row.realised_pct).toFixed(1)}%`}
                    </td>
                    {WINDOWS.map((w) => (
                      <PointCell key={w.key} point={row.points?.[w.key]} currency={row.currency} />
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  )
}
