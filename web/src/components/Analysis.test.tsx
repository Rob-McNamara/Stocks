// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, waitFor, within, cleanup, fireEvent } from '@testing-library/react'
import Analysis from './Analysis'
import type { RiskRow } from '../services/api'

// Analysis is a thin renderer over GET /api/portfolio/risk. PriceChart is
// stubbed out — it fetches its own history and is not what these tests cover.
vi.mock('./PriceChart', () => ({ default: () => <div data-testid="price-chart" /> }))

vi.mock('../services/api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../services/api')>()
  return {
    ...actual,
    apiClient: {
      getHoldings: vi.fn(),
      getHoldingsSymbolFields: vi.fn(),
      getSymbolInfo: vi.fn(),
      getPortfolioRisk: vi.fn(),
      getCachedPrices: vi.fn(),
      updateHoldingsSymbolFields: vi.fn(),
    },
  }
})

import { apiClient } from '../services/api'
const getPortfolioRisk = apiClient.getPortfolioRisk as ReturnType<typeof vi.fn>
const getHoldings = apiClient.getHoldings as ReturnType<typeof vi.fn>
const getHoldingsSymbolFields = apiClient.getHoldingsSymbolFields as ReturnType<typeof vi.fn>
const getSymbolInfo = apiClient.getSymbolInfo as ReturnType<typeof vi.fn>
const getCachedPrices = apiClient.getCachedPrices as ReturnType<typeof vi.fn>

function riskRow(overrides: Partial<RiskRow> & { symbol: string }): RiskRow {
  return {
    current_price: 100,
    purchase_price: 80,
    pl_pct: 25,
    stop_loss: 90,
    stop_loss_pct: 12.5,
    stop_loss_dollar: 100,
    total_invested: 800,
    shares: 100,
    is_trailing_sell: false,
    sma50: null,
    sma150: null,
    ema40w: null,
    high30d: null,
    currency: 'AUD',
    ...overrides,
  } as RiskRow
}

/**
 * TXG-like: price well above a trailing stop (+13.36%), plus a tight stop and
 * a holding with no stop at all.
 */
const ROWS: RiskRow[] = [
  riskRow({ symbol: 'MID.AX', current_price: 12.0, stop_loss: 10.0 }), // +20.00%
  riskRow({ symbol: 'TXG', current_price: 58.48, stop_loss: 51.59, is_trailing_sell: true }), // +13.36%
  riskRow({ symbol: 'TIGHT.AX', current_price: 10.2, stop_loss: 10.0 }), // +2.00%
  riskRow({ symbol: 'NONE.AX', current_price: 5.0, stop_loss: null }), // no stop → —
]

beforeEach(() => {
  cleanup()
  getPortfolioRisk.mockReset().mockResolvedValue({ rows: ROWS, totals: { total_invested: 1000, total_sl_dollar: 50 } })
  getHoldings.mockReset().mockResolvedValue([])
  getHoldingsSymbolFields.mockReset().mockResolvedValue({})
  getSymbolInfo.mockReset().mockResolvedValue([])
  getCachedPrices.mockReset().mockResolvedValue([])
})

async function renderAnalysis() {
  const result = render(<Analysis onLoading={() => {}} />)
  await waitFor(() => expect(screen.queryByText(/No active holdings/)).toBeNull())
  await waitFor(() => expect(analysisTable().querySelectorAll('tbody tr').length).toBeGreaterThan(0))
  return result
}

/**
 * Scoped to the card, since the Dashboard also renders Diff headers and the
 * symbols appear in the chart's picker too.
 */
function analysisTable(): HTMLTableElement {
  return screen.getByText('Active Holdings Analysis').closest('.manager-card')!.querySelector('table')!
}

function header(name: string): HTMLElement {
  return within(analysisTable().querySelector('thead') as HTMLElement).getByText(new RegExp(`^${name}`))
}

const label = (th: Element) => th.textContent!.replace(/[↕↑↓]/g, '').trim()

/**
 * Flatten the two-level header into one label per data column, left to right.
 * A cell spanning several columns contributes its children from the second row
 * instead of itself, so the result lines up 1:1 with the body cells.
 */
