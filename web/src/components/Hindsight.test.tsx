// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, waitFor, cleanup, within } from '@testing-library/react'
import Hindsight from './Hindsight'
import type { HindsightRow, HindsightPoint } from '../services/api'

vi.mock('../services/api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../services/api')>()
  return { ...actual, apiClient: { getHindsight: vi.fn() } }
})

import { apiClient } from '../services/api'
const getHindsight = apiClient.getHindsight as ReturnType<typeof vi.fn>

const point = (over: Partial<HindsightPoint> = {}): HindsightPoint => ({
  value: 11,
  date: '2025-01-10',
  status: 'ok',
  delta: 100,
  pct: 10,
  ...over,
})

const row = (over: Partial<HindsightRow> = {}): HindsightRow => ({
  symbol: 'TST.AX',
  currency: 'AUD',
  sale_date: '2025-01-03',
  quantity: 100,
  sale_price: 10,
  purchase_price: 8,
  realised_pct: 25,
  delisted_on: null,
  points: {
    week1: point(),
    week6: point(),
    month3: point(),
    peak: point(),
    low: point(),
    current: point(),
  },
  ...over,
})

beforeEach(() => {
  cleanup()
  getHindsight.mockReset().mockResolvedValue([row()])
})

async function renderScreen() {
  const result = render(<Hindsight onLoading={() => {}} />)
  await waitFor(() => expect(screen.queryByText(/Loading hindsight/)).toBeNull())
  return result
}

const bodyRow = (symbol: string) => screen.getByText(symbol).closest('tr') as HTMLElement

describe('Hindsight', () => {
  it('shows each sale with its trade result and the six windows', async () => {
    await renderScreen()
    for (const label of ['+1 week', '+6 weeks', '+3 months', 'Peak since', 'Low since', 'Now']) {
      expect(screen.getByText(label)).toBeTruthy()
    }
    const r = bodyRow('TST.AX')
    expect(r.textContent).toContain('2025-01-03')
    expect(r.textContent).toContain('+25.0%') // the trade itself
  })

  /**
   * The screen's whole reading depends on this inversion: a price that rose
   * after the sale is money left on the table, so it must not read green just
   * because the number went up.
   */
  it('colours a post-sale rise as regret and a fall as vindication', async () => {
    getHindsight.mockResolvedValue([
      row({
        symbol: 'ROSE.AX',
        points: { ...row().points, current: point({ pct: 20, delta: 200 }) },
      }),
      row({
        symbol: 'FELL.AX',
        points: { ...row().points, current: point({ pct: -20, delta: -200, value: 8 }) },
      }),
    ])
    await renderScreen()

    const rose = within(bodyRow('ROSE.AX')).getByText('+20.0%')
    const fell = within(bodyRow('FELL.AX')).getByText('−20.0%')
    expect(rose.style.color).toBe('rgb(244, 67, 54)') // red — should have held
    expect(fell.style.color).toBe('rgb(76, 175, 80)') // green — right to sell
  })

  /**
   * "Not yet", "never" and "missing" are three different answers. Collapsing
   * them into one blank cell would send the reader looking for a backfill that
   * cannot exist, or waiting for a window that already failed.
   */
  it('distinguishes the three empty states', async () => {
    getHindsight.mockResolvedValue([
      row({
        points: {
          ...row().points,
          week1: point({ value: null, delta: null, pct: null, status: 'pending' }),
          week6: point({ value: null, delta: null, pct: null, status: 'delisted' }),
          month3: point({ value: null, delta: null, pct: null, status: 'no_data' }),
        },
      }),
    ])
    await renderScreen()

    const r = bodyRow('TST.AX')
    expect(within(r).getByText('to come')).toBeTruthy()
    expect(within(r).getByText('delisted')).toBeTruthy()
    expect(within(r).getByText('no data')).toBeTruthy()
  })

  /**
   * The rows are native by design, so one total over a mixed list would be a
   * number with no meaning. AUD and USD are reported apart.
   */
  it('totals each currency separately and never adds them together', async () => {
    getHindsight.mockResolvedValue([
      row({ symbol: 'AUS.AX', currency: 'AUD', points: { ...row().points, current: point({ delta: -100 }) } }),
      row({ symbol: 'USO', currency: 'USD', points: { ...row().points, current: point({ delta: 250 }) } }),
    ])
    await renderScreen()

    // Scoped to the header: a non-AUD row also carries its code as a tag.
    const header = within(screen.getByText('Hindsight').closest('.card-header') as HTMLElement)
    expect(header.getByText(/^AUD/).textContent).toContain('100.00')
    expect(header.getByText(/^USD/).textContent).toContain('250.00')
    // 250 − 100 = 150 would be the meaningless combined figure.
    expect(screen.queryByText(/150\.00/)).toBeNull()
  })

  it('marks a delisted symbol and labels its final price', async () => {
    getHindsight.mockResolvedValue([
      row({
        delisted_on: '2025-08-01',
        points: { ...row().points, current: point({ status: 'delisted', value: 3.91 }) },
      }),
    ])
    await renderScreen()

    const r = bodyRow('TST.AX')
    expect(within(r).getAllByText('delisted').length).toBeGreaterThan(0)
    expect(within(r).getByText('final')).toBeTruthy()
  })

  it('shows a dash where the matching purchase was never recorded', async () => {
    getHindsight.mockResolvedValue([row({ purchase_price: null, realised_pct: null })])
    await renderScreen()
    expect(within(bodyRow('TST.AX')).getAllByText('—').length).toBe(2)
  })

  /**
   * Regression. Twice this session a new field crashed the screen because the
   * *running* backend did not send it yet. A row that arrives without its
   * points must render as empty cells, not a white screen.
   */
  it('survives a row from a backend that sends no points at all', async () => {
    getHindsight.mockResolvedValue([
      { symbol: 'OLD.AX', currency: 'AUD', sale_date: '2025-01-03', quantity: 10, sale_price: 10 } as HindsightRow,
    ])
    await renderScreen()

    const r = bodyRow('OLD.AX')
    expect(within(r).getAllByText('no data')).toHaveLength(6)
  })

  it('says so when there is nothing to second-guess', async () => {
    getHindsight.mockResolvedValue([])
    await renderScreen()
    expect(screen.getByText(/nothing to second-guess/)).toBeTruthy()
  })

  it('reports a failed load instead of rendering an empty table', async () => {
    getHindsight.mockRejectedValue(new Error('backend is down'))
    await renderScreen()
    expect(screen.getByText(/backend is down/)).toBeTruthy()
  })
})
