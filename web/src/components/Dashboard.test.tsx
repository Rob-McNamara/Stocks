// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, waitFor, within, cleanup, fireEvent } from '@testing-library/react'
import Dashboard from './Dashboard'
import type { CustomListEntry, PortfolioOverview } from '../services/api'

// The Dashboard is a pure renderer over GET /api/portfolio/overview — mock
// the client and feed it a canned payload.
vi.mock('../services/api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../services/api')>()
  return {
    ...actual,
    apiClient: { getPortfolioOverview: vi.fn(), getPortfolioHistory: vi.fn() },
  }
})

import { apiClient } from '../services/api'
const getPortfolioOverview = apiClient.getPortfolioOverview as ReturnType<typeof vi.fn>
const getPortfolioHistory = apiClient.getPortfolioHistory as ReturnType<typeof vi.fn>

/** Minimal history payload; the value chart has its own test file. */
function historyFixture() {
  return {
    series: [
      { date: '2026-01-01', stocks: 1000, cash: 500, total: 1500, flow: 1500 },
      { date: '2026-01-02', stocks: 1100, cash: 500, total: 1600, flow: 0 },
    ],
    summary: {
      start_date: '2026-01-01',
      end_date: '2026-01-02',
      opening_value: 0,
      end_value: 1600,
      net_contributions: 1500,
      gain: 100,
      twr_pct: 6.6667,
    },
  }
}

const emptyAgg = { count: 0, value: 0, dividends: 0, pl: 0, cost: 0 }

/**
 * A custom-list row. `compare_value` defaults to the price, which is what the
 * server sends for every list that is not comparing volume — keeping the
 * fixtures to the fields each test actually cares about.
 */
function entry(
  symbol: string,
  price: number,
  field_value: number,
  extra: Partial<CustomListEntry> = {},
): CustomListEntry {
  const diff = price - field_value
  return {
    symbol,
    price,
    compare_value: price,
    field_value,
    diff,
    pct_diff: (diff / field_value) * 100,
    currency: null,
    is_trailing: false,
    ...extra,
  }
}

function overviewFixture(): PortfolioOverview {
  return {
    totals: { stock_count: 2, total_value: 2690, total_pl: 680, holdings_pl: 630, sold_pl: 50 },
    breakdowns: {
      equities: { count: 2, value: 2690, dividends: 0, pl: 630, cost: 2060 },
      etfs: { ...emptyAgg },
      holdings: { count: 2, value: 2690, dividends: 0, pl: 630, cost: 2060 },
      sold: { ...emptyAgg },
    },
    sectors: [],
    worst_holdings: [
      { symbol: 'PART.AX', price: 1.5, sma150: 3.0, pct_diff: -50 },
      { symbol: 'MAN.AX', price: 12.0, sma150: 10.0, pct_diff: 20 },
    ],
    best_watchlist: [
      { symbol: 'WATCH.AX', price: 5.0, sma50: 4.5, sma50_trend: 'up', days_since_50sma: 3, volume_pct_50sma: 12 },
    ],
    custom_lists: [
      {
        key: 'stop_losses',
        label: 'Stop Losses',
        source: 'holdings',
        field_source: 'holdings',
        operator: 'pct_below',
        field_label: 'Stop Loss Price',
        // The server always reports the direction it ranked by, and whether the
        // limit cut anything — the header renders its arrow from these.
        compare: 'price',
        sort: 'asc',
        truncated: false,
        entries: [
          entry('MAN.AX', 12.0, 9.0),
          entry('TRL.AX', 28.0, 27.0, { is_trailing: true }),
        ],
      },
    ],
  }
}

async function renderDashboard(props: Partial<Parameters<typeof Dashboard>[0]> = {}) {
  const result = render(<Dashboard onLoading={() => {}} {...props} />)
  await waitFor(() => expect(screen.queryByText(/Loading dashboard/)).toBeNull())
  return result
}

beforeEach(() => {
  cleanup()
  getPortfolioOverview.mockReset()
  getPortfolioOverview.mockResolvedValue(overviewFixture())
  getPortfolioHistory.mockReset().mockResolvedValue(historyFixture())
})

