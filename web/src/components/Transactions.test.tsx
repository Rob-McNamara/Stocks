// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, waitFor, cleanup, fireEvent, within } from '@testing-library/react'
import Transactions from './Transactions'

vi.mock('../services/api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../services/api')>()
  return {
    ...actual,
    apiClient: {
      getMeta: vi.fn(),
      getCashAccounts: vi.fn(),
      getTransactionsLedger: vi.fn(),
      getFxRateForDate: vi.fn(),
      updateHoldingTransaction: vi.fn(),
      removeHoldingTransaction: vi.fn(),
      getTaxYears: vi.fn(),
    },
  }
})

import { apiClient } from '../services/api'
const mocked = apiClient as unknown as Record<string, ReturnType<typeof vi.fn>>

const ACCOUNTS = [
  { id: 6, name: 'CBA Invest', currency: 'AUD', balance: 100, interest_rate: null, include_in_portfolio: true, notes: null },
  { id: 2, name: 'IBKR', currency: 'USD', balance: 500, interest_rate: null, include_in_portfolio: true, notes: null },
]

function row(overrides: Record<string, unknown> = {}) {
  return {
    key: String(Math.random()),
    id: 1,
    symbol: 'AAA.AX',
    transaction_type: 'purchase',
    date: '2026-01-01',
    quantity: 10,
    price: 5,
    currency: 'AUD',
    original_price: null,
    fx_rate: null,
    amount: null,
    brokerage: null,
    notes: null,
    cash_account_id: null,
    per_share: false,
    payment_date: null,
    custom_fields: {},
    ...overrides,
  }
}

// Deliberately varied: differing amounts, a null brokerage, a derived dividend
// with no id, and two different settlement accounts.
const LEDGER = [
  row({ key: 'a', id: 1, symbol: 'ZZZ.AX', date: '2026-03-01', quantity: 10, price: 5, brokerage: 9.5, cash_account_id: 6 }),
  row({ key: 'b', id: 2, symbol: 'AAA.AX', date: '2026-01-15', quantity: 2, price: 3, brokerage: 1.25, cash_account_id: 2, notes: 'zebra' }),
  row({ key: 'c', id: 3, symbol: 'MMM.AX', date: '2026-02-10', transaction_type: 'dividend', quantity: null, price: null, amount: 300, brokerage: null, cash_account_id: 6, notes: 'apple', franked_amount: 300, franking_credit: 128.57, payment_date: '2026-03-12', dividend_type: 'Interim' }),
  row({ key: 'd', id: null, symbol: 'DDD.AX', date: '2026-02-20', transaction_type: 'dividend', quantity: null, price: null, amount: 0.5, per_share: true, brokerage: null, cash_account_id: null }),
]

beforeEach(() => {
  cleanup()
  for (const fn of Object.values(mocked)) fn.mockReset?.()
  mocked.getMeta.mockResolvedValue({
    currencies: ['AUD', 'USD'], sectors: [], holdings_custom_fields: [],
    watchlist_custom_fields: [], dashboard_custom_lists: [],
    reserved_holdings_keys: [], reserved_watchlist_keys: [],
  })
  mocked.getCashAccounts.mockResolvedValue(ACCOUNTS)
  mocked.getTransactionsLedger.mockResolvedValue({ rows: LEDGER })
  mocked.getFxRateForDate.mockResolvedValue(null)
})

async function renderScreen() {
  const result = render(<Transactions onLoading={() => {}} />)
  await waitFor(() => expect(screen.getByText('ZZZ.AX')).toBeTruthy())
  return result
}

const header = (name: string) =>
  screen.getAllByRole('columnheader').find((th) => th.textContent!.replace(/[↕↑↓]/g, '').trim() === name)!

/** Text of one column, top to bottom, in render order. */
function column(index: number): string[] {
  const body = document.querySelector('tbody')!
  return [...body.querySelectorAll('tr')].map((tr) => tr.children[index].textContent!.trim())
}

