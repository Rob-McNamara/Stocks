// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, waitFor, cleanup, fireEvent, within } from '@testing-library/react'
import TaxReport from './TaxReport'

vi.mock('../services/api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../services/api')>()
  return {
    ...actual,
    downloadTaxReportCsv: vi.fn(),
    apiClient: {
      getTaxYears: vi.fn(),
      getTaxReport: vi.fn(),
      recordTaxExport: vi.fn(),
      updateSymbolTaxSettings: vi.fn(),
      saveTaxStatement: vi.fn(),
      deleteTaxStatement: vi.fn(),
      updateAccountTaxShare: vi.fn(),
    },
  }
})

import { apiClient, downloadTaxReportCsv } from '../services/api'
const mocked = apiClient as unknown as Record<string, ReturnType<typeof vi.fn>>
const mockedDownload = downloadTaxReportCsv as unknown as ReturnType<typeof vi.fn>

const YEARS = {
  default_fy: 2026,
  ownership: [{ kind: 'symbol', name: 'HERS.AX', share_pct: 0 }, { kind: 'account', name: 'Joint Saver', share_pct: 50 }],
  years: [
    { fy: 2027, label: '2026–27', start: '2026-07-01', end: '2027-06-30', in_progress: true },
    { fy: 2026, label: '2025–26', start: '2025-07-01', end: '2026-06-30', in_progress: false },
  ],
}

const REPORT = {
  fy: 2026, label: '2025–26', start: '2025-07-01', end: '2026-06-30', entity: 'individual',
  generated_at: '2026-10-11 13:00', in_progress: false, detail: 'full', last_exported_at: null,
  losses_carried_in: 0, losses_carried_in_source: 'computed',
  included: ['checks', 'cgt'],
  checks: [{ severity: 'warn', section: 'trusts', message: 'No 2025–26 annual tax statement entered for VAS.AX.' }],
  sections: [
    {
      key: 'cgt', title: 'Capital gains', notes: ['Parcels are matched first in, first out.'], text: 'CAPITAL GAINS\n  18A Net capital gain 502.86',
      summary: [{ item: '18A', label: 'Net capital gain', amount: 502.86, value: null, note: null }],
      tables: [{
        title: 'Parcels sold',
        columns: [
          { label: 'Symbol', kind: 'text' }, { label: 'Currency', kind: 'text' },
          { label: 'Proceeds (trade ccy)', kind: 'money' }, { label: 'Sale FX rate', kind: 'rate' },
          { label: 'Proceeds (AUD)', kind: 'money' }, { label: 'Gain (AUD)', kind: 'money' },
        ],
        rows: [['SPCX', 'USD', 1179.9, 1.4245, 1680.76, -52.46]],
      }],
    },
  ],
  symbols: [{ symbol: 'GMG.AX', name: 'Goodman Group', currency: 'AUD', income_kind: 'company', income_kind_set: false, franking_pct: null, withholding_pct: null, share_pct: null }],
  statements: [{ symbol: 'VAS.AX', name: null, cash_aud: 130.12, distributions: 4, statement: null }],
  accounts: [{ id: 7, name: 'Joint Saver', currency: 'AUD', share_pct: null, interest_aud: 100 }],
  ownership: ['Left out, as not yours (share 0%): HERS.AX.'],
  worksheet: [{
    title: 'Dividends: franking to copy from each statement',
    columns: [{ label: 'Symbol', kind: 'text' }, { label: 'Gross paid (AUD)', kind: 'money' }, { label: 'Franked amount', kind: 'entry' }],
    rows: [['CBA.AX', 2.6, null]],
  }],
  text: 'TAX REPORT — 2025–26 income year',
}

const writeText = vi.fn()

beforeEach(() => {
  cleanup()
  for (const fn of Object.values(mocked)) fn.mockReset?.()
  mockedDownload.mockReset()
  writeText.mockReset().mockResolvedValue(undefined)
  Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true })
  mocked.getTaxYears.mockResolvedValue(YEARS)
  mocked.getTaxReport.mockResolvedValue(REPORT)
  mocked.recordTaxExport.mockResolvedValue(undefined)
  mocked.updateSymbolTaxSettings.mockResolvedValue(undefined)
  mocked.saveTaxStatement.mockResolvedValue({})
})

async function open() {
  render(<TaxReport onClose={() => {}} />)
  await waitFor(() => expect((screen.getByRole('combobox') as HTMLSelectElement).value).toBe('2026'))
}

const checkbox = (label: string) =>
  screen.getByText(label).closest('label')!.querySelector('input') as HTMLInputElement

describe('tax report options', () => {
  it('defaults to the last finished year with every section ticked', async () => {
    await open()
    for (const label of ['Capital gains', 'Australian dividends', 'Foreign income', 'Interest', 'Trust and ETF distributions', 'Possible deductions', 'Needs attention']) {
      expect(checkbox(label).checked).toBe(true)
    }
    // The planning view only makes sense for the year still running.
    expect(checkbox('This year so far').disabled).toBe(true)
  })

  it('says up front what is not all yours', async () => {
    await open()
    expect(screen.getByText(/HERS\.AX left out, Joint Saver 50%/)).toBeTruthy()
  })

  it('offers the planning view for the year in progress', async () => {
    await open()
    fireEvent.change(screen.getByRole('combobox'), { target: { value: '2027' } })
    expect(checkbox('This year so far').disabled).toBe(false)
    expect(checkbox('This year so far').checked).toBe(true)
  })

  it('asks only for what is ticked, with any losses entered', async () => {
    await open()
    fireEvent.click(checkbox('Interest'))
    fireEvent.click(screen.getByText('Totals only'))
    fireEvent.change(screen.getByPlaceholderText("Worked out from the app's records"), { target: { value: '1200' } })
    fireEvent.click(screen.getByText('Generate'))
    await waitFor(() => expect(mocked.getTaxReport).toHaveBeenCalled())
    expect(mocked.getTaxReport).toHaveBeenCalledWith({
      fy: 2026,
      sections: ['checks', 'cgt', 'dividends', 'foreign', 'trusts', 'deductions'],
      detail: 'summary',
      lossesCarriedIn: 1200,
    })
  })
})

