// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, waitFor, within, cleanup, fireEvent } from '@testing-library/react'
import HoldingsManager from './HoldingsManager'
import { invalidateAppConfig } from '../utils/appConfig'
import { COLLAPSED_CARDS_KEY } from '../utils/collapsedCards'

// The form logic under test lives entirely client-side; every apiClient
// call is mocked so no test touches the network.
vi.mock('../services/api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../services/api')>()
  return {
    ...actual,
    apiClient: {
      getMeta: vi.fn(),
      getHoldings: vi.fn(),
      getCashAccounts: vi.fn(),
      getSymbolInfo: vi.fn(),
      getHoldingsSymbolFields: vi.fn(),
      getPortfolioHoldings: vi.fn(),
      getPortfolioLots: vi.fn(),
      // Loading any holding auto-selects it for the chart card, so the price
      // chart mounts and reaches for these as soon as a test supplies one.
      getConfig: vi.fn(),
      updateConfig: vi.fn(),
      getPriceHistory: vi.fn(),
      getChartDrawings: vi.fn(),
      getFxRateForDate: vi.fn(),
      getCurrentPrices: vi.fn(),
      addHoldingTransaction: vi.fn(),
      addHoldingFromWatchlist: vi.fn(),
      updateHoldingsSymbolFields: vi.fn(),
      refreshDividends: vi.fn(),
      renameHoldingSymbol: vi.fn(),
    },
  }
})

import { apiClient } from '../services/api'
const mocked = apiClient as unknown as Record<string, ReturnType<typeof vi.fn>>

const TODAY = new Date().toISOString().slice(0, 10)

function makeTransaction(overrides: Record<string, unknown> = {}) {
  return {
    id: 1,
    symbol: 'TST.AX',
    transaction_type: 'purchase',
    date: '2026-01-05',
    quantity: 10,
    price: 5,
    amount: null,
    brokerage: null,
    notes: null,
    created_at: '2026-01-05T00:00:00Z',
    dividends_total: 0,
    currency: 'AUD',
    original_price: null,
    fx_rate: null,
    custom_fields: {},
    ...overrides,
  }
}

const confirmMock = vi.fn()

beforeEach(() => {
  cleanup()
  vi.stubGlobal('confirm', confirmMock)
  confirmMock.mockReset()
  for (const fn of Object.values(mocked)) fn.mockReset?.()
  mocked.getMeta.mockResolvedValue({
    currencies: ['AUD', 'USD'],
    sectors: ['Materials'],
    holdings_custom_fields: [],
    watchlist_custom_fields: [],
    dashboard_custom_lists: [],
    reserved_holdings_keys: [],
    reserved_watchlist_keys: [],
  })
  mocked.getHoldings.mockResolvedValue([])
  mocked.getCashAccounts.mockResolvedValue([])
  mocked.getSymbolInfo.mockResolvedValue([])
  mocked.getHoldingsSymbolFields.mockResolvedValue({})
  mocked.getPortfolioHoldings.mockResolvedValue({ holdings: [], fx_rates: {} })
  mocked.getPortfolioLots.mockResolvedValue({ lots: [] })
  mocked.getConfig.mockResolvedValue({})
  mocked.updateConfig.mockResolvedValue(undefined)
  // The config read is cached module-wide, so one test's stored collapse state
  // would otherwise be reused by every later one.
  invalidateAppConfig()
  mocked.getPriceHistory.mockResolvedValue([])
  mocked.getChartDrawings.mockResolvedValue([])
  mocked.getFxRateForDate.mockResolvedValue(null)
  mocked.addHoldingTransaction.mockResolvedValue(makeTransaction({ id: 99 }))
  mocked.addHoldingFromWatchlist.mockResolvedValue({ transaction: makeTransaction({ id: 99, symbol: 'SOL.AX' }), removed_memberships: 1 })
  mocked.updateHoldingsSymbolFields.mockResolvedValue({})
})

const input = (placeholder: string | RegExp) => screen.getByPlaceholderText(placeholder) as HTMLInputElement
const submitButton = () => screen.getByRole('button', { name: 'Record Transaction' })
const triggerButton = () => screen.getByRole('button', { name: /Record Holding/ })

// The record form lives in a dialog, so every test that reaches for a field has
// to open it first. A prefill opens the dialog on its own; clicking again is a
// no-op, so this stays a single path for both cases.
async function renderManager(props: Partial<Parameters<typeof HoldingsManager>[0]> = {}) {
  const result = render(<HoldingsManager scope="local" onLoading={() => {}} {...props} />)
  await waitFor(() => expect((triggerButton() as HTMLButtonElement).disabled).toBe(false))
  fireEvent.click(triggerButton())
  await waitFor(() => expect((submitButton() as HTMLButtonElement).disabled).toBe(false))
  return result
}