function headerLabels(): string[] {
  const rows = [...analysisTable().querySelectorAll('thead tr')]
  const second = rows[1] ? [...rows[1].children] : []
  let next = 0
  const out: string[] = []
  for (const th of rows[0].children) {
    const span = parseInt(th.getAttribute('colspan') ?? '1', 10)
    if (span > 1) {
      for (let i = 0; i < span; i++) out.push(label(second[next++]))
    } else {
      out.push(label(th))
    }
  }
  return out
}

/** Top-level group headers with the number of columns each spans. */
function headerGroups(): Array<{ label: string; span: number }> {
  return [...analysisTable().querySelectorAll('thead tr')[0].children]
    .filter((th) => parseInt(th.getAttribute('colspan') ?? '1', 10) > 1)
    .map((th) => ({ label: label(th), span: parseInt(th.getAttribute('colspan')!, 10) }))
}

/** Each group's first and last leaf-column index. */
function groupRanges(): Array<{ label: string; start: number; end: number }> {
  const out: Array<{ label: string; start: number; end: number }> = []
  let col = 0
  for (const th of analysisTable().querySelectorAll('thead tr')[0].children) {
    const span = parseInt(th.getAttribute('colspan') ?? '1', 10)
    if (span > 1) out.push({ label: label(th), start: col, end: col + span - 1 })
    col += span
  }
  return out
}

function columnIndex(name: string): number {
  return headerLabels().indexOf(name)
}

/**
 * Index of a leaf column within a named group. "Price" and "P/L%" each appear
 * twice — once under Current, once under Stop Loss — so a bare label lookup is
 * ambiguous and would silently resolve to whichever comes first.
 */
function columnIndexIn(groupLabel: string, leafLabel: string): number {
  const group = groupRanges().find((g) => g.label === groupLabel)
  if (!group) throw new Error(`no column group named ${groupLabel}`)
  const within = headerLabels().slice(group.start, group.end + 1).indexOf(leafLabel)
  if (within < 0) throw new Error(`${groupLabel} has no column named ${leafLabel}`)
  return group.start + within
}

/** The Diff cell for each row, in render order. */
function diffColumn(): string[] {
  const index = columnIndex('Diff%')
  return [...analysisTable().querySelectorAll('tbody tr')].map((row) => row.children[index].textContent!.trim())
}

/**
 * Expand the totals row into one entry per column, honouring colSpan, so a cell
 * can be checked against the header it actually sits under.
 */
function footerByColumn(): string[] {
  const cells = [...analysisTable().querySelector('tfoot tr')!.children]
  const out: string[] = []
  for (const cell of cells) {
    const span = parseInt(cell.getAttribute('colspan') ?? '1', 10)
    for (let i = 0; i < span; i++) out.push(cell.textContent!.trim())
  }
  return out
}

function symbolColumn(): string[] {
  return [...analysisTable().querySelectorAll('tbody tr')].map((row) =>
    row.children[0].textContent!.replace(/AUD$/, '').trim(),
  )
}

