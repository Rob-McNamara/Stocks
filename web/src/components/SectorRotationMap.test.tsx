// @vitest-environment jsdom
import { describe, it, expect, beforeEach, vi } from 'vitest'
import { render, screen, cleanup, fireEvent, within } from '@testing-library/react'
import SectorRotationMap from './SectorRotationMap'
import { formatSessionDate } from '../utils/sessionDate'
import type { RotationReturns, SectorHoldings, SectorRotationSet } from '../services/api'

// Opening a tile fetches its fund's holdings; the tests decide what comes back.
vi.mock('../services/api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../services/api')>()
  return { ...actual, apiClient: { getSectorHoldings: vi.fn() } }
})

import { apiClient } from '../services/api'
const getSectorHoldings = apiClient.getSectorHoldings as ReturnType<typeof vi.fn>

const returns = (d: number | null, w: number | null, m: number | null, q: number | null): RotationReturns => ({
  '1d': d, '1w': w, '1m': m, '3m': q,
})

/** A sector whose relative figures follow from its returns, as the API's do. */
function sector(symbol: string, name: string, own: RotationReturns, benchmark: RotationReturns, lastDate = '2026-10-23') {
  const minus = (a: number | null, b: number | null) => (a == null || b == null ? null : a - b)
  return {
    symbol,
    sector: name,
    long_name: null,
    last_date: lastDate,
    returns: own,
    relative: returns(
      minus(own['1d'], benchmark['1d']),
      minus(own['1w'], benchmark['1w']),
      minus(own['1m'], benchmark['1m']),
      minus(own['3m'], benchmark['3m']),
    ),
  }
}

function us(): SectorRotationSet {
  const spy = returns(-0.5, -0.4, -1.0, 3.0)
  return {
    name: 'US',
    as_of: '2026-10-23',
    benchmark: { symbol: 'SPY', name: 'S&P 500', long_name: 'SPDR S&P 500 ETF', last_date: '2026-10-23', returns: spy },
    sectors: [
      sector('XLK', 'Technology', returns(0.5, -0.1, -6.2, 2.7), spy),
      sector('XLC', 'Communication Services', returns(null, null, null, null), spy, '2026-10-22'),
      sector('XLE', 'Energy', returns(2.6, 1.6, 11.2, 16.8), spy),
      sector('XLU', 'Utilities', returns(-0.7, 0.8, 7.1, -7.9), spy),
    ],
  }
}

function asx(): SectorRotationSet {
  const xjo = returns(0.2, 1.0, 2.0, 4.0)
  return {
    name: 'ASX',
    as_of: '2026-10-23',
    benchmark: { symbol: '^AXJO', name: 'S&P/ASX 200', long_name: null, last_date: '2026-10-23', returns: xjo },
    sectors: [sector('^AXFJ', 'Financials', returns(1.2, 2.0, 3.0, 5.0), xjo)],
  }
}

/** Tiles in screen order, as their symbols. */
const tileSymbols = () =>
  Array.from(document.querySelectorAll('.rotation-tile')).map((t) => t.querySelector('.rotation-symbol')?.textContent)

const tile = (symbol: string) =>
  Array.from(document.querySelectorAll<HTMLElement>('.rotation-tile')).find(
    (t) => t.querySelector('.rotation-symbol')?.textContent === symbol,
  )!

const pressed = (name: string) => screen.getByRole('button', { name }).getAttribute('aria-pressed')

/** XLE opened up: two holdings and a cash line Yahoo gives no ticker for. */
function energyHoldings(): SectorHoldings {
  const none = returns(null, null, null, null)
  return {
    set: 'US',
    symbol: 'XLE',
    name: 'Energy',
    as_of: '2026-10-23',
    benchmark: { symbol: 'SPY', name: 'S&P 500' },
    top_weight: 39.4,
    holdings: [
      { symbol: 'XOM', name: 'Exxon Mobil Corp', weight: 22.9, last_date: '2026-10-23', returns: returns(1.2, 2.0, 9.0, 15.0), relative: returns(1.7, 2.4, 10.0, 12.0) },
      { symbol: 'CVX', name: 'Chevron Corp', weight: 16.1, last_date: '2026-10-23', returns: returns(-0.4, 1.0, 6.0, 10.0), relative: returns(0.1, 1.4, 7.0, 7.0) },
      { symbol: null, name: 'Cash', weight: 0.4, last_date: null, returns: none, relative: none },
    ],
  }
}