describe('Dashboard custom lists', () => {
  it('renders stop-loss entries with price, stop value and margin', async () => {
    await renderDashboard()
    const table = screen.getByText('Stop Losses').closest('.manager-card')!
    const rows = within(table as HTMLElement).getAllByRole('row').slice(1) // skip header
    expect(rows).toHaveLength(2)
    expect(rows[0].textContent).toContain('MAN.AX')
    expect(rows[0].textContent).toContain('$12.00')
    expect(rows[0].textContent).toContain('$9.00')
    expect(rows[0].textContent).toContain('+33.33%')
  })

  // A volume list ranks the same way but on a share count, so the column has to
  // change its heading and stop formatting the number as money.
  it('labels and formats the compared column as volume when the list says so', async () => {
    const fixture = overviewFixture()
    const list = fixture.custom_lists[0]
    list.label = 'Heavy Volume'
    list.compare = 'volume'
    list.operator = 'above'
    list.field_label = 'Min Volume'
    list.entries = [entry('MAN.AX', 12.0, 100000, { compare_value: 500000, diff: 400000, pct_diff: 400 })]
    getPortfolioOverview.mockResolvedValue(fixture)
    await renderDashboard()

    const table = screen.getByText('Heavy Volume').closest('.manager-card')! as HTMLElement
    expect(within(table).getByText('Volume')).toBeTruthy()
    expect(within(table).queryByText('Price')).toBeNull()

    const row = within(table).getAllByRole('row')[1]
    expect(row.textContent).toContain('500,000')
    // The price is still in the payload but must not reach the column
    expect(row.textContent).not.toContain('$12.00')
    expect(table.textContent).toContain('volume is above Min Volume')
  })

  // Deploying the client ahead of the API is a normal state, and the field is
  // new — a row without it must render the price, not blank the dashboard.
  it('falls back to the price when the API sends no compare_value', async () => {
    const fixture = overviewFixture()
    fixture.custom_lists[0].entries = [entry('MAN.AX', 12.0, 9.0, { compare_value: undefined })]
    getPortfolioOverview.mockResolvedValue(fixture)
    await renderDashboard()

    const table = screen.getByText('Stop Losses').closest('.manager-card')! as HTMLElement
    expect(within(table).getAllByRole('row')[1].textContent).toContain('$12.00')
  })

  it('marks trailing-sell triggers with the T badge and leaves manual stops unmarked', async () => {
    await renderDashboard()
    const table = screen.getByText('Stop Losses').closest('.manager-card')!
    const badge = within(table as HTMLElement).getByTitle('Trailing sell trigger')
    expect(badge.textContent).toBe('T')
    // The badge sits in TRL.AX's row, not MAN.AX's
    expect(badge.closest('tr')!.textContent).toContain('TRL.AX')
    const manualRow = within(table as HTMLElement).getByText('MAN.AX').closest('tr')!
    expect(within(manualRow).queryByTitle('Trailing sell trigger')).toBeNull()
  })
})

