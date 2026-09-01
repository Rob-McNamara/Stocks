// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, waitFor, cleanup, fireEvent, within } from '@testing-library/react'
import ConfigPanel from './ConfigPanel'

// The panel is a thin editor over `app_config`; every call is mocked so the
// tests can decide what the server says.
vi.mock('../services/api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../services/api')>()
  return {
    ...actual,
    apiClient: {
      getConfig: vi.fn(),
      updateConfig: vi.fn(),
      getSymbolInfo: vi.fn(),
      getMeta: vi.fn(),
    },
  }
})

import { apiClient } from '../services/api'
import { invalidateAppConfig } from '../utils/appConfig'

const getConfig = apiClient.getConfig as ReturnType<typeof vi.fn>
const updateConfig = apiClient.updateConfig as ReturnType<typeof vi.fn>
const getMeta = apiClient.getMeta as ReturnType<typeof vi.fn>
const getSymbolInfo = apiClient.getSymbolInfo as ReturnType<typeof vi.fn>

beforeEach(() => {
  cleanup()
  invalidateAppConfig()
  getConfig.mockReset().mockResolvedValue({
    watchlist_custom_fields: JSON.stringify([{ key: 'target', label: 'Target', type: 'number' }]),
    holdings_custom_fields: JSON.stringify([{ key: 'thesis', label: 'Thesis', type: 'text', actions: ['purchase'] }]),
  })
  updateConfig.mockReset().mockResolvedValue(undefined)
  getMeta.mockReset().mockResolvedValue({ sectors: [], currencies: [] })
  getSymbolInfo.mockReset().mockResolvedValue([])
})

async function renderPanel() {
  const result = render(<ConfigPanel onLoading={() => {}} />)
  await waitFor(() => expect(screen.queryByText(/Loading configuration/)).toBeNull())
  return result
}

const removeButtonNear = (label: string) => {
  const row = screen.getByText(label).closest('tr')!
  return Array.from(row.querySelectorAll('button')).find((b) => b.textContent === 'Remove')!
}

/**
 * The API validates these config keys and can refuse a write. Every save has to
 * report a refusal and put the panel back — otherwise the change looks saved,
 * is not, and disappears on the next reload with nothing to explain it.
 */
describe('a refused config save', () => {
  it('reports the server’s reason and restores the watchlist field', async () => {
    await renderPanel()
    expect(screen.getByText('Target')).toBeTruthy()

    updateConfig.mockRejectedValueOnce(new Error('Invalid watchlist_custom_fields: field 1: label is required'))
    fireEvent.click(removeButtonNear('Target'))

    await waitFor(() => expect(screen.getByText(/label is required/)).toBeTruthy())
    // Rolled back: the row the server refused to remove is still listed.
    expect(screen.getByText('Target')).toBeTruthy()
  })

  it('reports it and restores the holdings field', async () => {
    await renderPanel()
    expect(screen.getByText('Thesis')).toBeTruthy()

    updateConfig.mockRejectedValueOnce(new Error('Invalid holdings_custom_fields: field 1: key is required'))
    fireEvent.click(removeButtonNear('Thesis'))

    await waitFor(() => expect(screen.getByText(/key is required/)).toBeTruthy())
    expect(screen.getByText('Thesis')).toBeTruthy()
  })

  // A save that succeeds must not leave a stale error banner behind it.
  it('clears the banner once a later save succeeds', async () => {
    await renderPanel()
    updateConfig.mockRejectedValueOnce(new Error('Invalid watchlist_custom_fields: nope'))
    fireEvent.click(removeButtonNear('Target'))
    await waitFor(() => expect(screen.getByText(/nope/)).toBeTruthy())

    fireEvent.click(removeButtonNear('Target'))
    await waitFor(() => expect(screen.queryByText(/nope/)).toBeNull())
  })
})