describe('Analysis Difference column', () => {
  it('shows the gap between current price and stop loss as a percentage of the stop', async () => {
    await renderAnalysis()
    // 12.00 vs 10.00 → +20%; 58.48 vs 51.59 → +13.36%; 10.20 vs 10.00 → +2%
    expect(diffColumn()).toEqual(['+20.00%', '+13.36%', '+2.00%', '—'])
  })

  it('shows the same gap in dollars across the position', async () => {
    await renderAnalysis()
    const index = columnIndex('Diff$')
    const cells = [...analysisTable().querySelectorAll('tbody tr')].map((r) => r.children[index].textContent!.trim())
    // (price − stop) × 100 shares
    expect(cells).toEqual(['+$200.00', '+$689.00', '+$20.00', '—'])
  })

  it('leaves Diff$ blank when the share count is unknown', async () => {
    // A fully-exited or share-less row must not render (price − stop) × 0 as $0
    const fixture = ROWS.map((r) => ({ ...r, shares: 0 }))
    getPortfolioRisk.mockResolvedValue({ rows: fixture, totals: { total_invested: 0, total_sl_dollar: 0 } })
    await renderAnalysis()
    const index = columnIndex('Diff$')
    const cells = [...analysisTable().querySelectorAll('tbody tr')].map((r) => r.children[index].textContent!.trim())
    expect(cells.every((c) => c === '—')).toBe(true)
    // Diff% is share-independent, so it still renders
    expect(diffColumn()[0]).toBe('+20.00%')
  })

  it('sorts by Diff$ independently of Diff%', async () => {
    await renderAnalysis()
    fireEvent.click(header('Diff\\$'))
    // Ascending by dollars: NONE.AX (null) then 20, 200, 689
    expect(symbolColumn()).toEqual(['NONE.AX', 'TIGHT.AX', 'MID.AX', 'TXG'])
    // Diff% ordering differs — TXG is 13.36% but the largest dollar amount
    fireEvent.click(header('Diff%'))
    expect(symbolColumn()).toEqual(['NONE.AX', 'TIGHT.AX', 'TXG', 'MID.AX'])
  })

  it('renders the 40-day EMA under Moving Avgs, red when price is below it', async () => {
    const fixture = [
      riskRow({ symbol: 'ABOVE.AX', current_price: 12.0, ema40w: 10.0 }),
      riskRow({ symbol: 'BELOW.AX', current_price: 8.0, ema40w: 10.0 }),
      riskRow({ symbol: 'NOEMA.AX', current_price: 8.0, ema40w: null }),
    ]
    getPortfolioRisk.mockResolvedValue({ rows: fixture, totals: { total_invested: 0, total_sl_dollar: 0 } })
    await renderAnalysis()

    const index = columnIndexIn('Moving Avgs', '40we')
    const cells = [...analysisTable().querySelectorAll('tbody tr')].map((r) => r.children[index] as HTMLElement)
    expect(cells.map((c) => c.textContent!.trim())).toEqual(['$10.00', '$10.00', '—'])
    // Same below-the-average highlight the SMA columns use
    expect(cells[0].style.color).toBe('')
    expect(cells[1].style.color).toBe('rgb(244, 67, 54)')
  })

  it('renders an em dash when the holding has no stop loss', async () => {
    await renderAnalysis()
    const index = columnIndex('Diff%')
    const noneRow = [...analysisTable().querySelectorAll('tbody tr')].find((r) =>
      r.children[0].textContent!.startsWith('NONE.AX'),
    )!
    expect(noneRow.children[index].textContent!.trim()).toBe('—')
  })

  it('sorts ascending then descending, with missing stops sorting lowest', async () => {
    await renderAnalysis()

    fireEvent.click(header('Diff%'))
    // Null is treated as -Infinity by the shared sort, so NONE.AX leads
    expect(symbolColumn()).toEqual(['NONE.AX', 'TIGHT.AX', 'TXG', 'MID.AX'])
    expect(header('Diff%').textContent).toContain('↑')

    fireEvent.click(header('Diff%'))
    expect(symbolColumn()).toEqual(['MID.AX', 'TXG', 'TIGHT.AX', 'NONE.AX'])
    expect(header('Diff%').textContent).toContain('↓')
  })

  it('does not disturb the other sortable columns', async () => {
    await renderAnalysis()
    fireEvent.click(header('Diff%'))
    expect(header('Diff%').textContent).toContain('↑')

    // Switching columns moves the indicator and re-sorts by the new column
    fireEvent.click(header('Symbol'))
    expect(header('Diff%').textContent).toContain('↕')
    expect(symbolColumn()).toEqual(['MID.AX', 'NONE.AX', 'TIGHT.AX', 'TXG'])
  })

  /**
   * The totals row spans the leading columns with a colSpan. Adding a column
   * without widening it would slide both totals one cell left, silently
   * mislabelling them.
   */
  it('keeps the totals under the columns they belong to', async () => {
    await renderAnalysis()
    const footer = footerByColumn()
    expect(footer).toHaveLength(headerLabels().length)

    // Checked against the header each cell actually sits under, so reordering
    // the columns without moving the totals fails here.
    expect(footer[columnIndexIn('Stop Loss', 'P/L%')]).toContain('%')
    expect(footer[columnIndexIn('Stop Loss', 'P/L$')]).toContain('$')
    expect(footer[columnIndexIn('Current', 'Price')]).toContain('Total if all sold at Stop Loss')
    // The trailing SMA columns carry no total
    expect(footer[columnIndex('30d High')]).toBe('')
  })

  it('lays the columns out in the configured order', async () => {
    await renderAnalysis()
    expect(headerLabels()).toEqual([
      'Symbol',
      'Price',
      'P/L%',
      'Price',
      'Diff%',
      'Diff$',
      'P/L%',
      'P/L$',
      '40we',
      '50s',
      '150s',
      '30d High',
    ])
  })

  it('groups the current, stop-loss and moving-average columns under shared headings', async () => {
    await renderAnalysis()
    expect(headerGroups()).toEqual([
      { label: 'Current', span: 2 },
      { label: 'Stop Loss', span: 5 },
      { label: 'Moving Avgs', span: 3 },
    ])

    // Each group must sit directly above exactly its own columns
    const labels = headerLabels()
    const at = (g: string) => {
      const r = groupRanges().find((x) => x.label === g)!
      return labels.slice(r.start, r.end + 1)
    }
    expect(at('Current')).toEqual(['Price', 'P/L%'])
    expect(at('Stop Loss')).toEqual(['Price', 'Diff%', 'Diff$', 'P/L%', 'P/L$'])
    expect(at('Moving Avgs')).toEqual(['40we', '50s', '150s'])
  })

  it('spans ungrouped headers across both header rows', async () => {
    await renderAnalysis()
    // Every top-row cell either spans both rows or heads a group
    for (const th of analysisTable().querySelectorAll('thead tr')[0].children) {
      const rowSpan = th.getAttribute('rowspan')
      const colSpan = parseInt(th.getAttribute('colspan') ?? '1', 10)
      expect(rowSpan === '2' || colSpan > 1).toBe(true)
    }
  })

  /**
   * Group boundaries are drawn with an inset box-shadow on the cell to the
   * right of each one (`rule-left`). Shadows do not collapse the way borders
   * do, so exactly one cell per boundary must carry it — painting both sides
   * would render two parallel lines. Every row has to mark the same columns,
   * or the rules break up partway down the table.
   */
  it('marks each group boundary on the cell to its right, in every row', async () => {
    await renderAnalysis()
    const table = analysisTable()
    const columnCount = headerLabels().length

    /**
     * Expand a row into one entry per column. A spanning cell only appears at
     * its first column — its left-edge shadow is drawn there, so the columns it
     * continues over must not be credited with the class.
     */
    const byColumn = (row: Element): Array<Element | null> => {
      const out: Array<Element | null> = []
      for (const cell of row.children) {
        const span = parseInt(cell.getAttribute('colspan') ?? '1', 10)
        out.push(cell)
        for (let i = 1; i < span; i++) out.push(null)
      }
      return out
    }

    const rows = [
      ...[...table.querySelectorAll('tbody tr')].map((r) => ({ what: 'body row', cells: byColumn(r) })),
      { what: 'totals row', cells: byColumn(table.querySelector('tfoot tr')!) },
    ]

    // Columns that begin a boundary: each group's first column, plus the one
    // immediately after each group ends.
    const expected = new Set<number>()
    for (const group of groupRanges()) {
      if (group.start > 0) expected.add(group.start)
      if (group.end < columnCount - 1) expected.add(group.end + 1)
    }
    expect(expected.size).toBeGreaterThan(0)

    for (const { what, cells } of rows) {
      expect(cells).toHaveLength(columnCount)
      for (let col = 0; col < columnCount; col++) {
        const marked = cells[col]?.className.includes('rule-left') ?? false
        expect(marked, `${what}: column ${col} (${headerLabels()[col]})`).toBe(expected.has(col))
      }
    }
  })

  it('supplies exactly as many second-row cells as the groups span', async () => {
    await renderAnalysis()
    const spanned = headerGroups().reduce((sum, g) => sum + g.span, 0)
    expect(analysisTable().querySelectorAll('thead tr')[1].children).toHaveLength(spanned)
  })
})

describe('Analysis table integrity', () => {
  it('gives every row the same cell count as the header', async () => {
    await renderAnalysis()
    // Counted from the flattened leaf headers, not `thead th` — the two-level
    // header has more th elements than the table has data columns.
    const columnCount = headerLabels().length
    for (const row of analysisTable().querySelectorAll('tbody tr')) {
      expect(within(row as HTMLElement).getAllByRole('cell')).toHaveLength(columnCount)
    }
  })
})
