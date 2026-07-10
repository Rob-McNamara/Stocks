const API_BASE_URL = import.meta.env.VITE_API_URL ?? 'http://localhost:3001/api/v1'
const API_TOKEN: string | undefined = import.meta.env.VITE_API_TOKEN

/** fetch wrapper that attaches the bearer token when VITE_API_TOKEN is set. */
function apiFetch(input: string, init: RequestInit = {}): Promise<Response> {
  if (API_TOKEN) {
    init.headers = { ...(init.headers ?? {}), Authorization: `Bearer ${API_TOKEN}` }
  }
  return fetch(input, init)
}

/**
 * Extract a human-readable message from a v1 error response. The API wraps
 * errors as {"error": {"code", "message"}}; fall back to the raw body for
 * anything else.
 */
async function apiErrorMessage(response: Response): Promise<string> {
  const text = await response.text()
  try {
    const parsed = JSON.parse(text)
    if (parsed?.error?.message) return parsed.error.message
  } catch {
    // not JSON — use the raw body
  }
  return text
}

interface WatchlistSymbol {
  id: number
  symbol: string
  list_name: string
  added_at: string
  notes: string | null
  breakthrough_price: number | null
  stop_loss_price: number | null
  custom_fields: Record<string, string>
}

interface AppConfig {
  key: string
  value: string
}

interface CurrentPrice {
  symbol: string
  price: number | null
  change: number | null
  change_percent: number | null
  volume: number | null
  last_updated: string
  price_date?: string | null
  error?: string
}

interface PriceHistoryPoint {
  date: string
  close: number | null
  volume: number | null
}

interface EventLogEntry {
  id: number
  timestamp: string
  level: string
  source: string
  event_type: string
  symbol?: string | null
  details?: string | null
}

interface HoldingTransaction {
  id: number
  symbol: string
  transaction_type: 'purchase' | 'sale' | 'dividend'
  date: string
  quantity: number | null
  price: number | null
  amount: number | null
  brokerage: number | null
  notes: string | null
  created_at: string
  dividends_total: number
  currency: string
  original_price: number | null
  fx_rate: number | null
  custom_fields: Record<string, string>
}

export interface BreakdownAgg {
  count: number
  value: number
  dividends: number
  pl: number
  cost: number
}

export interface WorstHoldingEntry {
  symbol: string
  price: number
  sma150: number
  pct_diff: number
}

export interface BestWatchlistEntry {
  symbol: string
  price: number
  sma50: number
  sma50_trend: 'up' | 'down' | null
  days_since_50sma: number
  volume_pct_50sma: number | null
}

export interface CustomListEntry {
  symbol: string
  price: number
  field_value: number
  diff: number
  pct_diff: number
  currency: string | null
}

export interface CustomListResult {
  key: string
  label: string
  source: string
  /** Where the entry symbols live: 'holdings' or 'watchlist' — drives navigation */
  field_source: string
  operator: string
  field_label: string
  entries: CustomListEntry[]
}

export interface PortfolioOverview {
  totals: {
    stock_count: number
    total_value: number
    total_pl: number
    holdings_pl: number
    sold_pl: number
  }
  breakdowns: {
    equities: BreakdownAgg
    etfs: BreakdownAgg
    holdings: BreakdownAgg
    sold: BreakdownAgg
  }
  sectors: Array<BreakdownAgg & { name: string }>
  worst_holdings: WorstHoldingEntry[]
  best_watchlist: BestWatchlistEntry[]
  custom_lists: CustomListResult[]
}

export interface WatchlistIndicators {
  sma50: number | null
  sma150: number | null
  sma50_trend: 'up' | 'down' | null
  sma150_trend: 'up' | 'down' | null
  days_since_50sma: number | null
  volume_pct_50sma: number | null
  days_since_150sma: number | null
  volume_pct_150sma: number | null
  volume_change_pct: number | null
}

export interface EnrichedWatchlistItem {
  id: number
  symbol: string
  list_name: string
  added_at: string
  notes: string | null
  breakthrough_price: number | null
  stop_loss_price: number | null
  custom_fields: Record<string, string>
  instrument_type: string | null
  long_name: string | null
  currency: string | null
  price: number | null
  change: number | null
  change_percent: number | null
  volume: number | null
  price_date: string | null
  last_updated: string | null
  indicators: WatchlistIndicators | null
}

