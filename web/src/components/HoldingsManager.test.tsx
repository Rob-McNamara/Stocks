// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, waitFor, cleanup, fireEvent } from '@testing-library/react'
import HoldingsManager from './HoldingsManager'

// The form logic under test lives entirely client-side; every apiClient
// call is mocked so no test touches the network.
vi.mock('../services/api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../services/api')>()
  return {
    ...actual,
    apiClient: {
      getMeta: vi.fn(),
      getHoldings: vi.fn(),
      getSymbolInfo: vi.fn(),
      getHoldingsSymbolFields: vi.fn(),
      getPortfolioHoldings: vi.fn(),
      getPortfolioLots: vi.fn(),
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
  mocked.getSymbolInfo.mockResolvedValue([])
  mocked.getHoldingsSymbolFields.mockResolvedValue({})
  mocked.getPortfolioHoldings.mockResolvedValue({ holdings: [], fx_rates: {} })
  mocked.getPortfolioLots.mockResolvedValue({ lots: [] })
  mocked.getFxRateForDate.mockResolvedValue(null)
  mocked.addHoldingTransaction.mockResolvedValue(makeTransaction({ id: 99 }))
  mocked.addHoldingFromWatchlist.mockResolvedValue({ transaction: makeTransaction({ id: 99, symbol: 'SOL.AX' }), removed_memberships: 1 })
  mocked.updateHoldingsSymbolFields.mockResolvedValue({})
})

const input = (placeholder: string | RegExp) => screen.getByPlaceholderText(placeholder) as HTMLInputElement
const submitButton = () => screen.getByRole('button', { name: 'Record Transaction' })

async function renderManager(props: Partial<Parameters<typeof HoldingsManager>[0]> = {}) {
  const result = render(<HoldingsManager onLoading={() => {}} {...props} />)
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
  it('reports success and resets the form', async () => {
    await renderManager()
    fillPurchase('TST.AX', '10', '5')
    fireEvent.change(input('Notes (optional)'), { target: { value: 'note' } })
    submitForm()
    await waitFor(() => expect(screen.getByText(/Transaction recorded successfully/)).toBeTruthy())
    expect(input(/Symbol \(e\.g\./).value).toBe('')
    expect(input('Quantity').value).toBe('')
    expect(input(/Price per share/).value).toBe('')
    expect(input('Notes (optional)').value).toBe('')
    // Holdings are re-fetched so the tables reflect the new transaction
    expect(mocked.getHoldings.mock.calls.length).toBeGreaterThan(1)
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