beforeEach(() => {
  cleanup()
  getSectorHoldings.mockReset().mockResolvedValue(energyHoldings())
})

describe('SectorRotationMap', () => {
  it('opens on the first set, one month against the benchmark', () => {
    render(<SectorRotationMap sets={[us(), asx()]} />)
    expect(pressed('US')).toBe('true')
    expect(pressed('1M')).toBe('true')
    expect(pressed('vs Benchmark')).toBe('true')
    expect(pressed('vs Return')).toBe('false')
    expect(screen.getByText('Fri 23 Oct 2026')).toBeTruthy()
  })

  it('puts the benchmark first in black, then the sectors strongest first', () => {
    render(<SectorRotationMap sets={[us()]} />)
    expect(tileSymbols()).toEqual(['SPY', 'XLE', 'XLU', 'XLK', 'XLC'])
    const benchmark = tile('SPY')
    expect(benchmark.style.background).toMatch(/#111111|rgb\(17, 17, 17\)/)
    expect(within(benchmark).getByText('-1.0%')).toBeTruthy()
    expect(within(benchmark).getByText('Benchmark')).toBeTruthy()
  })

  it('prints the move against the benchmark large and the plain return beneath it', () => {
    render(<SectorRotationMap sets={[us()]} />)
    const energy = tile('XLE')
    expect(within(energy).getByText('+12.2 pp')).toBeTruthy()
    expect(within(energy).getByText('+11.2% return')).toBeTruthy()
  })

  it('swaps the two figures on vs Return', () => {
    render(<SectorRotationMap sets={[us()]} />)
    fireEvent.click(screen.getByRole('button', { name: 'vs Return' }))
    const energy = tile('XLE')
    expect(within(energy).getByText('+11.2%')).toBeTruthy()
    expect(within(energy).getByText('+12.2 pp vs S&P 500')).toBeTruthy()
    expect(screen.getByText('−8%')).toBeTruthy()
  })

  it('re-ranks the tiles for another period, and widens the legend', () => {
    render(<SectorRotationMap sets={[us()]} />)
    expect(screen.getByText('−8 pp')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '1D' }))
    expect(tileSymbols()).toEqual(['SPY', 'XLE', 'XLK', 'XLU', 'XLC'])
    expect(screen.getByText('−2 pp')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '3M' }))
    expect(tileSymbols()).toEqual(['SPY', 'XLE', 'XLK', 'XLU', 'XLC'])
    expect(screen.getByText('+12 pp')).toBeTruthy()
  })

  it('says when a sector has no price for the as-of day', () => {
    render(<SectorRotationMap sets={[us()]} />)
    const stale = tile('XLC')
    expect(within(stale).getByText('—')).toBeTruthy()
    expect(within(stale).getByText('No price for Fri 23 Oct 2026')).toBeTruthy()
    expect(stale.getAttribute('title')).toContain('the newest is Thu 22 Oct 2026')
  })

  it('switches between sets', () => {
    render(<SectorRotationMap sets={[us(), asx()]} />)
    fireEvent.click(screen.getByRole('button', { name: 'ASX' }))
    expect(tileSymbols()).toEqual(['^AXJO', '^AXFJ'])
    expect(within(tile('^AXJO')).getByText('S&P/ASX 200')).toBeTruthy()
    expect(within(tile('^AXFJ')).getByText('+1.0 pp')).toBeTruthy()
  })

  it('offers no set switcher when there is only one set', () => {
    render(<SectorRotationMap sets={[us()]} />)
    expect(screen.queryByRole('group', { name: 'Sector set' })).toBeNull()
  })

  it('says so when there are no sets', () => {
    render(<SectorRotationMap sets={[]} />)
    expect(screen.getByText('No sector sets configured.')).toBeTruthy()
  })
})