describe('tax report results', () => {
  const dialog = () => within(screen.getByRole('dialog', { name: 'Tax report' }))

  async function generated() {
    await open()
    fireEvent.click(dialog().getByText('Generate'))
    await dialog().findByText('Needs attention')
  }

  it('shows the checks, the figures and both currencies', async () => {
    await generated()
    expect(dialog().getByText('No 2025–26 annual tax statement entered for VAS.AX.')).toBeTruthy()
    expect(dialog().getByText('502.86')).toBeTruthy()
    expect(dialog().getByText('1,179.90')).toBeTruthy()
    expect(dialog().getByText('1.4245')).toBeTruthy()
    expect(dialog().getByText('1,680.76')).toBeTruthy()
    expect(dialog().getByText('-52.46').className).toContain('negative')
  })

  it('copies the whole report and records the export', async () => {
    await generated()
    fireEvent.click(dialog().getByText('Copy all'))
    await waitFor(() => expect(writeText).toHaveBeenCalledWith(REPORT.text))
    await waitFor(() => expect(mocked.recordTaxExport).toHaveBeenCalledWith(2026))
    expect(await dialog().findByText(/Copied the whole report/)).toBeTruthy()
  })

  it('copies one section on its own', async () => {
    await generated()
    fireEvent.click(dialog().getByText('Copy'))
    await waitFor(() => expect(writeText).toHaveBeenCalledWith(REPORT.sections[0].text))
  })

  it('downloads the CSV for the same options', async () => {
    await generated()
    fireEvent.click(dialog().getByText('Download CSV'))
    await waitFor(() => expect(mockedDownload).toHaveBeenCalled())
    expect(mockedDownload.mock.calls[0][0]).toMatchObject({ fy: 2026, detail: 'full' })
    await waitFor(() => expect(mocked.recordTaxExport).toHaveBeenCalledWith(2026))
  })

  it('confirms a worked-out income type and regenerates', async () => {
    await generated()
    fireEvent.change(dialog().getByLabelText('GMG.AX income type'), { target: { value: 'trust' } })
    fireEvent.click(within(dialog().getByLabelText('GMG.AX income type').closest('tr')!).getByText('Save'))
    await waitFor(() => expect(mocked.updateSymbolTaxSettings).toHaveBeenCalledWith('GMG.AX', {
      income_kind: 'trust', franking_pct: null, dividend_withholding_pct: null, tax_share_pct: null,
    }))
    await waitFor(() => expect(mocked.getTaxReport).toHaveBeenCalledTimes(2))
  })

  it('saves an annual tax statement from its Part A figures', async () => {
    await generated()
    fireEvent.click(dialog().getByText('Enter'))
    const field = dialog().getByText('Non-primary production income').closest('label')!.querySelector('input')!
    fireEvent.change(field, { target: { value: '55.10' } })
    fireEvent.click(dialog().getByText('Save 2025–26 statement'))
    await waitFor(() => expect(mocked.saveTaxStatement).toHaveBeenCalled())
    const [symbol, fy, figures] = mocked.saveTaxStatement.mock.calls[0]
    expect(symbol).toBe('VAS.AX')
    expect(fy).toBe(2026)
    expect(figures.non_primary_income).toBe(55.1)
    expect(figures.franking_credits).toBeNull()
  })

  it('leaves a holding out by setting its share to 0', async () => {
    await generated()
    fireEvent.change(dialog().getByLabelText('GMG.AX your share %'), { target: { value: '0' } })
    fireEvent.click(within(dialog().getByLabelText('GMG.AX your share %').closest('tr')!).getByText('Save'))
    await waitFor(() => expect(mocked.updateSymbolTaxSettings).toHaveBeenCalledWith('GMG.AX', expect.objectContaining({ tax_share_pct: 0 })))
  })

  it("sets the user's share of a joint cash account", async () => {
    mocked.updateAccountTaxShare.mockResolvedValue(undefined)
    await generated()
    fireEvent.change(dialog().getByLabelText('Joint Saver your share %'), { target: { value: '50' } })
    fireEvent.click(dialog().getAllByText('Save').at(-1)!)
    await waitFor(() => expect(mocked.updateAccountTaxShare).toHaveBeenCalledWith(7, 50))
    await waitFor(() => expect(mocked.getTaxReport).toHaveBeenCalledTimes(2))
  })

  it('shows what is left out at the top of the report', async () => {
    await generated()
    expect(dialog().getByText('Left out, as not yours (share 0%): HERS.AX.')).toBeTruthy()
  })

  /** The print layout sits outside the app root, every table open, and ends
   *  with the worksheet's blank boxes. */
  it('prints the report with the worksheet of fields to fill in', async () => {
    const print = vi.spyOn(window, 'print').mockImplementation(() => {})
    await generated()
    const layout = document.body.querySelector(':scope > .tax-print')!
    expect(layout).toBeTruthy()
    expect(layout.textContent).toContain('Tax report — 2025–26 income year')
    expect(layout.textContent).toContain('To complete by hand')
    expect(layout.textContent).toContain('Dividends: franking to copy from each statement')
    expect(layout.querySelectorAll('td.tax-entry')).toHaveLength(1)
    expect(layout.textContent).toContain('1,680.76')
    fireEvent.click(dialog().getByText('Print'))
    expect(print).toHaveBeenCalled()
    print.mockRestore()
  })
})