describe('Stop Losses Difference sorting', () => {
  /** Symbols in render order for the Stop Losses table. */
  function symbolOrder(): string[] {
    const table = screen.getByText('Stop Losses').closest('.manager-card')!
    return within(table as HTMLElement)
      .getAllByRole('row')
      .slice(1)
      .map((row) => row.textContent!.match(/[A-Z]+\.[A-Z]+/)![0])
  }

  function diffHeader(): HTMLElement {
    const table = screen.getByText('Stop Losses').closest('.manager-card')!
    return within(table as HTMLElement).getByText(/^Difference/)
  }

  it('shows the direction the server reported, without a neutral state', async () => {
    await renderDashboard()
    expect(diffHeader().textContent).toContain('↑')
    expect(symbolOrder()).toEqual(['MAN.AX', 'TRL.AX'])
  })

  /**
   * The core of this feature: the server ranks then truncates to the list's
   * limit, so a click must re-query rather than reverse the rows already on
   * screen — otherwise a row outside the cut can never appear.
   */
  it('re-queries with the reversed direction instead of reordering locally', async () => {
    await renderDashboard()
    expect(getPortfolioOverview).toHaveBeenCalledTimes(1)
    expect(getPortfolioOverview).toHaveBeenLastCalledWith({})

    // The server returns a different row set for desc — one the asc cut excluded
    const desc = overviewFixture()
    desc.custom_lists[0].sort = 'desc'
    desc.custom_lists[0].entries = [entry('CUT.AX', 20.0, 10.0), entry('MAN.AX', 12.0, 9.0)]
    getPortfolioOverview.mockResolvedValue(desc)

    fireEvent.click(diffHeader())

    await waitFor(() => expect(getPortfolioOverview).toHaveBeenCalledTimes(2))
    expect(getPortfolioOverview).toHaveBeenLastCalledWith({ stop_losses: 'desc' })
    // A symbol absent from the first response is now on screen — impossible
    // with client-side reordering
    await waitFor(() => expect(symbolOrder()).toEqual(['CUT.AX', 'MAN.AX']))
    expect(diffHeader().textContent).toContain('↓')
  })

  it('flips against the server direction, so a desc-configured list goes to asc', async () => {
    const fixture = overviewFixture()
    fixture.custom_lists[0].sort = 'desc'
    getPortfolioOverview.mockResolvedValue(fixture)
    await renderDashboard()
    expect(diffHeader().textContent).toContain('↓')

    fireEvent.click(diffHeader())
    await waitFor(() => expect(getPortfolioOverview).toHaveBeenLastCalledWith({ stop_losses: 'asc' }))
  })

  it('keeps the dashboard on screen while re-sorting', async () => {
    await renderDashboard()
    let resolve: (v: unknown) => void = () => {}
    getPortfolioOverview.mockReturnValue(new Promise((r) => { resolve = r }))

    fireEvent.click(diffHeader())
    // Mid-flight the table is still rendered — no full-page loading placeholder
    expect(screen.queryByText(/Loading dashboard/)).toBeNull()
    expect(symbolOrder()).toEqual(['MAN.AX', 'TRL.AX'])

    resolve(overviewFixture())
    await waitFor(() => expect(getPortfolioOverview).toHaveBeenCalledTimes(2))
  })

  it('sends only the clicked list, leaving other lists at their configured order', async () => {
    const fixture = overviewFixture()
    fixture.custom_lists.push({
      ...fixture.custom_lists[0],
      key: 'breakthroughs',
      label: 'Breakthrough Price',
      entries: [entry('AAA.AX', 5.0, 4.0)],
    })
    getPortfolioOverview.mockResolvedValue(fixture)
    await renderDashboard()

    fireEvent.click(diffHeader())
    await waitFor(() => expect(getPortfolioOverview).toHaveBeenLastCalledWith({ stop_losses: 'desc' }))
  })
})

// A crossover list is about when the price crossed and how much volume was
// behind it, so it trades the Difference column for that pair.
describe('crossover list columns', () => {
  function crossFixture(operator: string, metric: string) {
    const fixture = overviewFixture()
    const list = fixture.custom_lists[0]
    list.label = 'Broke Out'
    list.operator = operator
    list.metric = metric
    list.field_label = '50-Day SMA'
    list.entries = [entry('MAN.AX', 12.0, 10.0, { days: 5, volume_cross_pct: 200 })]
    return fixture
  }

  const card = () => screen.getByText('Broke Out').closest('.manager-card')! as HTMLElement

  it('replaces Difference with the day count and the volume behind it', async () => {
    getPortfolioOverview.mockResolvedValue(crossFixture('days_above', 'days'))
    await renderDashboard()

    expect(within(card()).getByText(/^Days Above/)).toBeTruthy()
    expect(within(card()).getByText('Vol on Cross')).toBeTruthy()
    expect(within(card()).queryByText(/^Difference/)).toBeNull()

    const row = within(card()).getAllByRole('row')[1]
    expect(row.textContent).toContain('5d')
    expect(row.textContent).toContain('+200%')
    expect(row.textContent).not.toContain('20.00%')
  })

  it('names the column for the direction being counted', async () => {
    getPortfolioOverview.mockResolvedValue(crossFixture('days_below', 'days'))
    await renderDashboard()
    expect(within(card()).getByText(/^Days Below/)).toBeTruthy()
  })

  // Reversing re-ranks on the server by the list's metric alone, so only that
  // column may carry the sort affordance.
  it('makes only the ranked column sortable', async () => {
    getPortfolioOverview.mockResolvedValue(crossFixture('volume_cross_pct', 'volume_cross_pct'))
    await renderDashboard()

    const volHeader = within(card()).getByText(/^Vol on Cross/)
    expect(volHeader.className).toContain('sortable-header')
    expect(volHeader.textContent).toContain('↑')
    expect(within(card()).getByText('Days Above').className).not.toContain('sortable-header')

    fireEvent.click(volHeader)
    await waitFor(() => expect(getPortfolioOverview).toHaveBeenLastCalledWith({ stop_losses: 'desc' }))
  })

  it('renders a dash when a row has no crossing figures', async () => {
    const fixture = crossFixture('days_above', 'days')
    fixture.custom_lists[0].entries = [entry('MAN.AX', 12.0, 10.0, { days: null, volume_cross_pct: null })]
    getPortfolioOverview.mockResolvedValue(fixture)
    await renderDashboard()

    const row = within(card()).getAllByRole('row')[1]
    expect(row.textContent).toContain('—')
  })

  // An API predating the metric sends none, and its lists are all percentage
  // gaps — the Difference column must survive that.
  it('keeps the Difference column when the API sends no metric', async () => {
    const fixture = overviewFixture()
    delete fixture.custom_lists[0].metric
    getPortfolioOverview.mockResolvedValue(fixture)
    await renderDashboard()

    const table = screen.getByText('Stop Losses').closest('.manager-card')! as HTMLElement
    expect(within(table).getByText(/^Difference/)).toBeTruthy()
    expect(within(table).getAllByRole('row')[1].textContent).toContain('33.33%')
  })
})

