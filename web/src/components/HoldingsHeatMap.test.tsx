// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, cleanup, fireEvent, within } from '@testing-library/react'
import HoldingsHeatMap from './HoldingsHeatMap'
import type { PortfolioHolding } from '../services/api'

function holding(overrides: Partial<PortfolioHolding> & { symbol: string }): PortfolioHolding {
  return {
    long_name: null,
    instrument_type: 'EQUITY',
    is_etf: false,
    is_international: false,
    currency: 'AUD',
    sector: null,
    notes: null,
    fields: {},
    shares: 100,
    invested: 1000,
    avg_cost: 10,
    native_avg_cost: 10,
    current_price: 12,
    native_current_price: 12,
    price_source: 'cache',
    price_date: '2026-08-28',
    change: 0.1,
    change_percent: 1,
    volume: 1000,
    current_value: 1200,
    dividends: 0,
    pl: 200,
    pl_pct: 20,
    sma50: null, native_sma50: null,
    sma150: null, native_sma150: null,
    ema40w: null, native_ema40w: null, day_pl: null,
    stop_loss: null,
    is_trailing_sell: false,
    basis_date: null,
    basis_price: null,
    ...overrides,
  }
}

const tiles = () => Array.from(document.querySelectorAll('svg g rect'))
const areaOf = (rect: Element) =>
  parseFloat(rect.getAttribute('width')!) * parseFloat(rect.getAttribute('height')!)

const tileFor = (symbol: string): SVGGElement => {
  const title = Array.from(document.querySelectorAll('svg title')).find((t) =>
    t.textContent!.startsWith(symbol),
  )
  return title!.closest('g') as unknown as SVGGElement
}

beforeEach(cleanup)

