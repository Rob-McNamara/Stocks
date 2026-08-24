// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, waitFor, within, cleanup, fireEvent } from '@testing-library/react'
import Dashboard from './Dashboard'
import type { PortfolioOverview } from '../services/api'

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
        sort: 'asc',
        truncated: false,
        entries: [
          { symbol: 'MAN.AX', price: 12.0, field_value: 9.0, diff: 3.0, pct_diff: 33.33, currency: null, is_trailing: false },
          { symbol: 'TRL.AX', price: 28.0, field_value: 27.0, diff: 1.0, pct_diff: 3.7, currency: null, is_trailing: true },
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
    desc.custom_lists[0].entries = [
      { symbol: 'CUT.AX', price: 20.0, field_value: 10.0, diff: 10.0, pct_diff: 100, currency: null, is_trailing: false },
      { symbol: 'MAN.AX', price: 12.0, field_value: 9.0, diff: 3.0, pct_diff: 33.33, currency: null, is_trailing: false },
    ]
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
      entries: [
        { symbol: 'AAA.AX', price: 5.0, field_value: 4.0, diff: 1.0, pct_diff: 25, currency: null, is_trailing: false },
      ],
    })
    getPortfolioOverview.mockResolvedValue(fixture)
    await renderDashboard()

    fireEvent.click(diffHeader())
    await waitFor(() => expect(getPortfolioOverview).toHaveBeenLastCalledWith({ stop_losses: 'desc' }))
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
