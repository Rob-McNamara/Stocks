// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'

// The client is exercised against a stubbed global fetch — no server. Each
// test that changes VITE_API_TOKEN re-imports the module because the token
// is read once at module load.

const fetchMock = vi.fn()

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })
}

async function loadClient() {
  vi.resetModules()
  return (await import('./api')).apiClient
}

beforeEach(() => {
  fetchMock.mockReset()
  vi.stubGlobal('fetch', fetchMock)
})

afterEach(() => {
  vi.unstubAllGlobals()
  vi.unstubAllEnvs()
})

describe('error envelope handling', () => {
  it('surfaces the v1 envelope message from a failed write', async () => {
    const apiClient = await loadClient()
    fetchMock.mockResolvedValue(
      jsonResponse({ error: { code: 'bad_request', message: 'Quantity must be greater than zero' } }, 400),
    )
    await expect(
      apiClient.addHoldingTransaction({ symbol: 'TST.AX', transaction_type: 'purchase', date: '2026-01-05' }),
    ).rejects.toThrow('Quantity must be greater than zero')
  })

  it('falls back to the raw body for non-JSON errors', async () => {
    const apiClient = await loadClient()
    fetchMock.mockResolvedValue(new Response('gateway exploded', { status: 502 }))
    await expect(apiClient.getPortfolioHoldings()).rejects.toThrow('gateway exploded')
  })

  it('checkHealth returns false instead of throwing when the server is unreachable', async () => {
    const apiClient = await loadClient()
    fetchMock.mockRejectedValue(new TypeError('fetch failed'))
    expect(await apiClient.checkHealth()).toBe(false)
  })
})

describe('bearer token attachment', () => {
  it('attaches Authorization when VITE_API_TOKEN is set', async () => {
    vi.stubEnv('VITE_API_TOKEN', 'secret-token')
    const apiClient = await loadClient()
    fetchMock.mockResolvedValue(jsonResponse({ status: 'ok' }))
    await apiClient.checkHealth()
    const [, init] = fetchMock.mock.calls[0]
    expect((init?.headers as Record<string, string>).Authorization).toBe('Bearer secret-token')
  })

  it('sends no Authorization header when the token is unset', async () => {
    const apiClient = await loadClient()
    fetchMock.mockResolvedValue(jsonResponse({ status: 'ok' }))
    await apiClient.checkHealth()
    const [, init] = fetchMock.mock.calls[0]
    const headers = (init?.headers ?? {}) as Record<string, string>
    expect(headers.Authorization).toBeUndefined()
  })
})

describe('core getter shapes', () => {
  // Fixture JSON mirrors the server's wire shapes; these tests catch
  // client/server drift until the OpenAPI schemas are typed.

  it('getPortfolioOverview returns the overview payload as-is', async () => {
    const apiClient = await loadClient()
    const payload = {
      totals: { stock_count: 1, total_value: 100, total_pl: 5, holdings_pl: 5, sold_pl: 0 },
      breakdowns: {
        equities: { count: 1, value: 100, dividends: 0, pl: 5, cost: 95 },
        etfs: { count: 0, value: 0, dividends: 0, pl: 0, cost: 0 },
        holdings: { count: 1, value: 100, dividends: 0, pl: 5, cost: 95 },
        sold: { count: 0, value: 0, dividends: 0, pl: 0, cost: 0 },
      },
      sectors: [],
      worst_holdings: [],
      best_watchlist: [],
      custom_lists: [],
    }
    fetchMock.mockResolvedValue(jsonResponse(payload))
    const overview = await apiClient.getPortfolioOverview()
    expect(fetchMock.mock.calls[0][0]).toContain('/portfolio/overview')
    expect(overview.totals.total_value).toBe(100)
    expect(overview.custom_lists).toEqual([])
  })

  it('getPortfolioHoldings returns holdings with the stop-loss fields', async () => {
    const apiClient = await loadClient()
    fetchMock.mockResolvedValue(
      jsonResponse({
        holdings: [
          {
            symbol: 'TRL.AX', long_name: null, instrument_type: 'EQUITY', is_etf: false,
            is_international: false, currency: 'AUD', sector: null, notes: null, fields: {},
            shares: 50, invested: 1000, avg_cost: 20, native_avg_cost: 20, current_price: 28,
            native_current_price: 28, price_source: 'cache', price_date: '2026-07-10',
            change: null, change_percent: null, volume: null, current_value: 1400,
            dividends: 0, pl: 400, pl_pct: 40, sma150: null,
            stop_loss: 27.0, is_trailing_sell: true,
          },
        ],
        fx_rates: {},
      }),
    )
    const { holdings } = await apiClient.getPortfolioHoldings()
    expect(holdings[0].stop_loss).toBe(27.0)
    expect(holdings[0].is_trailing_sell).toBe(true)
  })

  it('getWatchlistEnriched passes the list filter and returns items', async () => {
    const apiClient = await loadClient()
    fetchMock.mockResolvedValue(jsonResponse({ items: [], prices_updated_at: '2026-07-11T00:00:00Z' }))
    const result = await apiClient.getWatchlistEnriched('Growth')
    expect(fetchMock.mock.calls[0][0]).toContain('/watchlist/enriched?list=Growth')
    expect(result.prices_updated_at).toBe('2026-07-11T00:00:00Z')
  })
})