/**
 * A holding cannot be valued before its first stored price, so a configured
 * floor cuts off the years the chart would otherwise draw with the stock line
 * flat at zero. Ranges reaching past it return the same window as "All".
 */
describe('portfolio history range buttons', () => {
  const rangeButtons = () => {
    const card = screen.getByText('Portfolio Value').closest('.manager-card')! as HTMLElement
    return within(card).getAllByRole('button').map((b) => b.textContent)
  }

  it('offers every range when no floor is configured', async () => {
    await renderDashboard()
    expect(rangeButtons()).toEqual(['3M', '6M', '12M', '2Y', '5Y', 'All'])
  })

  it('keeps them all when the floor predates every range', async () => {
    getPortfolioHistory.mockResolvedValue({ ...historyFixture(), start_floor: '2000-01-01' })
    await renderDashboard()
    expect(rangeButtons()).toEqual(['3M', '6M', '12M', '2Y', '5Y', 'All'])
  })

  // Today's date as the floor is before every bounded range's start, whenever
  // the suite happens to run — so the expectation does not drift with the clock.
  it('drops the ranges the floor swallows', async () => {
    const today = new Date().toISOString().slice(0, 10)
    getPortfolioHistory.mockResolvedValue({ ...historyFixture(), start_floor: today })
    await renderDashboard()
    expect(rangeButtons()).toEqual(['All'])
  })

  // Otherwise no button is lit and the chart shows a window nothing claims.
  it('falls back to All when the selected range is dropped', async () => {
    await renderDashboard()
    const card = () => screen.getByText('Portfolio Value').closest('.manager-card')! as HTMLElement
    fireEvent.click(within(card()).getByRole('button', { name: '5Y' }))
    await waitFor(() =>
      expect(getPortfolioHistory).toHaveBeenLastCalledWith(expect.stringMatching(/^\d{4}-\d{2}-\d{2}$/)),
    )

    const today = new Date().toISOString().slice(0, 10)
    getPortfolioHistory.mockResolvedValue({ ...historyFixture(), start_floor: today })
    // Any refetch now carries the floor back with it.
    fireEvent.click(within(card()).getByRole('button', { name: '3M' }))

    await waitFor(() => expect(rangeButtons()).toEqual(['All']))
    // "All" asks for no start date at all, leaving the window to the server.
    await waitFor(() => expect(getPortfolioHistory).toHaveBeenLastCalledWith(undefined))
  })

  // An API older than the setting omits the field rather than sending null.
  it('offers every range when the API sends no floor at all', async () => {
    const older = historyFixture() as Record<string, unknown>
    delete older.start_floor
    getPortfolioHistory.mockResolvedValue(older)
    await renderDashboard()
    expect(rangeButtons()).toEqual(['3M', '6M', '12M', '2Y', '5Y', 'All'])
  })
})

