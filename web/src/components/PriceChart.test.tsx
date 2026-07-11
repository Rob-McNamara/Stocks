// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { render, screen, waitFor, cleanup, fireEvent } from '@testing-library/react'
import PriceChart from './PriceChart'

vi.mock('../services/api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../services/api')>()
  return {
    ...actual,
    apiClient: {
      getPriceHistory: vi.fn(),
      getSymbolInfo: vi.fn(),
      getFxRateForDate: vi.fn(),
    },
  }
})

import { apiClient } from '../services/api'
const getPriceHistory = apiClient.getPriceHistory as ReturnType<typeof vi.fn>
const getSymbolInfo = apiClient.getSymbolInfo as ReturnType<typeof vi.fn>

// Chart geometry constants from PriceChart's chartData
const LEFT = 72
const PLOT_WIDTH = 1040 - 72 - 20

function isoDaysAgo(n: number): string {
  const d = new Date()
  d.setDate(d.getDate() - n)
  return d.toISOString().slice(0, 10)
}

/** 40 recent daily bars, oldest first, closes around $10–12. */
const DATES = Array.from({ length: 40 }, (_, i) => isoDaysAgo(39 - i))
const HISTORY = DATES.map((date, i) => ({ date, close: 10 + (i % 5) * 0.5, volume: 1000 + i }))

async function renderChart(props: Partial<Parameters<typeof PriceChart>[0]> = {}) {
  const result = render(<PriceChart symbol="TST.AX" currency="AUD" onLoading={() => {}} {...props} />)
  await waitFor(() => expect(screen.queryByText(/Loading chart/)).toBeNull())
  return result
}

beforeEach(() => {
  cleanup()
  getPriceHistory.mockReset().mockResolvedValue(HISTORY)
  getSymbolInfo.mockReset().mockResolvedValue([])
})

afterEach(() => {
  vi.restoreAllMocks()
})

describe('SMA defaults and toggles', () => {
  it('starts with SMA50 and SMA150 active', async () => {
    await renderChart()
    const active = [20, 50, 100, 150, 200].filter(
      (p) => screen.getByRole('button', { name: `SMA ${p}` }).className.includes('active'),
    )
    expect(active).toEqual([50, 150])
    // Legend mirrors the active set
    expect(screen.getByText('50-day SMA')).toBeTruthy()
    expect(screen.getByText('150-day SMA')).toBeTruthy()
    expect(screen.queryByText('20-day SMA')).toBeNull()
  })

  it('toggles periods on and off via the buttons', async () => {
    await renderChart()
    fireEvent.click(screen.getByRole('button', { name: 'SMA 150' }))
    expect(screen.getByRole('button', { name: 'SMA 150' }).className).not.toContain('active')
    expect(screen.queryByText('150-day SMA')).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: 'SMA 20' }))
    expect(screen.getByText('20-day SMA')).toBeTruthy()
  })
})

describe('purchase dot placement', () => {
  it('lands on the first bar at or after the purchase date', async () => {
    const purchaseIdx = 10
    const { container } = await renderChart({
      purchasePrice: 10,
      purchaseDate: DATES[purchaseIdx],
    })
    const dot = container.querySelector('circle[fill="#4caf50"]')!
    expect(dot).toBeTruthy()
    const expectedX = LEFT + (PLOT_WIDTH * purchaseIdx) / (DATES.length - 1)
    expect(Math.abs(parseFloat(dot.getAttribute('cx')!) - expectedX)).toBeLessThan(0.5)
  })

  it('renders on the axis in orange when the purchase predates the chart range', async () => {
    const { container } = await renderChart({
      purchasePrice: 10,
      purchaseDate: '2020-01-02',
    })
    expect(container.querySelector('circle[fill="#4caf50"]')).toBeNull()
    const axisDot = container.querySelector('circle[fill="#ff9800"]')!
    expect(axisDot).toBeTruthy()
    expect(parseFloat(axisDot.getAttribute('cx')!)).toBe(LEFT)
  })
})

describe('stop-loss marker', () => {
  it('renders the marker dot when a stop loss is set and omits it otherwise', async () => {
    const { container } = await renderChart({
      markerPrice: 9.0,
      markerLabel: 'Stop Loss',
      markerMode: 'stoploss',
    })
    expect(container.querySelector('circle[fill="#e91e63"]')).toBeTruthy()

    cleanup()
    const { container: bare } = await renderChart()
    expect(bare.querySelector('circle[fill="#e91e63"]')).toBeNull()
  })

  it('shows the trailing-sell label in the hover tooltip', async () => {
    // jsdom reports a zero-size bounding box; give the SVG its real size so
    // mouse coordinates map to an index
    vi.spyOn(Element.prototype, 'getBoundingClientRect').mockReturnValue({
      x: 0, y: 0, left: 0, top: 0, right: 1040, bottom: 260, width: 1040, height: 260,
      toJSON: () => ({}),
    } as DOMRect)

    const { container } = await renderChart({
      markerPrice: 9.0,
      markerLabel: 'Trailing Sell',
      markerMode: 'stoploss',
    })
    fireEvent.mouseMove(container.querySelector('svg')!, { clientX: 500, clientY: 100 })
    await waitFor(() => expect(screen.getByText(/Trailing Sell: \$9\.00/)).toBeTruthy())
  })
})
