// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, waitFor, cleanup, fireEvent } from '@testing-library/react'
import DividendImport from './DividendImport'

vi.mock('../services/api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../services/api')>()
  return { ...actual, apiClient: { importDividends: vi.fn() } }
})

import { apiClient } from '../services/api'
const importDividends = (apiClient as unknown as { importDividends: ReturnType<typeof vi.fn> }).importDividends

const CSV = 'symbol,ex_date,total_amount\nCBA.AX,2025-08-20,2.60\n'

function plan(overrides: Record<string, unknown> = {}) {
  return {
    applied: false,
    updated: 0,
    created: 0,
    counts: { update: 2, create: 0, unchanged: 0, error: 1 },
    rows: [
      { line: 2, symbol: 'CBA.AX', ex_date: '2025-08-20', status: 'update', transaction_id: 5, matched_date: null,
        changes: [{ field: 'Payment date', from: null, to: '2025-09-29' }], warnings: [], error: null, selected: true, applied: false },
      { line: 3, symbol: 'VTS.AX', ex_date: '2025-09-29', status: 'update', transaction_id: 6, matched_date: '2025-09-30',
        changes: [{ field: 'Ex-date', from: '2025-09-30', to: '2025-09-29' }], warnings: ['Total differs from what is recorded'], error: null, selected: false, applied: false },
      { line: 4, symbol: 'BKW.AX', ex_date: '2025-08-21', status: 'error', transaction_id: null, matched_date: null,
        changes: [], warnings: [], error: 'No BKW.AX holding in the app', selected: false, applied: false },
    ],
    ...overrides,
  }
}

beforeEach(() => {
  cleanup()
  importDividends.mockReset()
})

async function chooseFile() {
  const onImported = vi.fn()
  render(<DividendImport onClose={() => {}} onImported={onImported} />)
  const input = screen.getByLabelText('Dividend CSV file') as HTMLInputElement
  const file = new File([CSV], 'dividends.csv', { type: 'text/csv' })
  fireEvent.change(input, { target: { files: [file] } })
  await screen.findByText('No BKW.AX holding in the app')
  return onImported
}

describe('dividend import', () => {
  it('previews the file without writing, ticking only clean rows', async () => {
    importDividends.mockResolvedValue(plan())
    await chooseFile()
    expect(importDividends).toHaveBeenCalledWith(CSV, { shortfall_is_withholding: false })
    expect((screen.getByLabelText('Import line 2') as HTMLInputElement).checked).toBe(true)
    expect((screen.getByLabelText('Import line 3') as HTMLInputElement).checked).toBe(false)
    expect(screen.queryByLabelText('Import line 4')).toBeNull()
    expect(screen.getByText('⚠ Total differs from what is recorded')).toBeTruthy()
    expect(screen.getByText('Apply 1 selected')).toBeTruthy()
  })

  it('previews again when lower totals are read as tax withheld', async () => {
    importDividends.mockResolvedValue(plan())
    await chooseFile()
    fireEvent.click(screen.getByText('Lower totals are tax withheld'))
    await waitFor(() => expect(importDividends).toHaveBeenLastCalledWith(CSV, { shortfall_is_withholding: true }))
  })

  it('applies the ticked lines and reloads the ledger', async () => {
    importDividends.mockResolvedValueOnce(plan())
    const onImported = await chooseFile()
    fireEvent.click(screen.getByLabelText('Import line 3'))
    importDividends.mockResolvedValueOnce(plan({ applied: true, updated: 2 }))
    fireEvent.click(screen.getByText('Apply 2 selected'))
    await waitFor(() => expect(onImported).toHaveBeenCalled())
    const [, , apply] = importDividends.mock.calls[1]
    expect([...apply.lines].sort()).toEqual([2, 3])
    expect(await screen.findByText(/Updated 2 dividends and recorded 0 new/)).toBeTruthy()
  })

  it('says why a file cannot be read', async () => {
    importDividends.mockRejectedValue(new Error('The header has no ex_date column'))
    render(<DividendImport onClose={() => {}} onImported={() => {}} />)
    const input = screen.getByLabelText('Dividend CSV file') as HTMLInputElement
    fireEvent.change(input, { target: { files: [new File(['x'], 'bad.csv')] } })
    expect(await screen.findByText(/The header has no ex_date column/)).toBeTruthy()
  })
})