describe('HoldingsHeatMap', () => {
  // Area is the only thing carrying position size, so it has to track value.
  it('sizes each tile by its share of the portfolio value', () => {
    render(
      <HoldingsHeatMap
        holdings={[
          holding({ symbol: 'BIG.AX', current_value: 6000 }),
          holding({ symbol: 'MID.AX', current_value: 3000 }),
          holding({ symbol: 'SMALL.AX', current_value: 1000 }),
        ]}
      />,
    )
    const [big, mid, small] = [tileFor('BIG.AX'), tileFor('MID.AX'), tileFor('SMALL.AX')].map(
      (g) => areaOf(g.querySelector('rect')!),
    )
    expect(big / mid).toBeCloseTo(2, 1)
    expect(mid / small).toBeCloseTo(3, 1)
  })

  // A holding with no usable price has a value of zero; drawing it at 0×0 would
  // leave an invisible tile that still answered to a click.
  it('omits holdings with no value', () => {
    render(
      <HoldingsHeatMap
        holdings={[
          holding({ symbol: 'REAL.AX', current_value: 5000 }),
          holding({ symbol: 'UNPRICED.AX', current_value: 0, current_price: null, pl_pct: null }),
        ]}
      />,
    )
    expect(tiles()).toHaveLength(1)
    expect(document.body.textContent).not.toContain('UNPRICED.AX')
  })

  it('shows the empty state when nothing can be mapped', () => {
    render(<HoldingsHeatMap holdings={[holding({ symbol: 'X.AX', current_value: 0 })]} />)
    expect(screen.getByText(/No priced holdings to map/)).toBeTruthy()
  })

  /**
   * Colour alone cannot carry profit and loss — a green-to-red ramp is exactly
   * what red-green colour vision deficiency flattens. The figure printed in the
   * tile is the redundant channel that keeps the map readable.
   */
  it('prints the percentage in tiles with room for it', () => {
    render(<HoldingsHeatMap holdings={[holding({ symbol: 'ONE.AX', pl_pct: 24.5 })]} />)
    expect(within(tileFor('ONE.AX') as unknown as HTMLElement).getByText('+24.5%')).toBeTruthy()
  })

  it('drops the label rather than overflowing a small tile, keeping the tooltip', () => {
    const holdings = [
      holding({ symbol: 'HUGE.AX', current_value: 500000 }),
      holding({ symbol: 'TINY.AX', current_value: 40 }),
    ]
    render(<HoldingsHeatMap holdings={holdings} />)

    expect(tileFor('HUGE.AX').querySelector('text')).toBeTruthy()
    expect(tileFor('TINY.AX').querySelector('text')).toBeNull()
    // The tooltip is what makes an unlabelled tile still worth having.
    expect(tileFor('TINY.AX').querySelector('title')!.textContent).toContain('TINY.AX')
  })

  // A long-held position can be up thousands of percent. The decimal place is
  // noise at that scale, and the extra characters are what push the label out.
  it('drops the decimal on percentages in the hundreds and above', () => {
    render(<HoldingsHeatMap holdings={[holding({ symbol: 'OLD.AX', pl_pct: 57150 })]} />)
    expect(within(tileFor('OLD.AX') as unknown as HTMLElement).getByText('+57150%')).toBeTruthy()
  })

  /**
   * The case a size threshold alone gets wrong: two tiles of identical size,
   * one labelled and one not, because the labels are different lengths.
   * Spilling the long one would paint over its neighbour.
   */
  it('hides a label too long for its tile while keeping a short one', () => {
    // Enough equal-valued holdings to make the tiles roomy by any pixel
    // threshold but too narrow for a long symbol.
    const holdings = Array.from({ length: 30 }, (_, i) =>
      holding({ symbol: i === 0 ? 'VERYLONGTICKER.AX' : `S${i}.AX`, current_value: 1000 }),
    )
    render(<HoldingsHeatMap holdings={holdings} />)

    const long = tileFor('VERYLONGTICKER.AX')
    const short = tileFor('S1.AX')
    const size = (g: SVGGElement) => {
      const rect = g.querySelector('rect')!
      return {
        width: parseFloat(rect.getAttribute('width')!),
        height: parseFloat(rect.getAttribute('height')!),
      }
    }
    // Guard the fixture: both tiles must be ones a pixel threshold would have
    // labelled, or the test proves nothing.
    for (const g of [long, short]) {
      expect(size(g).width).toBeGreaterThanOrEqual(52)
      expect(size(g).height).toBeGreaterThanOrEqual(40)
    }

    expect(short.querySelector('text')).toBeTruthy()
    expect(long.querySelector('text')).toBeNull()
    // The tooltip is what makes the unlabelled tile still worth having.
    expect(long.querySelector('title')!.textContent).toContain('VERYLONGTICKER.AX')
  })

  it('reports value, weight and both returns in the tooltip', () => {
    render(
      <HoldingsHeatMap
        holdings={[
          holding({ symbol: 'A.AX', long_name: 'Alpha Ltd', current_value: 7500, pl: 1500, pl_pct: 25, change_percent: -1.2 }),
          holding({ symbol: 'B.AX', current_value: 2500 }),
        ]}
      />,
    )
    const tooltip = tileFor('A.AX').querySelector('title')!.textContent!
    expect(tooltip).toContain('Alpha Ltd')
    expect(tooltip).toContain('$7,500')
    expect(tooltip).toContain('75.0% of holdings')
    expect(tooltip).toContain('+25.0%')
    expect(tooltip).toContain('-1.2%')
  })

  /**
   * SGOV's case: the US price rose, but the dollar fell further since the
   * purchases, so the AUD P/L is a loss. Without the parts the figure reads as
   * wrong; with them it says which way each one went.
   */
  it('splits a foreign holding\'s P/L into price and currency in the tooltip', () => {
    render(
      <HoldingsHeatMap
        holdings={[
          holding({ symbol: 'SGOV', is_international: true, currency: 'USD', invested: 7207, pl: -55.84, pl_pct: -0.77, pl_price: 7.12, pl_currency: -62.96 }),
          holding({ symbol: 'RSP', is_international: true, currency: 'USD', invested: 1000, pl: 85, pl_pct: 8.5, pl_price: 100, pl_currency: -40, dividends: 25 }),
          holding({ symbol: 'CBA.AX' }),
        ]}
      />,
    )
    const tooltipOf = (symbol: string) => tileFor(symbol).querySelector('title')!.textContent!
    expect(tooltipOf('SGOV')).toContain(
      'P/L: -0.8% ($-56)\n\u00a0\u00a0from price: +0.1% ($7)\n\u00a0\u00a0from currency: -0.9% ($-63)\nToday',
    )
    // Dividends are part of the P/L too, so they join the parts once paid.
    expect(tooltipOf('RSP')).toContain('\u00a0\u00a0from dividends: +2.5% ($25)')
    // Bought in AUD: there is nothing to split.
    expect(tooltipOf('CBA.AX')).not.toContain('from price')
  })

  // The two metrics answer different questions and a position is routinely up
  // overall while down today, so the toggle has to change what is rendered.
  it('switches the printed figure between total and today', () => {
    render(<HoldingsHeatMap holdings={[holding({ symbol: 'ONE.AX', pl_pct: 30, change_percent: -2.5 })]} />)
    const tile = () => tileFor('ONE.AX') as unknown as HTMLElement

    expect(within(tile()).getByText('+30.0%')).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: 'Today' }))
    expect(within(tile()).getByText('-2.5%')).toBeTruthy()
    expect(within(tile()).queryByText('+30.0%')).toBeNull()
  })

  /**
   * A day's move rarely leaves ±5%, so reading it against the lifetime-return
   * band would paint the whole map neutral and say nothing. The legend has to
   * follow the band it is describing.
   */
  it('narrows the colour band, and its legend, for today\'s move', () => {
    const fillOf = (symbol: string) =>
      tileFor(symbol).querySelector('rect')!.getAttribute('fill')!
    render(<HoldingsHeatMap holdings={[holding({ symbol: 'ONE.AX', pl_pct: 6, change_percent: 6 })]} />)

    // 6% of a ±50% band is barely a tint — nearly the neutral background.
    expect(screen.getByText('+50%')).toBeTruthy()
    expect(fillOf('ONE.AX')).toBe('rgb(211, 222, 216)')

    fireEvent.click(screen.getByRole('button', { name: 'Today' }))
    // The same figure is a full-strength move against the tighter band.
    expect(screen.getByText('+5%')).toBeTruthy()
    expect(fillOf('ONE.AX')).toBe('rgb(27, 94, 32)')
  })

  /**
   * With P/L measured from purchase, a long-held position reads in the
   * thousands of percent and says nothing about lately. The periods colour
   * every tile by the holding's price move over that span instead.
   */
  describe('periods', () => {
    const fillOf = (symbol: string) => tileFor(symbol).querySelector('rect')!.getAttribute('fill')!
    const tooltipOf = (symbol: string) => tileFor(symbol).querySelector('title')!.textContent!

    it('offers the periods beside P/L and Today, as the same pills', () => {
      render(<HoldingsHeatMap holdings={[holding({ symbol: 'ONE.AX' })]} />)
      const buttons = within(screen.getByRole('group', { name: 'Colour tiles by' })).getAllByRole('button')
      expect(buttons.map((b) => b.textContent)).toEqual([
        'P/L', 'Today', '1 week', '1 month', '3 months', '1 year', '2 years',
      ])
      for (const button of buttons) expect(button.className).toContain('sma-button')
    })

    it('prints each tile\'s move over the chosen period', () => {
      const moves = { '1w': 2.5, '1m': -4, '3m': 12, '1y': 30, '2y': -45 }
      render(<HoldingsHeatMap holdings={[holding({ symbol: 'CSL.AX', pl_pct: 3157, change_percent: 0.3, moves })]} />)
      const tile = () => tileFor('CSL.AX') as unknown as HTMLElement

      fireEvent.click(screen.getByRole('button', { name: '1 month' }))
      expect(within(tile()).getByText('-4.0%')).toBeTruthy()
      expect(within(tile()).queryByText('+3157%')).toBeNull()
      expect(screen.getByRole('button', { name: '1 month' }).getAttribute('aria-pressed')).toBe('true')

      fireEvent.click(screen.getByRole('button', { name: '2 years' }))
      expect(within(tile()).getByText('-45.0%')).toBeTruthy()
      expect(screen.getByRole('img').getAttribute('aria-label')).toBe(
        'Holdings sized by value and coloured by the price move over 2 years',
      )
    })

    /**
     * A week's move rarely passes ±10% and two years' runs to the hundreds, so
     * one band would leave the short periods blank or the long ones saturated.
     * Each period gets its own, and the legend states it.
     */
    it('gives each period its own band, and the legend follows it', () => {
      const moves = { '1w': 10, '1m': 10, '3m': 10, '1y': 10, '2y': 10 }
      render(<HoldingsHeatMap holdings={[holding({ symbol: 'ONE.AX', moves })]} />)

      const fills: string[] = []
      for (const [label, cap] of [['1 week', 10], ['1 month', 20], ['3 months', 40], ['1 year', 75], ['2 years', 100]] as const) {
        fireEvent.click(screen.getByRole('button', { name: label }))
        expect(screen.getByText(`−${cap}%`)).toBeTruthy()
        expect(screen.getByText(`+${cap}%`)).toBeTruthy()
        fills.push(fillOf('ONE.AX'))
      }
      // The same +10% is a full-strength week, a tenth of the way to a full
      // two years, and a different shade for every band between.
      expect(fills[0]).toBe('rgb(27, 94, 32)')
      expect(fills[4]).toBe('rgb(215, 225, 220)')
      expect(new Set(fills).size).toBe(5)
    })

    /**
     * Grey for no data and near-white for no move are different answers. A
     * holding listed eighteen months ago has no two-year move, and painting it
     * as a flat 0% would say it went nowhere.
     */
    it('shows a period the history does not reach as no data, not as 0%', () => {
      render(
        <HoldingsHeatMap
          holdings={[
            holding({ symbol: 'NEW', current_value: 5000, moves: { '1w': 1.2, '1y': 8, '2y': null } }),
            holding({ symbol: 'FLAT', current_value: 5000, moves: { '2y': 0 } }),
          ]}
        />,
      )
      fireEvent.click(screen.getByRole('button', { name: '2 years' }))

      expect(fillOf('NEW')).toBe('rgb(207, 216, 220)')
      expect(within(tileFor('NEW') as unknown as HTMLElement).getByText('—')).toBeTruthy()
      expect(tooltipOf('NEW')).toContain('2 years: — (not enough price history)')

      expect(fillOf('FLAT')).toBe('rgb(236, 239, 241)')
      expect(within(tileFor('FLAT') as unknown as HTMLElement).getByText('+0.0%')).toBeTruthy()

      // A period missing from the figures, rather than null, is no data too.
      fireEvent.click(screen.getByRole('button', { name: '3 months' }))
      expect(fillOf('NEW')).toBe('rgb(207, 216, 220)')
    })

    // An API older than the periods sends no `moves` at all. That has to read
    // as no data rather than paint every tile with an undefined colour.
    it('treats an API that sends no moves as no data', () => {
      render(<HoldingsHeatMap holdings={[holding({ symbol: 'OLD.AX' })]} />)
      fireEvent.click(screen.getByRole('button', { name: '1 week' }))
      expect(fillOf('OLD.AX')).toBe('rgb(207, 216, 220)')
      expect(tooltipOf('OLD.AX')).toContain('1 week: —')
    })

    it('adds the chosen period to the tooltip, keeping P/L and Today', () => {
      render(<HoldingsHeatMap holdings={[holding({ symbol: 'ONE.AX', moves: { '3m': 12.34, '1y': -5 } })]} />)
      expect(tooltipOf('ONE.AX')).not.toContain('3 months')

      fireEvent.click(screen.getByRole('button', { name: '3 months' }))
      expect(tooltipOf('ONE.AX')).toContain('3 months: +12.3%')
      expect(tooltipOf('ONE.AX')).not.toContain('1 year')
      expect(tooltipOf('ONE.AX')).toContain('P/L: +20.0%')
      expect(tooltipOf('ONE.AX')).toContain('Today: +1.0%')

      fireEvent.click(screen.getByRole('button', { name: 'P/L' }))
      expect(tooltipOf('ONE.AX')).not.toContain('3 months')
    })
  })

  describe('sector grouping', () => {
    const mixed = [
      holding({ symbol: 'BHP.AX', sector: 'Materials', current_value: 6000 }),
      holding({ symbol: 'RIO.AX', sector: 'Materials', current_value: 2000 }),
      holding({ symbol: 'CBA.AX', sector: 'Financials', current_value: 1500 }),
      holding({ symbol: 'MYSTERY.AX', sector: null, current_value: 500 }),
    ]

    const frames = () => Array.from(document.querySelectorAll('svg > rect')).slice(1)

    it('labels each sector with its share of the portfolio', () => {
      render(<HoldingsHeatMap holdings={mixed} />)
      expect(screen.getByText('Materials 80%')).toBeTruthy()
      expect(screen.getByText('Financials 15%')).toBeTruthy()
    })

    // A holding nobody has categorised still has to appear somewhere, and
    // "Unassigned" has to stay distinct from the selectable "Others" sector.
    it('collects uncategorised holdings under Unassigned', () => {
      render(<HoldingsHeatMap holdings={mixed} />)
      expect(screen.getByText(/^Unassigned/)).toBeTruthy()
      expect(tileFor('MYSTERY.AX')).toBeTruthy()
      expect(tileFor('MYSTERY.AX').querySelector('title')!.textContent).toContain('Sector: Unassigned')
    })

    /**
     * A narrow sector frame cannot always show its own name — a one-holding
     * "Consumer Discretionary" group does not fit the label. The tile has to
     * carry the sector so it stays identifiable when the header is dropped.
     */
    it('names the sector on every tile, not only on the frame', () => {
      render(<HoldingsHeatMap holdings={mixed} />)
      expect(tileFor('BHP.AX').querySelector('title')!.textContent).toContain('Sector: Materials')
      expect(tileFor('CBA.AX').querySelector('title')!.textContent).toContain('Sector: Financials')
    })

    it('keeps every holding on the map when grouped', () => {
      render(<HoldingsHeatMap holdings={mixed} />)
      expect(document.querySelectorAll('svg g rect')).toHaveLength(mixed.length)
    })

    it('puts a sector\'s tiles inside that sector\'s frame', () => {
      render(<HoldingsHeatMap holdings={mixed} />)
      const materials = ['BHP.AX', 'RIO.AX'].map((s) => {
        const r = tileFor(s).querySelector('rect')!
        return {
          x: parseFloat(r.getAttribute('x')!),
          y: parseFloat(r.getAttribute('y')!),
          width: parseFloat(r.getAttribute('width')!),
          height: parseFloat(r.getAttribute('height')!),
        }
      })
      // The frame containing both is the sector's; no other sector's tile may
      // fall inside it.
      const left = Math.min(...materials.map((t) => t.x))
      const right = Math.max(...materials.map((t) => t.x + t.width))
      const top = Math.min(...materials.map((t) => t.y))
      const bottom = Math.max(...materials.map((t) => t.y + t.height))

      for (const symbol of ['CBA.AX', 'MYSTERY.AX']) {
        const r = tileFor(symbol).querySelector('rect')!
        const x = parseFloat(r.getAttribute('x')!)
        const y = parseFloat(r.getAttribute('y')!)
        const inside = x >= left - 0.001 && x < right - 0.001 && y >= top - 0.001 && y < bottom - 0.001
        expect(inside).toBe(false)
      }
    })

    it('drops the sector frames and labels when switched to flat', () => {
      render(<HoldingsHeatMap holdings={mixed} />)
      expect(frames().length).toBeGreaterThan(0)

      fireEvent.click(screen.getByRole('button', { name: 'Flat' }))
      expect(screen.queryByText('Materials 80%')).toBeNull()
      expect(frames()).toHaveLength(0)
      // Every holding is still mapped, just against each other rather than
      // within sectors.
      expect(document.querySelectorAll('svg g rect')).toHaveLength(mixed.length)
    })
  })

  /**
   * A baseline replaces the purchase as the cost basis, so there is one figure
   * per holding — but the tooltip has to say which period it covers, or a
   * holding measured from 2025 reads as if it were measured from purchase.
   */
  describe('baselined holdings', () => {
    const legacy = [
      holding({ symbol: 'CSL.AX', current_value: 8616, pl: -5126, pl_pct: -36.5, basis_date: '2025-01-01', basis_price: 281.18 }),
      holding({ symbol: 'RECENT.AX', current_value: 2000, pl: 240, pl_pct: 12 }),
    ]

    it('colours by the figure the server sent, whatever period it covers', () => {
      render(<HoldingsHeatMap holdings={legacy} />)
      expect(within(tileFor('CSL.AX') as unknown as HTMLElement).getByText('-36.5%')).toBeTruthy()
      expect(within(tileFor('RECENT.AX') as unknown as HTMLElement).getByText('+12.0%')).toBeTruthy()
    })

    it('names the baseline period in the tooltip, and says nothing when there is none', () => {
      render(<HoldingsHeatMap holdings={legacy} />)
      expect(tileFor('CSL.AX').querySelector('title')!.textContent).toContain('P/L since 2025-01-01: -36.5%')
      const recent = tileFor('RECENT.AX').querySelector('title')!.textContent!
      expect(recent).toContain('P/L: +12.0%')
      expect(recent).not.toContain('since')
    })

    // An API older than the baseline omits the field entirely rather than
    // sending null. A strict `!== null` guard sails past `undefined` and the
    // next `.toFixed()` blanks the whole screen.
    it('survives an API that omits the baseline fields outright', () => {
      const older = holding({ symbol: 'OLD.AX', pl_pct: 12 }) as unknown as Record<string, unknown>
      delete older.basis_date
      delete older.basis_price
      render(<HoldingsHeatMap holdings={[older as unknown as PortfolioHolding]} />)
      expect(within(tileFor('OLD.AX') as unknown as HTMLElement).getByText('+12.0%')).toBeTruthy()
      expect(tileFor('OLD.AX').querySelector('title')!.textContent).not.toContain('since')
    })
  })

  it('sends a clicked tile to the caller so it can drive the chart', () => {
    const onSelectSymbol = vi.fn()
    render(
      <HoldingsHeatMap
        holdings={[holding({ symbol: 'A.AX', current_value: 6000 }), holding({ symbol: 'B.AX', current_value: 4000 })]}
        onSelectSymbol={onSelectSymbol}
      />,
    )
    fireEvent.click(tileFor('B.AX'))
    expect(onSelectSymbol).toHaveBeenCalledWith('B.AX')
  })
})