describe('a tile opened up', () => {
  /** The holdings panel's rows, cell by cell. */
  const rows = () =>
    Array.from(document.querySelectorAll('.rotation-holdings tbody tr')).map((tr) =>
      Array.from(tr.querySelectorAll('td')).map((td) => td.textContent),
    )

  it('lists the fund’s top holdings with their weights and moves, and closes again', async () => {
    render(<SectorRotationMap sets={[us()]} />)
    fireEvent.click(tile('XLE'))
    expect(getSectorHoldings).toHaveBeenCalledWith('US', 'XLE')
    expect(tile('XLE').getAttribute('aria-expanded')).toBe('true')

    const panel = await screen.findByRole('region', { name: 'Top holdings of XLE' })
    expect(within(panel).getByText('Top 3 holdings · 39.4% of the fund')).toBeTruthy()
    expect(within(panel).getByText('1M return')).toBeTruthy()
    expect(within(panel).getByText('vs S&P 500')).toBeTruthy()
    expect(rows()).toEqual([
      ['XOM', 'Exxon Mobil Corp', '22.9%', '+9.0%', '+10.0 pp'],
      ['CVX', 'Chevron Corp', '16.1%', '+6.0%', '+7.0 pp'],
      ['—', 'Cash', '0.4%', '—', '—'],
    ])

    fireEvent.click(tile('XLE'))
    expect(screen.queryByRole('region', { name: 'Top holdings of XLE' })).toBeNull()
    expect(tile('XLE').getAttribute('aria-expanded')).toBe('false')
  })

  it('follows the period, and shades the figure the tiles are measuring', async () => {
    render(<SectorRotationMap sets={[us()]} />)
    fireEvent.click(tile('XLE'))
    await screen.findByRole('region', { name: 'Top holdings of XLE' })
    const shaded = () => Array.from(document.querySelectorAll('.rotation-holdings-figure')).map((td) => td.textContent)

    fireEvent.click(screen.getByRole('button', { name: '1D' }))
    expect(screen.getByText('1D return')).toBeTruthy()
    expect(rows()[0]).toEqual(['XOM', 'Exxon Mobil Corp', '22.9%', '+1.2%', '+1.7 pp'])
    expect(shaded()[0]).toBe('+1.7 pp')

    fireEvent.click(screen.getByRole('button', { name: 'vs Return' }))
    expect(shaded()[0]).toBe('+1.2%')
  })

  it('says why a holding is blank when it has no price for the day', async () => {
    const opened = energyHoldings()
    opened.holdings[1] = { ...opened.holdings[1], last_date: '2026-10-22', returns: returns(null, null, null, null), relative: returns(null, null, null, null) }
    getSectorHoldings.mockResolvedValueOnce(opened)
    render(<SectorRotationMap sets={[us()]} />)
    fireEvent.click(tile('XLE'))
    await screen.findByRole('region', { name: 'Top holdings of XLE' })
    const row = Array.from(document.querySelectorAll('.rotation-holdings tbody tr'))[1]
    expect(row.getAttribute('title')).toBe('No price for Fri 23 Oct 2026 — the newest is Thu 22 Oct 2026')
    expect(within(row as HTMLElement).getByText('Last price Thu 22 Oct 2026')).toBeTruthy()
    expect(Array.from(document.querySelectorAll('.rotation-holdings tbody tr'))[0].getAttribute('title')).toBeNull()
  })

  it('says why when there are no holdings to show', async () => {
    getSectorHoldings.mockRejectedValueOnce(new Error('^AXIJ is an index rather than a fund, so Yahoo lists no holdings for it'))
    render(<SectorRotationMap sets={[us()]} />)
    fireEvent.click(tile('XLK'))
    expect(await screen.findByText('^AXIJ is an index rather than a fund, so Yahoo lists no holdings for it')).toBeTruthy()
  })

  it('closes from its own button, and when another set is chosen', async () => {
    render(<SectorRotationMap sets={[us(), asx()]} />)
    fireEvent.click(tile('XLE'))
    const panel = await screen.findByRole('region', { name: 'Top holdings of XLE' })
    fireEvent.click(within(panel).getByRole('button', { name: 'Close' }))
    expect(screen.queryByRole('region')).toBeNull()

    fireEvent.click(tile('XLE'))
    await screen.findByRole('region', { name: 'Top holdings of XLE' })
    fireEvent.click(screen.getByRole('button', { name: 'ASX' }))
    expect(screen.queryByRole('region')).toBeNull()
  })
})

describe('formatSessionDate', () => {
  it('names the weekday, since a US close lands on an Australian morning', () => {
    expect(formatSessionDate('2026-10-23')).toBe('Fri 23 Oct 2026')
    expect(formatSessionDate('2026-01-05')).toBe('Mon 5 Jan 2026')
  })
})
