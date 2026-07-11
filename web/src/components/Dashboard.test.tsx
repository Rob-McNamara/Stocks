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
    apiClient: { getPortfolioOverview: vi.fn() },
  }
})

import { apiClient } from '../services/api'
const getPortfolioOverview = apiClient.getPortfolioOverview as ReturnType<typeof vi.fn>

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