export interface PortfolioHolding {
  symbol: string
  long_name: string | null
  instrument_type: string | null
  is_etf: boolean
  is_international: boolean
  currency: string
  sector: string | null
  notes: string | null
  fields: Record<string, string>
  shares: number
  invested: number
  avg_cost: number | null
  native_avg_cost: number | null
  current_price: number | null
  native_current_price: number | null
  price_source: 'cache' | 'manual' | 'none'
  price_date: string | null
  change: number | null
  change_percent: number | null
  volume: number | null
  current_value: number
  dividends: number
  pl: number
  pl_pct: number | null
  sma150: number | null
}

export interface RiskRow {
  symbol: string
  currency: string
  current_price: number | null
  purchase_price: number | null
  pl_pct: number | null
  stop_loss: number | null
  is_trailing_sell: boolean
  stop_loss_pct: number | null
  stop_loss_dollar: number | null
  sma50: number | null
  sma150: number | null
  high30d: number | null
  total_invested: number
}

export interface RiskTotals {
  total_invested: number
  total_sl_dollar: number
  total_sl_pct: number | null
}

export interface PortfolioLot {
  transaction_id: number
  symbol: string
  date: string
  remaining: number
  current_value: number | null
  unrealised_pl: number | null
}

export interface SoldEntry {
  symbol: string
  date: string
  quantity: number
  avg_purchase_price: number
  sale_price: number
  brokerage: number
  dividends: number
  days_held: number
  realised_pl: number
}

export interface LedgerRow {
  key: string
  id: number | null
  symbol: string
  transaction_type: 'purchase' | 'sale' | 'dividend'
  date: string
  quantity: number | null
  price: number | null
  currency: string
  original_price: number | null
  fx_rate: number | null
  amount: number | null
  brokerage: number | null
  notes: string | null
  /** true for fetched dividend events (per-share amounts); false for manual rows (totals) */
  per_share: boolean
  payment_date: string | null
  custom_fields: Record<string, string>
}

export interface SyncState {
  holdings: string | null
  watchlist: string | null
  dividends: string | null
  symbol_info: string | null
  config: string | null
  watchlist_prices_updated_at: string | null
  holdings_prices_updated_at: string | null
  last_full_refresh_at: string | null
  server_time: string
}

export interface AppMeta {
  sectors: string[]
  currencies: string[]
  holdings_custom_fields: { key: string; label: string; type: string; actions: string[] }[]
  watchlist_custom_fields: { key: string; label: string; type: string }[]
  dashboard_custom_lists: unknown[]
  reserved_holdings_keys: string[]
  reserved_watchlist_keys: string[]
}

export interface HoldingTransactionPayload {
  symbol: string
  transaction_type: 'purchase' | 'sale' | 'dividend'
  date: string
  quantity?: number
  price?: number
  amount?: number
  brokerage?: number
  notes?: string
  currency?: string
  original_price?: number
  fx_rate?: number
  custom_fields?: Record<string, string>
  /** Acknowledge an over-sell warning (server responds 409 otherwise) */
  confirm?: boolean
}

