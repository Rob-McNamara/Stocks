// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, waitFor, cleanup, fireEvent } from '@testing-library/react'
import CorporateActions from './CorporateActions'

vi.mock('../services/api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../services/api')>()
  return {
    ...actual,
    apiClient: {
      getCorporateActions: vi.fn(),
      previewCorporateAction: vi.fn(),
      addCorporateAction: vi.fn(),
      deleteCorporateAction: vi.fn(),
    },
  }
})

import { apiClient } from '../services/api'
const mocked = apiClient as unknown as Record<string, ReturnType<typeof vi.fn>>

const ACTION = {
  id: 1, kind: 'merger', date: '2025-09-23', from_symbol: 'BKW.AX', to_symbol: 'SOL.AX',
  from_quantity: 80, to_quantity: 66, rollover: true, market_value: null, notes: null, created_at: '2026-10-25',
}
const PLAN = {
  held: 80, cost: 2041.63, currency: 'AUD',
  parcels: [{ purchase_id: 2, acquired: '2025-01-07', given_up: 80, received: 66, cost: 2041.63, native_cost: 2041.63 }],
}

beforeEach(() => {
  cleanup()
  for (const fn of Object.values(mocked)) fn.mockReset()
  mocked.getCorporateActions.mockResolvedValue([])
  vi.spyOn(window, 'confirm').mockReturnValue(true)
})

const field = (label: string, index = 0) =>
  screen.getAllByText(label)[index].closest('label')!.querySelector('input') as HTMLInputElement

function fillMerger() {
  fireEvent.change(field('Implementation date'), { target: { value: '2025-09-23' } })
  fireEvent.change(field('Shares given up — symbol'), { target: { value: 'bkw.ax' } })
  fireEvent.change(field('How many', 0), { target: { value: '80' } })
  fireEvent.change(field('Shares received — symbol'), { target: { value: 'SOL.AX' } })
  fireEvent.change(field('How many', 1), { target: { value: '66' } })
}

describe('corporate actions', () => {
  it('checks a merger before recording it, showing the parcels that move', async () => {
    const onChanged = vi.fn()
    mocked.previewCorporateAction.mockResolvedValue({ action: ACTION, plan: PLAN })
    mocked.addCorporateAction.mockResolvedValue({ action: ACTION, plan: PLAN })
    render(<CorporateActions symbols={['BKW.AX', 'SOL.AX']} onClose={() => {}} onChanged={onChanged} />)
    await screen.findByText('None yet.')

    fillMerger()
    expect((screen.getByText('Record merger') as HTMLButtonElement).disabled).toBe(true)
    fireEvent.click(screen.getByText('Check'))
    await screen.findByText(/These parcels move to SOL.AX/)
    expect(mocked.previewCorporateAction).toHaveBeenCalledWith(expect.objectContaining({
      from_symbol: 'BKW.AX', to_symbol: 'SOL.AX', from_quantity: 80, to_quantity: 66, rollover: true, market_value: null,
    }))
    expect(screen.getByText('2025-01-07')).toBeTruthy()
    expect(screen.getAllByText('2,041.63').length).toBeGreaterThan(0)

    // Changing anything means checking again.
    fireEvent.change(field('How many', 1), { target: { value: '65' } })
    expect((screen.getByText('Record merger') as HTMLButtonElement).disabled).toBe(true)
    fireEvent.change(field('How many', 1), { target: { value: '66' } })

    mocked.getCorporateActions.mockResolvedValue([ACTION])
    fireEvent.click(screen.getByText('Record merger'))
    await waitFor(() => expect(onChanged).toHaveBeenCalled())
    expect(mocked.addCorporateAction).toHaveBeenCalled()
    expect(await screen.findByText(/Recorded: 80 BKW.AX became 66 SOL.AX/)).toBeTruthy()
  })

  it('asks for the market value when roll-over is not chosen', async () => {
    render(<CorporateActions symbols={[]} onClose={() => {}} onChanged={() => {}} />)
    await screen.findByText('None yet.')
    fillMerger()
    fireEvent.click(screen.getByText('Scrip-for-scrip roll-over chosen'))
    expect((screen.getByText('Check') as HTMLButtonElement).disabled).toBe(true)
    fireEvent.change(field('Market value of the shares received (AUD) — what the old shares were disposed of for'), { target: { value: '2640' } })
    expect((screen.getByText('Check') as HTMLButtonElement).disabled).toBe(false)
  })

  it('deletes an action, undoing it', async () => {
    const onChanged = vi.fn()
    mocked.getCorporateActions.mockResolvedValueOnce([ACTION]).mockResolvedValue([])
    mocked.deleteCorporateAction.mockResolvedValue(undefined)
    render(<CorporateActions symbols={[]} onClose={() => {}} onChanged={onChanged} />)
    fireEvent.click(await screen.findByText('Delete'))
    await waitFor(() => expect(mocked.deleteCorporateAction).toHaveBeenCalledWith(1))
    await waitFor(() => expect(onChanged).toHaveBeenCalled())
  })
})