describe('transactions table sorting', () => {
  it('marks exactly the intended columns sortable', async () => {
    await renderScreen()
    const sortable = screen.getAllByRole('columnheader')
      .filter((th) => th.className.includes('sortable-header'))
      .map((th) => th.textContent!.replace(/[↕↑↓]/g, '').trim())
    expect(sortable).toEqual(['Date', 'Symbol', 'Amount', 'Brokerage', 'Cash Account', 'Notes'])
  })

  it('sorts by symbol and reverses on a second click', async () => {
    await renderScreen()
    fireEvent.click(header('Symbol'))
    expect(column(1)).toEqual(['AAA.AX', 'DDD.AX', 'MMM.AX', 'ZZZ.AX'])
    fireEvent.click(header('Symbol'))
    expect(column(1)).toEqual(['ZZZ.AX', 'MMM.AX', 'DDD.AX', 'AAA.AX'])
  })

  it('sorts by date chronologically, not by the displayed text', async () => {
    await renderScreen()
    fireEvent.click(header('Date'))
    expect(column(1)).toEqual(['AAA.AX', 'MMM.AX', 'DDD.AX', 'ZZZ.AX'])
  })

  // Amount shows a dividend's own total but a trade's quantity x price, so the
  // sort has to read the same figure the eye does rather than the raw column.
  it('sorts Amount on the figure actually displayed', async () => {
    await renderScreen()
    fireEvent.click(header('Amount'))
    // DDD 0.50 (per-share dividend), AAA 2x3=6, ZZZ 10x5=50, MMM 300
    expect(column(1)).toEqual(['DDD.AX', 'AAA.AX', 'ZZZ.AX', 'MMM.AX'])
  })

  // A blank is an absence, not a low value; burying real rows under a run of
  // empties helps nobody.
  it('keeps empty cells last in both directions', async () => {
    await renderScreen()
    fireEvent.click(header('Brokerage'))
    expect(column(1).slice(0, 2)).toEqual(['AAA.AX', 'ZZZ.AX'])
    fireEvent.click(header('Brokerage'))
    expect(column(1).slice(0, 2)).toEqual(['ZZZ.AX', 'AAA.AX'])
    // The two rows with no brokerage stay at the bottom either way.
    expect(column(1).slice(2).sort()).toEqual(['DDD.AX', 'MMM.AX'])
  })

  it('sorts by cash account name, with derived rows grouped under "not recorded"', async () => {
    await renderScreen()
    fireEvent.click(header('Cash Account'))
    expect(column(7)).toEqual(['CBA Invest', 'CBA Invest', 'IBKR', 'not recorded'])
  })

  it('sorts by notes', async () => {
    await renderScreen()
    fireEvent.click(header('Notes'))
    expect(column(1).slice(0, 2)).toEqual(['MMM.AX', 'AAA.AX'])
  })
})

describe('tax figures', () => {
  it('opens the tax report from the screen header', async () => {
    mocked.getTaxYears.mockResolvedValue({ default_fy: 2026, years: [] })
    await renderScreen()
    fireEvent.click(screen.getByText('Tax report…'))
    expect(await screen.findByRole('dialog', { name: 'Tax report' })).toBeTruthy()
  })

  /** The form shows what is stored, so what it sends back is the whole truth:
   *  a kept figure, a changed one, and a blank sent as null to clear. */
  it("edits a dividend's franking and withholding from its statement", async () => {
    mocked.updateHoldingTransaction.mockResolvedValue({})
    await renderScreen()
    const dividendRow = screen.getByText('MMM.AX').closest('tr')!
    fireEvent.click(within(dividendRow).getByText('Edit'))

    const dialog = within(document.querySelector('.modal-card') as HTMLElement)
    const field = (label: string) => dialog.getByText(label).parentElement!.querySelector('input') as HTMLInputElement
    expect(field('Franked amount').value).toBe('300')
    expect(field('Franking credit').value).toBe('128.57')
    expect(field('Tax withheld').value).toBe('')
    expect(field('Payment date').value).toBe('2026-03-12')
    expect(field('Type').value).toBe('Interim')
    fireEvent.change(field('Franking credit'), { target: { value: '128.58' } })
    fireEvent.change(field('Type'), { target: { value: '' } })
    fireEvent.click(screen.getByText('Save'))

    await waitFor(() => expect(mocked.updateHoldingTransaction).toHaveBeenCalled())
    const payload = mocked.updateHoldingTransaction.mock.calls[0][1]
    expect(payload.franked_amount).toBe(300)
    expect(payload.franking_credit).toBe(128.58)
    expect(payload.withholding_amount).toBeNull()
    expect(payload.payment_date).toBe('2026-03-12')
    expect(payload.dividend_type).toBeNull()
  })
})