export const apiClient = {
  async checkHealth(): Promise<boolean> {
    try {
      const response = await apiFetch(`${API_BASE_URL}/health`)
      return response.ok
    } catch {
      return false
    }
  },

  async getMeta(): Promise<AppMeta> {
    const response = await apiFetch(`${API_BASE_URL}/meta`)
    if (!response.ok) throw new Error('Failed to fetch app metadata')
    return response.json()
  },

  /** Per-domain last-modified stamps — poll this instead of refetching payloads. */
  async getSyncState(): Promise<SyncState> {
    const response = await apiFetch(`${API_BASE_URL}/sync-state`)
    if (!response.ok) throw new Error('Failed to fetch sync state')
    return response.json()
  },

  async getPortfolioOverview(): Promise<PortfolioOverview> {
    const response = await apiFetch(`${API_BASE_URL}/portfolio/overview`)
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to fetch portfolio overview')
    }
    return response.json()
  },

  async getPortfolioHoldings(): Promise<{ holdings: PortfolioHolding[]; fx_rates: Record<string, number | null> }> {
    const response = await apiFetch(`${API_BASE_URL}/portfolio/holdings`)
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to fetch portfolio holdings')
    }
    return response.json()
  },

  async getPortfolioLots(): Promise<{ lots: PortfolioLot[] }> {
    const response = await apiFetch(`${API_BASE_URL}/portfolio/lots`)
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to fetch portfolio lots')
    }
    return response.json()
  },

  async getPortfolioRisk(): Promise<{ rows: RiskRow[]; totals: RiskTotals }> {
    const response = await apiFetch(`${API_BASE_URL}/portfolio/risk`)
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to fetch portfolio risk analysis')
    }
    return response.json()
  },

  async getPortfolioSold(): Promise<{ entries: SoldEntry[]; total_realised_pl: number; total_cost: number }> {
    const response = await apiFetch(`${API_BASE_URL}/portfolio/sold`)
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to fetch sold stocks')
    }
    return response.json()
  },

  async getWatchlistLists(): Promise<string[]> {
    const response = await apiFetch(`${API_BASE_URL}/watchlist/lists`)
    if (!response.ok) throw new Error('Failed to fetch watchlist lists')
    return response.json()
  },

  /** Watchlist rows with prices and server-computed indicators in one call. */
  async getWatchlistEnriched(list?: string): Promise<{ items: EnrichedWatchlistItem[]; prices_updated_at: string | null }> {
    const url = list ? `${API_BASE_URL}/watchlist/enriched?list=${encodeURIComponent(list)}` : `${API_BASE_URL}/watchlist/enriched`
    const response = await apiFetch(url)
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to fetch enriched watchlist')
    }
    return response.json()
  },

  /** Set a symbol's list memberships, notes and fields in one transactional call. */
  async updateWatchlistSymbolLists(symbol: string, payload: { lists: string[]; notes: string | null; breakthrough_price: number | null; stop_loss_price: number | null; custom_fields?: Record<string, string> }): Promise<WatchlistSymbol[]> {
    const response = await apiFetch(`${API_BASE_URL}/watchlist/symbol/${encodeURIComponent(symbol)}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    })
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to update watchlist symbol')
    }
    return response.json()
  },

  /** Record a holding and remove the symbol from all watchlists atomically. */
  async addHoldingFromWatchlist(payload: HoldingTransactionPayload): Promise<{ transaction: HoldingTransaction; removed_memberships: number; warning?: string }> {
    const response = await apiFetch(`${API_BASE_URL}/holdings/from-watchlist`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    })
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to move stock to holdings')
    }
    return response.json()
  },

  /** Unified transaction + dividend-event ledger, merged and deduped server-side. */
  async getTransactionsLedger(): Promise<{ rows: LedgerRow[] }> {
    const response = await apiFetch(`${API_BASE_URL}/transactions/ledger`)
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to fetch transactions ledger')
    }
    return response.json()
  },

  /** Server-debounced full data refresh (watchlist + holdings prices, dividends). */
  async refreshAll(force = false): Promise<{ skipped: boolean; watchlist_prices?: number; holdings_prices?: number; dividends_updated?: number; errors?: string[] }> {
    const response = await apiFetch(`${API_BASE_URL}/refresh${force ? '?force=true' : ''}`, { method: 'POST' })
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to refresh data')
    }
    return response.json()
  },

  async getWatchlistSymbols(list?: string): Promise<WatchlistSymbol[]> {
    const url = list ? `${API_BASE_URL}/watchlist?list=${encodeURIComponent(list)}` : `${API_BASE_URL}/watchlist`
    const response = await apiFetch(url)
    if (!response.ok) throw new Error('Failed to fetch watchlist symbols')
    return response.json()
  },

  async addWatchlistSymbol(symbol: string, listName?: string, notes?: string, opts?: { breakthroughPrice?: number | null; stopLossPrice?: number | null; customFields?: Record<string, string> }): Promise<WatchlistSymbol> {
    const response = await apiFetch(`${API_BASE_URL}/watchlist`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ symbol: symbol.toUpperCase(), list_name: listName ?? 'Default', notes: notes || null, breakthrough_price: opts?.breakthroughPrice ?? null, stop_loss_price: opts?.stopLossPrice ?? null, custom_fields: opts?.customFields ?? {} })
    })
    if (!response.ok) {
      const error = await apiErrorMessage(response)
      throw new Error(error || 'Failed to add symbol')
    }
    return response.json()
  },

  async updateWatchlistSymbol(id: number, notes: string | null, opts?: { breakthroughPrice?: number | null; stopLossPrice?: number | null; customFields?: Record<string, string> }): Promise<WatchlistSymbol> {
    const response = await apiFetch(`${API_BASE_URL}/watchlist/${id}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ notes, breakthrough_price: opts?.breakthroughPrice ?? null, stop_loss_price: opts?.stopLossPrice ?? null, custom_fields: opts?.customFields ?? {} })
    })
    if (!response.ok) throw new Error('Failed to update symbol')
    return response.json()
  },

  async removeWatchlistSymbol(id: number): Promise<void> {
    const response = await apiFetch(`${API_BASE_URL}/watchlist/${id}`, {
      method: 'DELETE'
    })
    if (!response.ok) throw new Error('Failed to remove symbol')
  },

  async renameWatchlistList(oldName: string, newName: string): Promise<void> {
    const response = await apiFetch(`${API_BASE_URL}/watchlist/lists/rename`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ old_name: oldName, new_name: newName }),
    })
    if (!response.ok) throw new Error('Failed to rename list')
  },

  async getWatchlistPrices(list?: string): Promise<CurrentPrice[]> {
    const url = list ? `${API_BASE_URL}/watchlist/prices?list=${encodeURIComponent(list)}` : `${API_BASE_URL}/watchlist/prices`
    const response = await apiFetch(url)
    if (!response.ok) throw new Error('Failed to fetch current prices')
    return response.json()
  },

  async getWatchlistCachedPrices(list?: string): Promise<CurrentPrice[]> {
    const url = list ? `${API_BASE_URL}/watchlist/cached-prices?list=${encodeURIComponent(list)}` : `${API_BASE_URL}/watchlist/cached-prices`
    const response = await apiFetch(url)
    if (!response.ok) throw new Error('Failed to fetch cached prices')
    return response.json()
  },

  async getCurrentPrices(symbols: string[]): Promise<CurrentPrice[]> {
    if (symbols.length === 0) return []
    const response = await apiFetch(
      `${API_BASE_URL}/current-prices?symbols=${symbols.map(encodeURIComponent).join(',')}`
    )
    if (!response.ok) throw new Error('Failed to fetch current prices')
    return response.json()
  },

  async getCachedPrices(symbols: string[]): Promise<CurrentPrice[]> {
    if (symbols.length === 0) return []
    const response = await apiFetch(
      `${API_BASE_URL}/cached-prices?symbols=${symbols.map(encodeURIComponent).join(',')}`
    )
    if (!response.ok) throw new Error('Failed to fetch cached prices')
    return response.json()
  },

  async getHoldings(): Promise<HoldingTransaction[]> {
    const response = await apiFetch(`${API_BASE_URL}/holdings`)
    if (response.status === 404) {
      return []
    }
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to fetch holdings')
    }
    return response.json()
  },

  async getHoldingsSymbolFields(): Promise<Record<string, Record<string, string>>> {
    const response = await apiFetch(`${API_BASE_URL}/holdings/symbol-fields`)
    if (!response.ok) throw new Error('Failed to fetch holdings symbol fields')
    return response.json()
  },

  async updateHoldingsSymbolFields(symbol: string, notes: string | null, customFields: Record<string, string>): Promise<void> {
    const response = await apiFetch(`${API_BASE_URL}/holdings/symbol-fields/${encodeURIComponent(symbol)}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ notes, custom_fields: customFields }),
    })
    if (!response.ok) throw new Error('Failed to update holdings symbol fields')
  },

  async addHoldingTransaction(payload: HoldingTransactionPayload): Promise<HoldingTransaction> {
    const response = await apiFetch(`${API_BASE_URL}/holdings`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    })

    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to record holding transaction')
    }

    return response.json()
  },

  async updateHoldingTransaction(id: number, payload: HoldingTransactionPayload): Promise<HoldingTransaction> {
    const response = await apiFetch(`${API_BASE_URL}/holdings/${id}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    })

    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to update holding transaction')
    }

    return response.json()
  },

  async renameHoldingSymbol(oldSymbol: string, newSymbol: string): Promise<{ renamed: number }> {
    const response = await apiFetch(`${API_BASE_URL}/holdings/rename-symbol/${encodeURIComponent(oldSymbol)}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ new_symbol: newSymbol }),
    })
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to rename holding symbol')
    }
    return response.json()
  },

  async removeHoldingTransaction(id: number): Promise<void> {
    const response = await apiFetch(`${API_BASE_URL}/holdings/${id}`, {
      method: 'DELETE',
    })
    if (!response.ok) throw new Error('Failed to delete holding transaction')
  },

  async getPriceHistory(symbol: string, days = 300): Promise<PriceHistoryPoint[]> {
    const response = await apiFetch(`${API_BASE_URL}/price-history?symbol=${encodeURIComponent(symbol)}&days=${days}`)
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to fetch price history')
    }
    return response.json()
  },

  async getConfig(): Promise<Record<string, string>> {
    const response = await apiFetch(`${API_BASE_URL}/config`)
    if (!response.ok) throw new Error('Failed to fetch config')
    const items: AppConfig[] = await response.json()
    
    // Convert array to object
    return items.reduce((acc, item) => {
      acc[item.key] = item.value
      return acc
    }, {} as Record<string, string>)
  },

  async getEventLog(opts?: { page?: number; size?: number; level?: string; source?: string; event_type?: string; symbol?: string }): Promise<{ items: EventLogEntry[]; total: number }> {
    const params = new URLSearchParams()
    if (opts?.page) params.set('page', String(opts.page))
    if (opts?.size) params.set('size', String(opts.size))
    if (opts?.level) params.set('level', opts.level)
    if (opts?.source) params.set('source', opts.source)
    if (opts?.event_type) params.set('event_type', opts.event_type)
    if (opts?.symbol) params.set('symbol', opts.symbol)

    const response = await apiFetch(`${API_BASE_URL}/events?${params.toString()}`)
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to fetch event log')
    }
    return response.json()
  },

  async getSymbolInfo(): Promise<{ symbol: string; instrument_type: string | null; long_name: string | null; currency: string | null }[]> {
    const response = await apiFetch(`${API_BASE_URL}/symbol-info`)
    if (!response.ok) throw new Error('Failed to fetch symbol info')
    return response.json()
  },

  /** Returns AUD per 1 unit of each requested currency, keyed by ISO code. */
  async getFxRates(currencies: string[]): Promise<Record<string, number | null>> {
    const wanted = currencies.map((c) => c.toUpperCase()).filter((c) => c && c !== 'AUD')
    if (wanted.length === 0) return {}
    const response = await apiFetch(`${API_BASE_URL}/fx-rates?currencies=${wanted.map(encodeURIComponent).join(',')}`)
    if (!response.ok) return {}
    return response.json()
  },

  async getFxRateForDate(currency: string, date: string): Promise<{ rate: number; date: string } | null> {
    const response = await apiFetch(`${API_BASE_URL}/fx-rate?currency=${encodeURIComponent(currency)}&date=${encodeURIComponent(date)}`)
    if (!response.ok) return null
    return response.json()
  },

  async getDividends(): Promise<{ symbol: string; ex_date: string; payment_date: string | null; amount: number }[]> {
    const response = await apiFetch(`${API_BASE_URL}/dividends`)
    if (!response.ok) throw new Error('Failed to fetch dividends')
    return response.json()
  },

  async refreshDividends(): Promise<{ updated: number; errors: string[] }> {
    const response = await apiFetch(`${API_BASE_URL}/dividends/refresh`, { method: 'POST' })
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to refresh dividends')
    }
    return response.json()
  },

  async refreshSoldDividends(): Promise<{ updated: number; errors: string[] }> {
    const response = await apiFetch(`${API_BASE_URL}/dividends/refresh-sold`, { method: 'POST' })
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to refresh sold dividends')
    }
    return response.json()
  },

  async updateConfig(key: string, value: string): Promise<void> {
    const response = await apiFetch(`${API_BASE_URL}/config`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ key, value })
    })
    if (!response.ok) throw new Error('Failed to update config')
  },

  async analyzeStock(symbol: string, messages: { role: string; content: string }[]): Promise<{ role: string; content: string }> {
    const response = await apiFetch(`${API_BASE_URL}/stock-analysis`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ symbol, messages }),
    })
    if (!response.ok) {
      const error = await apiErrorMessage(response)
      throw new Error(error || 'Failed to analyze stock')
    }
    return response.json()
  },

  async getAnalysisHistory(symbol: string): Promise<{ id: number; role: string; content: string; model_used: string | null; created_at: string }[]> {
    const response = await apiFetch(`${API_BASE_URL}/stock-analysis/history?symbol=${encodeURIComponent(symbol)}`)
    if (!response.ok) throw new Error('Failed to fetch analysis history')
    return response.json()
  },

  async clearAnalysisHistory(symbol: string): Promise<void> {
    const response = await apiFetch(`${API_BASE_URL}/stock-analysis/history?symbol=${encodeURIComponent(symbol)}`, { method: 'DELETE' })
    if (!response.ok) throw new Error('Failed to clear analysis history')
  },
}
