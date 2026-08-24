// @vitest-environment jsdom
import { describe, it, expect, beforeEach } from 'vitest'
import { render, screen, within, cleanup } from '@testing-library/react'
import PortfolioHistoryChart from './PortfolioHistoryChart'
import type { PortfolioHistoryPoint } from '../services/api'

const SERIES: PortfolioHistoryPoint[] = [
  { date: '2026-01-01', stocks: 0, cash: 1000, total: 1000, flow: 1000 },
  { date: '2026-01-02', stocks: 800, cash: 200, total: 1000, flow: 0 },
  { date: '2026-01-03', stocks: 900, cash: 200, total: 1100, flow: 0 },
  { date: '2026-01-04', stocks: 900, cash: 1200, total: 2100, flow: 1000 },
]

beforeEach(cleanup)

describe('PortfolioHistoryChart', () => {
  it('renders both series with a legend, so identity is never colour alone', () => {
    const { container } = render(<PortfolioHistoryChart series={SERIES} />)
    const legend = container.querySelector('.chart-legend')!
    expect(within(legend as HTMLElement).getByText('Shares')).toBeTruthy()
    expect(within(legend as HTMLElement).getByText('Cash')).toBeTruthy()
    // Two stacked fills plus the total outline
    expect(container.querySelectorAll('path').length).toBeGreaterThanOrEqual(3)
  })

  it('marks only the days money moved', () => {
    const { container } = render(<PortfolioHistoryChart series={SERIES} />)
    const markers = [...container.querySelectorAll('circle')]
    // Two contribution days in the fixture; no hover dots without a hover
    expect(markers).toHaveLength(2)
    expect(markers.every((m) => m.querySelector('title')?.textContent?.includes('Money in'))).toBe(true)
  })

  /**
   * A value chart truncated at its minimum turns a small wobble into a cliff,
   * so the y-axis has to start at zero.
   */
  it('scales from zero rather than the lowest value', () => {
    const { container } = render(<PortfolioHistoryChart series={SERIES} />)
    const labels = [...container.querySelectorAll('text')]
      .map((t) => t.textContent!)
      .filter((t) => t.startsWith('$'))
    expect(labels).toContain('$0')
    // Top gridline is the series maximum
    expect(labels.some((l) => l.replace(/[$,]/g, '') === '2100')).toBe(true)
  })

  it('is labelled for assistive technology', () => {
    render(<PortfolioHistoryChart series={SERIES} />)
    expect(screen.getByRole('img', { name: /portfolio value over time/i })).toBeTruthy()
  })

  it('explains itself when there is nothing to plot', () => {
    const { container } = render(<PortfolioHistoryChart series={[]} />)
    expect(screen.getByText(/No portfolio history yet/)).toBeTruthy()
    expect(container.querySelector('svg')).toBeNull()
  })

  it('survives a single point without collapsing the scale', () => {
    const { container } = render(
      <PortfolioHistoryChart series={[{ date: '2026-01-01', stocks: 10, cash: 0, total: 10, flow: 10 }]} />,
    )
    const paths = [...container.querySelectorAll('path')].map((p) => p.getAttribute('d') ?? '')
    expect(paths.every((d) => !d.includes('NaN'))).toBe(true)
  })
})