describe('a successful config save', () => {
  it('removes the field and sends the remaining list', async () => {
    const onConfigChanged = vi.fn()
    render(<ConfigPanel onLoading={() => {}} onConfigChanged={onConfigChanged} />)
    await waitFor(() => expect(screen.queryByText(/Loading configuration/)).toBeNull())

    fireEvent.click(removeButtonNear('Target'))

    await waitFor(() => expect(onConfigChanged).toHaveBeenCalled())
    expect(updateConfig).toHaveBeenCalledWith('watchlist_custom_fields', '[]')
    expect(screen.queryByText('Target')).toBeNull()
  })
})

/**
 * A delisted symbol is marked here so nothing fetches it again. The mark is a
 * config key like any other, which means the server can refuse it and the panel
 * has to show what is actually stored rather than what was typed.
 */
describe('delisted symbols', () => {
  const withDead = (extra: Record<string, string> = {}) =>
    getConfig.mockResolvedValue({
      watchlist_custom_fields: '[]',
      holdings_custom_fields: '[]',
      'dead_symbol_JLG.AX': '2025-08-01',
      ...extra,
    })

  // A symbol with a final price is listed in the Manual Prices card too, so
  // every lookup has to be scoped to this card or it matches both.
  const card = () => screen.getByText('Delisted Symbols').closest('.manager-card') as HTMLElement
  const rowFor = (symbol: string) => within(card()).getByText(symbol).closest('tr')!
  const buttonIn = (row: HTMLElement, label: string) =>
    Array.from(row.querySelectorAll('button')).find((b) => b.textContent === label)!

  it('marks a symbol delisted with its last traded date', async () => {
    await renderPanel()
    fireEvent.change(screen.getByPlaceholderText('e.g. JLG.AX'), { target: { value: 'CCLD.AX' } })
    // The date input has no placeholder, so reach it through its label column.
    const form = screen.getByPlaceholderText('e.g. JLG.AX').closest('.config-edit')!
    fireEvent.change(form.querySelector('input[type="date"]')!, { target: { value: '2026-06-17' } })
    fireEvent.click(screen.getByText('Mark Delisted'))

    await waitFor(() =>
      expect(updateConfig).toHaveBeenCalledWith('dead_symbol_CCLD.AX', '2026-06-17'),
    )
  })

  it('shows the companion manual price, and says so when there is none', async () => {
    withDead({ 'manual_price_JLG.AX': '3.91' })
    await renderPanel()
    expect(rowFor('JLG.AX').textContent).toContain('$3.91')

    cleanup()
    invalidateAppConfig()
    withDead()
    await renderPanel()
    // Without a final price the symbol falls back to its last stored bar, and
    // JLG.AX has none — the panel has to say that rather than show a blank.
    expect(rowFor('JLG.AX').textContent).toContain('not set')
  })

  it('clears the mark by writing an empty value', async () => {
    withDead()
    await renderPanel()
    fireEvent.click(buttonIn(rowFor('JLG.AX'), 'Remove'))

    await waitFor(() => expect(updateConfig).toHaveBeenCalledWith('dead_symbol_JLG.AX', ''))
    expect(within(card()).queryByText('JLG.AX')).toBeNull()
  })

  // The server validates the date shape. A refusal that left the row on screen
  // would show a mark that does not exist until the next reload.
  it('reports a refused date and keeps the stored value', async () => {
    withDead()
    await renderPanel()
    updateConfig.mockRejectedValueOnce(
      new Error("Invalid dead_symbol_JLG.AX: '01/08/2025' is not a date in YYYY-MM-DD form"),
    )
    fireEvent.click(buttonIn(rowFor('JLG.AX'), 'Remove'))

    await waitFor(() => expect(screen.getByText(/is not a date/)).toBeTruthy())
    expect(within(card()).getByText('JLG.AX')).toBeTruthy()
  })

  // Clearing writes an empty value rather than deleting the row, so a blank
  // must not read back as a delisted symbol on the next load.
  it('does not list a symbol whose mark has been cleared', async () => {
    getConfig.mockResolvedValue({
      watchlist_custom_fields: '[]',
      holdings_custom_fields: '[]',
      'dead_symbol_CCLD.AX': '',
    })
    await renderPanel()
    expect(within(card()).queryByText('CCLD.AX')).toBeNull()
  })
})
