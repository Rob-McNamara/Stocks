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
      getCashAccounts: vi.fn(),
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
  mocked.getCashAccounts.mockResolvedValue([])
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
const triggerButton = () => screen.getByRole('button', { name: /Record Holding/ })

// The record form lives in a dialog, so every test that reaches for a field has
// to open it first. A prefill opens the dialog on its own; clicking again is a
// no-op, so this stays a single path for both cases.
async function renderManager(props: Partial<Parameters<typeof HoldingsManager>[0]> = {}) {
  const result = render(<HoldingsManager onLoading={() => {}} {...props} />)
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
    render(<HoldingsManager onLoading={() => {}} />)
    await waitFor(() => expect((triggerButton() as HTMLButtonElement).disabled).toBe(false))
    expect(screen.queryByPlaceholderText(/Symbol \(e\.g\./)).toBeNull()
    fireEvent.click(triggerButton())
    expect(screen.getByPlaceholderText(/Symbol \(e\.g\./)).toBeTruthy()
  })

  // "Move to Holdings" would look broken if it filled a form nobody could see.
  it('opens itself for a watchlist prefill with no click', async () => {
    render(<HoldingsManager onLoading={() => {}} prefill={{ symbol: 'SOL.AX', price: 44.02 }} />)
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