function fillPurchase(symbol: string, qty: string, price: string) {
  fireEvent.change(input(/Symbol \(e\.g\./), { target: { value: symbol } })
  fireEvent.change(input('Quantity'), { target: { value: qty } })
  fireEvent.change(input(/Price per share/), { target: { value: price } })
}

function submitForm() {
  fireEvent.submit(submitButton().closest('form')!)
}

/** A holding as the heat map needs it — area from value, colour from return. */
function makePortfolioHolding(over: Record<string, unknown> & { symbol: string }) {
  return {
    long_name: null, instrument_type: null, is_etf: false, is_international: false,
    currency: 'AUD', sector: null, notes: null, fields: {}, shares: 10, invested: 900,
    avg_cost: 90, native_avg_cost: 90, current_price: 100, native_current_price: 100,
    price_source: 'cache', price_date: '2026-09-17', change: 1, change_percent: 1,
    volume: 1000, current_value: 1000, dividends: 0, pl: 100, pl_pct: 11.1,
    sma50: null, native_sma50: null, sma150: null, native_sma150: null,
    ema40w: null, native_ema40w: null, day_pl: null, stop_loss: null, is_trailing_sell: false,
    ...over,
  }
}

describe('local vs international screens', () => {
  const MIXED = [
    makePortfolioHolding({ symbol: 'BHP.AX' }),
    makePortfolioHolding({ symbol: 'VAS.AX', is_etf: true }),
    makePortfolioHolding({ symbol: 'EXPD', is_international: true }),
    makePortfolioHolding({ symbol: 'IVV.AX', is_etf: true, is_international: true }),
  ]

  /**
   * One open purchase behind every symbol in MIXED. The summary card renders
   * "No holdings configured" until there are transactions, so every scope test
   * needs these whether or not it looks at the lot table.
   */
  const PURCHASES = MIXED.map((h, i) => makeTransaction({ id: i + 1, symbol: h.symbol }))
  const LOTS = MIXED.map((_, i) => ({ transaction_id: i + 1, remaining: 10, current_value: 100, unrealised_pl: 5 }))

  async function renderScope(scope: 'local' | 'international') {
    mocked.getPortfolioHoldings.mockResolvedValue({ holdings: MIXED, fx_rates: {} })
    mocked.getHoldings.mockResolvedValue(PURCHASES)
    mocked.getPortfolioLots.mockResolvedValue({ lots: LOTS })
    render(<HoldingsManager scope={scope} onLoading={() => {}} />)
    await waitFor(() => expect((triggerButton() as HTMLButtonElement).disabled).toBe(false))
  }

  const sectionHeadings = () =>
    [...document.querySelectorAll('.holdings-card h3')].map((h) => h.textContent)

  it('shows only the local sections on the local screen', async () => {
    await renderScope('local')
    await waitFor(() => expect(sectionHeadings()).toEqual(['Equities', 'ETFs', 'Active Holdings']))
    expect(screen.getByRole('button', { name: 'Equities Heat Map' })).toBeTruthy()
    expect(screen.queryByRole('button', { name: 'International Equities Heat Map' })).toBeNull()
  })

  it('shows only the international sections on the international screen', async () => {
    await renderScope('international')
    await waitFor(() =>
      expect(sectionHeadings()).toEqual(['International Equities', 'International ETFs', 'Active Holdings']),
    )
    expect(screen.getByRole('button', { name: 'International Equities Heat Map' })).toBeTruthy()
    expect(screen.queryByRole('button', { name: 'Equities Heat Map' })).toBeNull()
  })

  // The chart picker is the screen's own list of positions; offering the other
  // screen's symbols would chart a holding this screen does not show.
  it('offers only this screen\'s symbols to the chart picker', async () => {
    await renderScope('international')
    const picker = (await screen.findByLabelText('Symbol')) as HTMLSelectElement
    expect([...picker.options].map((o) => o.value)).toEqual(['EXPD', 'IVV.AX'])
  })

  it('opens the chart on one of its own holdings, not the first overall', async () => {
    await renderScope('international')
    const picker = (await screen.findByLabelText('Symbol')) as HTMLSelectElement
    expect(picker.value).toBe('EXPD')
  })

  it('says where the holdings went when this screen has none', async () => {
    mocked.getPortfolioHoldings.mockResolvedValue({
      holdings: [makePortfolioHolding({ symbol: 'BHP.AX' })],
      fx_rates: {},
    })
    mocked.getHoldings.mockResolvedValue([makeTransaction({ id: 1, symbol: 'BHP.AX' })])
    mocked.getPortfolioLots.mockResolvedValue({
      lots: [{ transaction_id: 1, remaining: 10, current_value: 100, unrealised_pl: 5 }],
    })
    render(<HoldingsManager scope="international" onLoading={() => {}} />)
    await waitFor(() => expect((triggerButton() as HTMLButtonElement).disabled).toBe(false))
    await screen.findByText(/No international holdings/)
    // Not the "nothing configured yet" message — there are holdings, elsewhere.
    expect(screen.queryByText('No holdings configured.')).toBeNull()
  })

  it('still says nothing is configured when the portfolio is genuinely empty', async () => {
    render(<HoldingsManager scope="local" onLoading={() => {}} />)
    await waitFor(() => expect((triggerButton() as HTMLButtonElement).disabled).toBe(false))
    await screen.findByText('No holdings configured.')
  })

  it('lists only this screen\'s purchases in the lot table', async () => {
    await renderScope('local')
    const table = document.querySelector('.holdings-table-wrapper')! as HTMLElement
    await waitFor(() => expect(within(table).getByText('BHP.AX')).toBeTruthy())
    expect(within(table).getByText('VAS.AX')).toBeTruthy()
    // Both have an open lot; only locality keeps them off this screen.
    expect(within(table).queryByText('EXPD')).toBeNull()
    expect(within(table).queryByText('IVV.AX')).toBeNull()
  })
})

describe('which currency a price leads with', () => {
  const EXPD = makePortfolioHolding({
    symbol: 'EXPD', is_international: true, currency: 'USD',
    current_price: 291.5, native_current_price: 190.92,
    sma150: 282.5, native_sma150: 185.0, current_value: 2915, dividends: 0,
    sma50: 305.0, native_sma50: 200.0, ema40w: 274.0, native_ema40w: 180.0, invested: 2000,
  })
  const BHP = makePortfolioHolding({
    symbol: 'BHP.AX', current_price: 41.25, native_current_price: 41.25,
    sma150: 38.0, native_sma150: 38.0, current_value: 1000,
    sma50: 40.0, native_sma50: 40.0, ema40w: 36.0, native_ema40w: 36.0,
  })

  async function renderScope(scope: 'local' | 'international', tx: Record<string, unknown>) {
    mocked.getPortfolioHoldings.mockResolvedValue({ holdings: [EXPD, BHP], fx_rates: { USD: 1.527 } })
    mocked.getHoldings.mockResolvedValue([makeTransaction(tx)])
    mocked.getPortfolioLots.mockResolvedValue({
      lots: [{ transaction_id: 1, remaining: 10, current_value: 2915, unrealised_pl: 5 }],
    })
    render(<HoldingsManager scope={scope} onLoading={() => {}} />)
    await waitFor(() => expect((triggerButton() as HTMLButtonElement).disabled).toBe(false))
  }

  const usdPurchase = { id: 1, symbol: 'EXPD', currency: 'USD', price: 275.0, original_price: 180.1 }

  it('leads with the traded currency on the international screen, AUD in brackets', async () => {
    await renderScope('international', usdPurchase)
    // Summary card: "10@190.92 (A$291.50)" — the card's USD tag names the
    // currency, so the figure does not repeat it.
    const card = await screen.findByText(/10@190\.92/)
    expect(card.textContent).toContain('(A$291.50)')
    expect(card.textContent).not.toContain('USD')

    const table = document.querySelector('.holdings-table-wrapper')! as HTMLElement
    const price = within(table).getByText(/USD 180\.10/)
    expect(price.textContent).toContain('(A$275.00)')
    // The column no longer promises AUD.
    expect(within(table).getByRole('columnheader', { name: 'Price' })).toBeTruthy()
  })

  it('keeps AUD leading on the local screen', async () => {
    await renderScope('local', { id: 1, symbol: 'BHP.AX', price: 40.0 })
    expect(await screen.findByText(/10@\$41\.25/)).toBeTruthy()
    const table = document.querySelector('.holdings-table-wrapper')! as HTMLElement
    expect(within(table).getByText(/\$40\.00/)).toBeTruthy()
    expect(within(table).getByRole('columnheader', { name: 'Price (AUD)' })).toBeTruthy()
  })

  it('reads the 150SMA in the stock\'s currency and marks every AUD total with A$', async () => {
    await renderScope('international', usdPurchase)
    expect(await screen.findByText(/150SMA: 185\.00/)).toBeTruthy()
    expect(screen.getByText(/50SMA: 200\.00/)).toBeTruthy()
    expect(screen.getByText(/40W EMA: 180\.00/)).toBeTruthy()
    expect(screen.getByText(/Current value: A\$2915\.00/)).toBeTruthy()
    expect(screen.getByText(/Dividends: A\$0\.00/)).toBeTruthy()
    // The P/L is an AUD figure too — current value less cost, plus income.
    // Scoped to the card: a heat map tile's tooltip quotes a P/L as well.
    const intlCard = screen.getByText(/Current value: A\$2915\.00/).closest('div')!.parentElement!
    expect(within(intlCard).getByText(/P\/L: \+A\$/)).toBeTruthy()
  })

  it('leaves the local card in plain AUD, with no suffix', async () => {
    await renderScope('local', { id: 1, symbol: 'BHP.AX', price: 40.0 })
    expect(await screen.findByText(/150SMA: \$38\.00/)).toBeTruthy()
    expect(screen.getByText(/50SMA: \$40\.00/)).toBeTruthy()
    expect(screen.getByText(/40W EMA: \$36\.00/)).toBeTruthy()
    const value = screen.getByText(/Current value: \$1000\.00/)
    expect(value.textContent).not.toContain('A$')
    const card = value.closest('div')!.parentElement!
    expect(within(card).getByText(/P\/L: /).textContent).not.toContain('A$')
  })

  // Every total adds up across currencies, so all four are AUD — and say so
  // on the screen where the figures beside them are not.
  it('marks the Holdings Summary totals as AUD on the international screen', async () => {
    await renderScope('international', usdPurchase)
    const header = (await screen.findAllByText(/Net Invested:/))[0].parentElement!
    expect(within(header).getByText(/Net Invested:/).textContent).toContain('A$2,000.00')
    expect(within(header).getByText(/Current Value:/).textContent).toContain('A$2,915.00')
    expect(within(header).getByText(/Dividends:/).textContent).toContain('A$0.00')
    expect(within(header).getByText(/P\/L:/).textContent).toContain('A$915.00')
  })

  it('leaves the local screen\'s totals in a plain $', async () => {
    await renderScope('local', { id: 1, symbol: 'BHP.AX', price: 40.0 })
    const header = (await screen.findAllByText(/Net Invested:/))[0].parentElement!
    expect(within(header).getByText(/Net Invested:/).textContent).not.toContain('A$')
    expect(within(header).getByText(/P\/L:/).textContent).not.toContain('A$')
  })

  // A foreign stock bought in AUD has no native figure to lead with, and the
  // transaction's own currency is what says so — not the symbol's.
  it('shows a lone AUD price for a foreign holding bought in AUD', async () => {
    await renderScope('international', { id: 1, symbol: 'EXPD', currency: 'AUD', price: 275.0, original_price: null })
    const table = document.querySelector('.holdings-table-wrapper')! as HTMLElement
    const cell = within(table).getByText('$275.00')
    expect(cell.textContent).not.toContain('(')
  })
})

describe("today's P/L", () => {
  const holding = (over: Record<string, unknown> & { symbol: string }) =>
    makePortfolioHolding({ is_international: true, currency: 'USD', ...over })

  async function renderWith(holdings: unknown[]) {
    mocked.getPortfolioHoldings.mockResolvedValue({ holdings, fx_rates: { USD: 1.5 } })
    mocked.getHoldings.mockResolvedValue([makeTransaction({ id: 1, symbol: 'EXPD' })])
    mocked.getPortfolioLots.mockResolvedValue({
      lots: [{ transaction_id: 1, remaining: 10, current_value: 100, unrealised_pl: 5 }],
    })
    render(<HoldingsManager scope="international" onLoading={() => {}} />)
    await waitFor(() => expect((triggerButton() as HTMLButtonElement).disabled).toBe(false))
    return (await screen.findAllByText(/Net Invested:/))[0].parentElement!
  }

  it("totals the day's money across the screen's holdings", async () => {
    const header = await renderWith([
      holding({ symbol: 'EXPD', day_pl: 120, current_value: 2120 }),
      holding({ symbol: 'IVV.AX', is_etf: true, day_pl: -20, current_value: 880 }),
    ])
    // 120 − 20 against an opening 3000 − 100.
    expect(within(header).getByText(/Today's P\/L:/).textContent).toContain('+A$100.00')
    expect(within(header).getByText(/Today's P\/L:/).textContent).toContain('+3.4%')
  })

  // A stale or manual price has no change behind it, so counting it as a flat
  // day would quietly drag the percentage toward zero.
  it('leaves out a holding whose quote carried no change', async () => {
    const header = await renderWith([
      holding({ symbol: 'EXPD', day_pl: 120, current_value: 2120 }),
      holding({ symbol: 'IVV.AX', is_etf: true, day_pl: null, current_value: 5000 }),
    ])
    expect(within(header).getByText(/Today's P\/L:/).textContent).toContain('+A$120.00')
    expect(within(header).getByText(/Today's P\/L:/).textContent).toContain('+6.0%')
  })

  it('shows nothing at all when no holding has a change', async () => {
    const header = await renderWith([holding({ symbol: 'EXPD', day_pl: null })])
    expect(within(header).queryByText(/Today's P\/L:/)).toBeNull()
    expect(within(header).getByText(/Net Invested:/)).toBeTruthy()
  })
})

describe('per-section heat maps', () => {
  const ONE_OF_EACH = [
    makePortfolioHolding({ symbol: 'BHP.AX' }),
    makePortfolioHolding({ symbol: 'EXPD', is_international: true }),
    makePortfolioHolding({ symbol: 'VAS.AX', is_etf: true }),
    makePortfolioHolding({ symbol: 'IVV.AX', is_etf: true, is_international: true }),
  ]

  const cardFor = (title: string) =>
    screen.getByRole('button', { name: title }).closest('.manager-card')! as HTMLElement

  async function renderWithHoldings(holdings: unknown[], scope: 'local' | 'international' = 'local') {
    mocked.getPortfolioHoldings.mockResolvedValue({ holdings, fx_rates: {} })
    render(<HoldingsManager scope={scope} onLoading={() => {}} />)
    await waitFor(() => expect((triggerButton() as HTMLButtonElement).disabled).toBe(false))
  }

  it('gives each of the screen\'s sections its own map, and drops the overall one', async () => {
    await renderWithHoldings(ONE_OF_EACH)
    await screen.findByRole('button', { name: 'Equities Heat Map' })

    // The section's own tile is in its map, and its neighbour's is not: the
    // whole point of the split is that each map is scaled to one section.
    const equities = cardFor('Equities Heat Map')
    expect(within(equities).getByText('BHP.AX')).toBeTruthy()
    expect(within(equities).queryByText('VAS.AX')).toBeNull()
    expect(within(cardFor('ETFs Heat Map')).getByText('VAS.AX')).toBeTruthy()

    // The map of everything now lives on the Dashboard.
    expect(screen.queryByText('Heat Map')).toBeNull()
  })

  it('folds one section without touching the others', async () => {
    await renderWithHoldings(ONE_OF_EACH)
    const toggle = (title: string) => screen.getByRole('button', { name: title })
    await waitFor(() => expect(toggle('Equities Heat Map')).toBeTruthy())

    fireEvent.click(toggle('Equities Heat Map'))
    expect(toggle('Equities Heat Map').getAttribute('aria-expanded')).toBe('false')
    expect(within(cardFor('Equities Heat Map')).queryByText('BHP.AX')).toBeNull()

    // The three siblings share one stored key; folding one must not shut them.
    expect(toggle('ETFs Heat Map').getAttribute('aria-expanded')).toBe('true')
    expect(within(cardFor('ETFs Heat Map')).getByText('VAS.AX')).toBeTruthy()
  })

  it('adds to the stored set rather than replacing it', async () => {
    mocked.getConfig.mockResolvedValue({ [COLLAPSED_CARDS_KEY]: JSON.stringify(['heatmap-ETFs']) })
    await renderWithHoldings(ONE_OF_EACH)
    const equities = await screen.findByRole('button', { name: 'Equities Heat Map' })
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'ETFs Heat Map' }).getAttribute('aria-expanded')).toBe('false'),
    )

    fireEvent.click(equities)
    await waitFor(() =>
      expect(mocked.updateConfig).toHaveBeenCalledWith(
        COLLAPSED_CARDS_KEY,
        JSON.stringify(['heatmap-ETFs', 'heatmap-Equities'].sort()),
      ),
    )
  })

  // A key written by an older version must not leave every map shut with
  // nothing on screen to explain it.
  it('opens every map when the stored value is unreadable', async () => {
    mocked.getConfig.mockResolvedValue({ [COLLAPSED_CARDS_KEY]: 'not json' })
    await renderWithHoldings(ONE_OF_EACH)
    await screen.findByRole('button', { name: 'Equities Heat Map' })
    expect(within(cardFor('Equities Heat Map')).getByText('BHP.AX')).toBeTruthy()
    expect(cardFor('Equities Heat Map').querySelector('button')!.getAttribute('aria-expanded')).toBe('true')
  })

  it('skips a section with no holdings rather than framing an empty map', async () => {
    await renderWithHoldings([makePortfolioHolding({ symbol: 'BHP.AX' })])
    await screen.findByRole('button', { name: 'Equities Heat Map' })
    expect(screen.queryByText('ETFs Heat Map')).toBeNull()
  })

  it('shows no map at all before any holding has loaded', async () => {
    await renderWithHoldings([])
    expect(screen.queryByText(/Heat Map/)).toBeNull()
  })
})

describe('watchlist prefill', () => {
  it('lands every prefill value in its input — including the stop loss', async () => {
    const onPrefillConsumed = vi.fn()
    await renderManager({
      prefill: {
        symbol: 'SOL.AX',
        price: 44.02,
        notes: 'from watchlist',
        customFields: { sector: 'Materials', stop_loss: '41.54' },
      },
      onPrefillConsumed,
    })
    expect(input(/Symbol \(e\.g\./).value).toBe('SOL.AX')
    expect(input(/Price per share/).value).toBe('44.02')
    expect(input('Notes (optional)').value).toBe('from watchlist')
    expect(input('Stop Loss Price (optional)').value).toBe('41.54')
    expect(input('Trailing Sell % (optional)').value).toBe('')
    expect((screen.getByTitle('Sector') as HTMLSelectElement).value).toBe('Materials')
    expect(onPrefillConsumed).toHaveBeenCalled()
  })

  it('routes a prefilled save through the atomic move-from-watchlist call', async () => {
    const onPrefillSaved = vi.fn()
    await renderManager({
      prefill: { symbol: 'SOL.AX', price: 44.02 },
      onPrefillSaved,
    })
    fireEvent.change(input('Quantity'), { target: { value: '5' } })
    submitForm()
    await waitFor(() => expect(mocked.addHoldingFromWatchlist).toHaveBeenCalled())
    expect(mocked.addHoldingTransaction).not.toHaveBeenCalled()
    await waitFor(() => expect(onPrefillSaved).toHaveBeenCalledWith('SOL.AX'))
  })
})

describe('over-sell confirmation', () => {
  it('asks before recording a sale beyond the held quantity and aborts on cancel', async () => {
    mocked.getHoldings.mockResolvedValue([makeTransaction()]) // 10 shares held
    await renderManager()
    fireEvent.change(input(/Symbol \(e\.g\./), { target: { value: 'TST.AX' } })
    fireEvent.change(screen.getAllByRole('combobox')[0], { target: { value: 'sale' } })
    fireEvent.change(input('Quantity'), { target: { value: '15' } })
    fireEvent.change(input(/Price per share/), { target: { value: '6' } })

    confirmMock.mockReturnValue(false)
    submitForm()
    await waitFor(() => expect(confirmMock).toHaveBeenCalled())
    expect(confirmMock.mock.calls[0][0]).toContain('only hold 10.00 TST.AX')
    expect(mocked.addHoldingTransaction).not.toHaveBeenCalled()
  })

  it('records the over-sell with confirm=true once acknowledged', async () => {
    mocked.getHoldings.mockResolvedValue([makeTransaction()])
    await renderManager()
    fireEvent.change(input(/Symbol \(e\.g\./), { target: { value: 'TST.AX' } })
    fireEvent.change(screen.getAllByRole('combobox')[0], { target: { value: 'sale' } })
    fireEvent.change(input('Quantity'), { target: { value: '15' } })
    fireEvent.change(input(/Price per share/), { target: { value: '6' } })

    confirmMock.mockReturnValue(true)
    submitForm()
    await waitFor(() => expect(mocked.addHoldingTransaction).toHaveBeenCalled())
    const payload = mocked.addHoldingTransaction.mock.calls[0][0]
    expect(payload.confirm).toBe(true)
    expect(payload.quantity).toBe(15)
  })

  it('needs no confirmation for a sale within the held quantity', async () => {
    mocked.getHoldings.mockResolvedValue([makeTransaction()])
    await renderManager()
    fireEvent.change(input(/Symbol \(e\.g\./), { target: { value: 'TST.AX' } })
    fireEvent.change(screen.getAllByRole('combobox')[0], { target: { value: 'sale' } })
    fireEvent.change(input('Quantity'), { target: { value: '5' } })
    fireEvent.change(input(/Price per share/), { target: { value: '6' } })
    submitForm()
    await waitFor(() => expect(mocked.addHoldingTransaction).toHaveBeenCalled())
    expect(confirmMock).not.toHaveBeenCalled()
  })
})

describe('foreign-currency handling', () => {
  it('blocks the save when no FX rate is available', async () => {
    mocked.getFxRateForDate.mockResolvedValue(null)
    await renderManager()
    fillPurchase('TSM', '10', '100')
    fireEvent.change(screen.getAllByRole('combobox')[1], { target: { value: 'USD' } })
    await waitFor(() => expect(screen.getByText(new RegExp(`Could not fetch USD/AUD rate`))).toBeTruthy())

    submitForm()
    await waitFor(() =>
      expect(screen.getByText(new RegExp(`No USD/AUD exchange rate available for ${TODAY}`))).toBeTruthy(),
    )
    expect(mocked.addHoldingTransaction).not.toHaveBeenCalled()
  })

  it('derives the AUD price from the fetched rate and preserves the originals', async () => {
    mocked.getFxRateForDate.mockResolvedValue({ rate: 1.5, date: TODAY })
    await renderManager()
    fillPurchase('TSM', '10', '100')
    fireEvent.change(screen.getAllByRole('combobox')[1], { target: { value: 'USD' } })
    await waitFor(() => expect(screen.getByText(/1 USD = 1\.5000 AUD/)).toBeTruthy())

    submitForm()
    await waitFor(() => expect(mocked.addHoldingTransaction).toHaveBeenCalled())
    const payload = mocked.addHoldingTransaction.mock.calls[0][0]
    expect(payload.currency).toBe('USD')
    expect(payload.original_price).toBe(100)
    expect(payload.fx_rate).toBe(1.5)
    expect(payload.price).toBe(150)
  })

  it('auto-detects the currency from symbol info', async () => {
    mocked.getSymbolInfo.mockResolvedValue([
      { symbol: 'TSM', instrument_type: 'EQUITY', long_name: 'TSMC', currency: 'USD' },
    ])
    await renderManager()
    fireEvent.change(input(/Symbol \(e\.g\./), { target: { value: 'TSM' } })
    const currencySelect = screen.getAllByRole('combobox')[1] as HTMLSelectElement
    await waitFor(() => expect(currencySelect.value).toBe('USD'))
  })
})

describe('successful save', () => {
  it('reports success, closes the dialog and resets the form', async () => {
    await renderManager()
    fillPurchase('TST.AX', '10', '5')
    fireEvent.change(input('Notes (optional)'), { target: { value: 'note' } })
    submitForm()
    await waitFor(() => expect(screen.getByText(/Transaction recorded successfully/)).toBeTruthy())
    // The dialog closes on success, taking the fields with it
    expect(screen.queryByPlaceholderText(/Symbol \(e\.g\./)).toBeNull()
    // Reopening must not resurrect the saved values
    fireEvent.click(triggerButton())
    expect(input(/Symbol \(e\.g\./).value).toBe('')
    expect(input('Quantity').value).toBe('')
    expect(input(/Price per share/).value).toBe('')
    expect(input('Notes (optional)').value).toBe('')
    // Holdings are re-fetched so the tables reflect the new transaction
    expect(mocked.getHoldings.mock.calls.length).toBeGreaterThan(1)
  })

  it('keeps the dialog open and the input intact when the save fails', async () => {
    mocked.addHoldingTransaction.mockRejectedValueOnce(new Error('server exploded'))
    await renderManager()
    fillPurchase('TST.AX', '10', '5')
    submitForm()
    await waitFor(() => expect(screen.getByText(/server exploded/)).toBeTruthy())
    expect(input(/Symbol \(e\.g\./).value).toBe('TST.AX')
    expect(input('Quantity').value).toBe('10')
  })

  it('rejects a submit without a symbol', async () => {
    await renderManager()
    fireEvent.change(input('Quantity'), { target: { value: '10' } })
    fireEvent.change(input(/Price per share/), { target: { value: '5' } })
    submitForm()
    await waitFor(() => expect(screen.getByText(/Symbol is required/)).toBeTruthy())
    expect(mocked.addHoldingTransaction).not.toHaveBeenCalled()
  })
})

describe('dividend settlement account', () => {
  const accounts = [
    { id: 6, name: 'CBA Invest', currency: 'AUD', balance: 100, interest_rate: null, include_in_portfolio: true, notes: null },
    { id: 2, name: 'IBKR', currency: 'USD', balance: 500, interest_rate: null, include_in_portfolio: true, notes: null },
  ]

  // The picker used to live inside the purchase/sale branch, so a dividend had
  // nowhere to be paid into and silently never reached the cash ledger.
  it('offers an account when recording a dividend', async () => {
    mocked.getCashAccounts.mockResolvedValue(accounts)
    await renderManager()
    fireEvent.change(screen.getByDisplayValue('Purchase'), { target: { value: 'dividend' } })

    await waitFor(() => expect(screen.getByText(/Deposit into/)).toBeTruthy())
    expect(screen.getByText(/CBA Invest/)).toBeTruthy()
  })

  it('sends the chosen account with the dividend', async () => {
    mocked.getCashAccounts.mockResolvedValue(accounts)
    await renderManager()
    fireEvent.change(screen.getByDisplayValue('Purchase'), { target: { value: 'dividend' } })
    fireEvent.change(input(/Symbol \(e\.g\./), { target: { value: 'VAS.AX' } })
    fireEvent.change(input(/Dividend amount/), { target: { value: '26.01' } })

    const picker = screen.getByTitle(/paid into/) as HTMLSelectElement
    fireEvent.change(picker, { target: { value: '6' } })
    submitForm()

    await waitFor(() => expect(mocked.addHoldingTransaction).toHaveBeenCalled())
    const payload = mocked.addHoldingTransaction.mock.calls[0][0]
    expect(payload.transaction_type).toBe('dividend')
    expect(payload.amount).toBe(26.01)
    expect(payload.cash_account_id).toBe(6)
  })

  // Dividends arrive in a currency too. Recorded as AUD, a US dividend would be
  // overstated by the exchange rate.
  it('records a foreign dividend in AUD at the rate for the date', async () => {
    mocked.getCashAccounts.mockResolvedValue(accounts)
    mocked.getFxRateForDate.mockResolvedValue({ rate: 1.5, date: '2026-08-07' })
    await renderManager()
    fireEvent.change(screen.getByDisplayValue('Purchase'), { target: { value: 'dividend' } })
    fireEvent.change(screen.getByDisplayValue('AUD'), { target: { value: 'USD' } })
    fireEvent.change(input(/Symbol \(e\.g\./), { target: { value: 'NSC' } })
    await waitFor(() => expect(screen.getByPlaceholderText(/Dividend amount \(USD\)/)).toBeTruthy())
    fireEvent.change(input(/Dividend amount/), { target: { value: '5.40' } })
    submitForm()

    await waitFor(() => expect(mocked.addHoldingTransaction).toHaveBeenCalled())
    const payload = mocked.addHoldingTransaction.mock.calls[0][0]
    expect(payload.currency).toBe('USD')
    expect(payload.fx_rate).toBe(1.5)
    expect(payload.amount).toBeCloseTo(8.10, 2)
  })

  it('refuses to save a foreign dividend with no rate for the date', async () => {
    mocked.getCashAccounts.mockResolvedValue(accounts)
    mocked.getFxRateForDate.mockResolvedValue(null)
    await renderManager()
    fireEvent.change(screen.getByDisplayValue('Purchase'), { target: { value: 'dividend' } })
    fireEvent.change(screen.getByDisplayValue('AUD'), { target: { value: 'USD' } })
    fireEvent.change(input(/Symbol \(e\.g\./), { target: { value: 'NSC' } })
    fireEvent.change(input(/Dividend amount/), { target: { value: '5.40' } })
    submitForm()

    await waitFor(() => expect(screen.getByText(/No USD\/AUD exchange rate/)).toBeTruthy())
    expect(mocked.addHoldingTransaction).not.toHaveBeenCalled()
  })

  it('offers a USD account once the dividend currency is USD', async () => {
    mocked.getCashAccounts.mockResolvedValue(accounts)
    mocked.getFxRateForDate.mockResolvedValue({ rate: 1.5, date: '2026-08-07' })
    await renderManager()
    fireEvent.change(screen.getByDisplayValue('Purchase'), { target: { value: 'dividend' } })
    fireEvent.change(screen.getByDisplayValue('AUD'), { target: { value: 'USD' } })

    await waitFor(() => expect(screen.getByText(/IBKR/)).toBeTruthy())
  })
})

describe('record dialog', () => {
  it('keeps the form out of the page until the trigger is clicked', async () => {
    render(<HoldingsManager scope="local" onLoading={() => {}} />)
    await waitFor(() => expect((triggerButton() as HTMLButtonElement).disabled).toBe(false))
    expect(screen.queryByPlaceholderText(/Symbol \(e\.g\./)).toBeNull()
    fireEvent.click(triggerButton())
    expect(screen.getByPlaceholderText(/Symbol \(e\.g\./)).toBeTruthy()
  })

  // "Move to Holdings" would look broken if it filled a form nobody could see.
  it('opens itself for a watchlist prefill with no click', async () => {
    render(<HoldingsManager scope="local" onLoading={() => {}} prefill={{ symbol: 'SOL.AX', price: 44.02 }} />)
    await waitFor(() => expect(input(/Symbol \(e\.g\./).value).toBe('SOL.AX'))
  })

  it('cancels an untouched form without asking', async () => {
    await renderManager()
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
    expect(confirmMock).not.toHaveBeenCalled()
    expect(screen.queryByPlaceholderText(/Symbol \(e\.g\./)).toBeNull()
  })

  it('keeps a half-filled form when the discard is declined', async () => {
    await renderManager()
    fillPurchase('TST.AX', '10', '5')
    confirmMock.mockReturnValue(false)
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
    expect(confirmMock).toHaveBeenCalled()
    expect(input(/Symbol \(e\.g\./).value).toBe('TST.AX')
  })

  it('discards a half-filled form once the discard is confirmed', async () => {
    await renderManager()
    fillPurchase('TST.AX', '10', '5')
    confirmMock.mockReturnValue(true)
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }))
    expect(screen.queryByPlaceholderText(/Symbol \(e\.g\./)).toBeNull()
    fireEvent.click(triggerButton())
    expect(input(/Symbol \(e\.g\./).value).toBe('')
  })

  it('closes on Escape', async () => {
    await renderManager()
    fireEvent.keyDown(window, { key: 'Escape' })
    expect(screen.queryByPlaceholderText(/Symbol \(e\.g\./)).toBeNull()
  })
})

// Sector and the exit-plan fields describe a position being opened; brokerage
// is a trading cost. Neither belongs on every transaction type.
describe('fields shown per transaction type', () => {
  const purchaseOnly = [
    'Stop Loss Price (optional)',
    'Trailing Sell % (optional)',
    'Trailing Sell Date',
  ]
  const setType = (value: string) =>
    fireEvent.change(screen.getByDisplayValue('Purchase'), { target: { value } })

  it('offers all of them on a purchase', async () => {
    await renderManager()
    purchaseOnly.forEach((p) => expect(screen.getByPlaceholderText(p)).toBeTruthy())
    expect(screen.getByTitle('Sector')).toBeTruthy()
    expect(screen.getByPlaceholderText('Brokerage fee (optional)')).toBeTruthy()
  })

  it('drops sector and the exit plan on a sale, keeping brokerage', async () => {
    await renderManager()
    setType('sale')
    purchaseOnly.forEach((p) => expect(screen.queryByPlaceholderText(p)).toBeNull())
    expect(screen.queryByTitle('Sector')).toBeNull()
    expect(screen.getByPlaceholderText('Brokerage fee (optional)')).toBeTruthy()
  })

  it('drops brokerage too on a dividend', async () => {
    await renderManager()
    setType('dividend')
    purchaseOnly.forEach((p) => expect(screen.queryByPlaceholderText(p)).toBeNull())
    expect(screen.queryByTitle('Sector')).toBeNull()
    expect(screen.queryByPlaceholderText('Brokerage fee (optional)')).toBeNull()
    expect(screen.getByPlaceholderText('Notes (optional)')).toBeTruthy()
  })

  // Switching type hides the inputs but keeps their state, so the save has to
  // do its own filtering or the hidden values ride along.
  it('does not write symbol-level fields from a sale', async () => {
    await renderManager()
    fillPurchase('TST.AX', '5', '5')
    fireEvent.change(input('Stop Loss Price (optional)'), { target: { value: '4.10' } })
    setType('sale')
    confirmMock.mockReturnValue(true)
    submitForm()
    await waitFor(() => expect(mocked.addHoldingTransaction).toHaveBeenCalled())
    expect(mocked.updateHoldingsSymbolFields).not.toHaveBeenCalled()
  })

  it('does not send a leftover brokerage with a dividend', async () => {
    await renderManager()
    fireEvent.change(input(/Symbol \(e\.g\./), { target: { value: 'TST.AX' } })
    fireEvent.change(input('Brokerage fee (optional)'), { target: { value: '19.95' } })
    setType('dividend')
    fireEvent.change(input(/Dividend amount/), { target: { value: '26.01' } })
    submitForm()
    await waitFor(() => expect(mocked.addHoldingTransaction).toHaveBeenCalled())
    expect(mocked.addHoldingTransaction.mock.calls[0][0].brokerage).toBeUndefined()
  })
})