describe('Dashboard navigation', () => {
  it('worst-holdings symbols navigate to the Holdings screen', async () => {
    const onNavigateToHoldings = vi.fn()
    await renderDashboard({ onNavigateToHoldings })
    fireEvent.click(screen.getByRole('button', { name: 'PART.AX' }))
    expect(onNavigateToHoldings).toHaveBeenCalledWith('PART.AX')
  })

  it('watchlist symbols navigate to the Watchlist screen', async () => {
    const onNavigateToWatchlist = vi.fn()
    const onNavigateToHoldings = vi.fn()
    await renderDashboard({ onNavigateToWatchlist, onNavigateToHoldings })
    fireEvent.click(screen.getByRole('button', { name: 'WATCH.AX' }))
    expect(onNavigateToWatchlist).toHaveBeenCalledWith('WATCH.AX')
    expect(onNavigateToHoldings).not.toHaveBeenCalled()
  })

  it('holdings-sourced custom list entries navigate to Holdings', async () => {
    const onNavigateToHoldings = vi.fn()
    await renderDashboard({ onNavigateToHoldings })
    fireEvent.click(screen.getByRole('button', { name: 'TRL.AX' }))
    expect(onNavigateToHoldings).toHaveBeenCalledWith('TRL.AX')
  })

  // An indicator list sourced from "both" mixes the tables, so one list-level
  // field_source cannot route every row — each row carries its own origin.
  it('routes each row of a mixed list by its own origin', async () => {
    const onNavigateToHoldings = vi.fn()
    const onNavigateToWatchlist = vi.fn()
    const fixture = overviewFixture()
    const list = fixture.custom_lists[0]
    list.label = 'Above 50SMA'
    list.source = 'both'
    list.field_source = 'indicator'
    list.field_label = '50-Day SMA'
    list.entries = [
      entry('HELD.AX', 12.0, 10.0, { origin: 'holdings' }),
      entry('WATCH.AX', 8.0, 5.0, { origin: 'watchlist' }),
    ]
    getPortfolioOverview.mockResolvedValue(fixture)
    await renderDashboard({ onNavigateToHoldings, onNavigateToWatchlist })

    const table = screen.getByText('Above 50SMA').closest('.manager-card')! as HTMLElement
    fireEvent.click(within(table).getByRole('button', { name: 'HELD.AX' }))
    expect(onNavigateToHoldings).toHaveBeenCalledWith('HELD.AX')
    fireEvent.click(within(table).getByRole('button', { name: 'WATCH.AX' }))
    expect(onNavigateToWatchlist).toHaveBeenCalledWith('WATCH.AX')
  })

  // Older payloads have no per-row origin; the list-level source still routes.
  it('falls back to the list field_source when a row has no origin', async () => {
    const onNavigateToHoldings = vi.fn()
    const fixture = overviewFixture()
    fixture.custom_lists[0].entries = [entry('MAN.AX', 12.0, 9.0, { origin: undefined })]
    getPortfolioOverview.mockResolvedValue(fixture)
    await renderDashboard({ onNavigateToHoldings })

    const table = screen.getByText('Stop Losses').closest('.manager-card')! as HTMLElement
    fireEvent.click(within(table).getByRole('button', { name: 'MAN.AX' }))
    expect(onNavigateToHoldings).toHaveBeenCalledWith('MAN.AX')
  })

  it('renders plain text instead of buttons when no navigation handler is wired', async () => {
    await renderDashboard()
    expect(screen.queryByRole('button', { name: 'PART.AX' })).toBeNull()
    expect(screen.getAllByText('PART.AX').length).toBeGreaterThan(0)
  })
})

describe('Dashboard states', () => {
  it('renders empty states for each table', async () => {
    getPortfolioOverview.mockResolvedValue({
      ...overviewFixture(),
      worst_holdings: [],
      best_watchlist: [],
      custom_lists: [
        { ...overviewFixture().custom_lists[0], entries: [] },
      ],
    })
    await renderDashboard()
    expect(screen.getByText('No SMA data available for holdings.')).toBeTruthy()
    expect(screen.getByText('No stocks currently above their 50SMA.')).toBeTruthy()
    expect(screen.getByText('No matching stocks found.')).toBeTruthy()
  })

  it('surfaces a load failure as the error banner', async () => {
    getPortfolioOverview.mockRejectedValue(new Error('overview unavailable'))
    render(<Dashboard onLoading={() => {}} />)
    await waitFor(() => expect(screen.getByText(/overview unavailable/)).toBeTruthy())
  })
})
