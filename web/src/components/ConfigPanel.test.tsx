// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, waitFor, cleanup, fireEvent } from '@testing-library/react'
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
