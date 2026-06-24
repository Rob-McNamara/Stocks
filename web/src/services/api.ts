const API_BASE_URL = 'http://localhost:3001/api'

interface WatchlistSymbol {
  id: number
  symbol: string
  list_name: string
  added_at: string
  notes: string | null
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

interface HoldingTransactionPayload {
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
}

export const apiClient = {
  async getWatchlistLists(): Promise<string[]> {
    const response = await fetch(`${API_BASE_URL}/watchlist/lists`)
    if (!response.ok) throw new Error('Failed to fetch watchlist lists')
    return response.json()
  },

  async getWatchlistSymbols(list?: string): Promise<WatchlistSymbol[]> {
    const url = list ? `${API_BASE_URL}/watchlist?list=${encodeURIComponent(list)}` : `${API_BASE_URL}/watchlist`
    const response = await fetch(url)
    if (!response.ok) throw new Error('Failed to fetch watchlist symbols')
    return response.json()
  },

  async addWatchlistSymbol(symbol: string, listName?: string, notes?: string, customFields?: Record<string, string>): Promise<WatchlistSymbol> {
    const response = await fetch(`${API_BASE_URL}/watchlist`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ symbol: symbol.toUpperCase(), list_name: listName ?? 'Default', notes: notes || null, custom_fields: customFields ?? {} })
    })
    if (!response.ok) {
      const error = await response.text()
      throw new Error(error || 'Failed to add symbol')
    }
    return response.json()
  },

  async updateWatchlistSymbol(id: number, notes: string | null, customFields?: Record<string, string>): Promise<WatchlistSymbol> {
    const response = await fetch(`${API_BASE_URL}/watchlist/${id}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ notes, custom_fields: customFields ?? {} })
    })
    if (!response.ok) throw new Error('Failed to update symbol')
    return response.json()
  },

  async removeWatchlistSymbol(id: number): Promise<void> {
    const response = await fetch(`${API_BASE_URL}/watchlist/${id}`, {
      method: 'DELETE'
    })
    if (!response.ok) throw new Error('Failed to remove symbol')
  },

  async getWatchlistPrices(list?: string): Promise<CurrentPrice[]> {
    const url = list ? `${API_BASE_URL}/watchlist/prices?list=${encodeURIComponent(list)}` : `${API_BASE_URL}/watchlist/prices`
    const response = await fetch(url)
    if (!response.ok) throw new Error('Failed to fetch current prices')
    return response.json()
  },

  async getWatchlistCachedPrices(list?: string): Promise<CurrentPrice[]> {
    const url = list ? `${API_BASE_URL}/watchlist/cached-prices?list=${encodeURIComponent(list)}` : `${API_BASE_URL}/watchlist/cached-prices`
    const response = await fetch(url)
    if (!response.ok) throw new Error('Failed to fetch cached prices')
    return response.json()
  },

  async getCurrentPrices(symbols: string[]): Promise<CurrentPrice[]> {
    if (symbols.length === 0) return []
    const response = await fetch(
      `${API_BASE_URL}/current-prices?symbols=${symbols.map(encodeURIComponent).join(',')}`
    )
    if (!response.ok) throw new Error('Failed to fetch current prices')
    return response.json()
  },

  async getCachedPrices(symbols: string[]): Promise<CurrentPrice[]> {
    if (symbols.length === 0) return []
    const response = await fetch(
      `${API_BASE_URL}/cached-prices?symbols=${symbols.map(encodeURIComponent).join(',')}`
    )
    if (!response.ok) throw new Error('Failed to fetch cached prices')
    return response.json()
  },

  async getHoldings(): Promise<HoldingTransaction[]> {
    const response = await fetch(`${API_BASE_URL}/holdings`)
    if (response.status === 404) {
      return []
    }
    if (!response.ok) {
      const message = await response.text()
      throw new Error(message || 'Failed to fetch holdings')
    }
    return response.json()
  },

  async getHoldingsSymbolFields(): Promise<Record<string, Record<string, string>>> {
    const response = await fetch(`${API_BASE_URL}/holdings/symbol-fields`)
    if (!response.ok) throw new Error('Failed to fetch holdings symbol fields')
    return response.json()
  },

  async updateHoldingsSymbolFields(symbol: string, notes: string | null, customFields: Record<string, string>): Promise<void> {
    const response = await fetch(`${API_BASE_URL}/holdings/symbol-fields/${encodeURIComponent(symbol)}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ notes, custom_fields: customFields }),
    })
    if (!response.ok) throw new Error('Failed to update holdings symbol fields')
  },

  async addHoldingTransaction(payload: HoldingTransactionPayload): Promise<HoldingTransaction> {
    const response = await fetch(`${API_BASE_URL}/holdings`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    })

    if (!response.ok) {
      const message = await response.text()
      throw new Error(message || 'Failed to record holding transaction')
    }

    return response.json()
  },

  async updateHoldingTransaction(id: number, payload: HoldingTransactionPayload): Promise<HoldingTransaction> {
    const response = await fetch(`${API_BASE_URL}/holdings/${id}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    })

    if (!response.ok) {
      const message = await response.text()
      throw new Error(message || 'Failed to update holding transaction')
    }

    return response.json()
  },

  async removeHoldingTransaction(id: number): Promise<void> {
    const response = await fetch(`${API_BASE_URL}/holdings/${id}`, {
      method: 'DELETE',
    })
    if (!response.ok) throw new Error('Failed to delete holding transaction')
  },

  async getPriceHistory(symbol: string, days = 300): Promise<PriceHistoryPoint[]> {
    const response = await fetch(`${API_BASE_URL}/price-history?symbol=${encodeURIComponent(symbol)}&days=${days}`)
    if (!response.ok) {
      const message = await response.text()
      throw new Error(message || 'Failed to fetch price history')
    }
    return response.json()
  },

  async getConfig(): Promise<Record<string, string>> {
    const response = await fetch(`${API_BASE_URL}/config`)
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

    const response = await fetch(`${API_BASE_URL}/events?${params.toString()}`)
    if (!response.ok) {
      const message = await response.text()
      throw new Error(message || 'Failed to fetch event log')
    }
    return response.json()
  },

  async getSymbolInfo(): Promise<{ symbol: string; instrument_type: string | null; long_name: string | null; currency: string | null }[]> {
    const response = await fetch(`${API_BASE_URL}/symbol-info`)
    if (!response.ok) throw new Error('Failed to fetch symbol info')
    return response.json()
  },

  async getFxRates(): Promise<{ USDAUD: number | null }> {
    const response = await fetch(`${API_BASE_URL}/fx-rates`)
    if (!response.ok) return { USDAUD: null }
    return response.json()
  },

  async getFxRateForDate(currency: string, date: string): Promise<{ rate: number; date: string } | null> {
    const response = await fetch(`${API_BASE_URL}/fx-rate?currency=${encodeURIComponent(currency)}&date=${encodeURIComponent(date)}`)
    if (!response.ok) return null
    return response.json()
  },

  async getDividends(): Promise<{ symbol: string; ex_date: string; payment_date: string | null; amount: number }[]> {
    const response = await fetch(`${API_BASE_URL}/dividends`)
    if (!response.ok) throw new Error('Failed to fetch dividends')
    return response.json()
  },

  async refreshDividends(): Promise<{ updated: number; errors: string[] }> {
    const response = await fetch(`${API_BASE_URL}/dividends/refresh`, { method: 'POST' })
    if (!response.ok) {
      const message = await response.text()
      throw new Error(message || 'Failed to refresh dividends')
    }
    return response.json()
  },

  async refreshSoldDividends(): Promise<{ updated: number; errors: string[] }> {
    const response = await fetch(`${API_BASE_URL}/dividends/refresh-sold`, { method: 'POST' })
    if (!response.ok) {
      const message = await response.text()
      throw new Error(message || 'Failed to refresh sold dividends')
    }
    return response.json()
  },

  async updateConfig(key: string, value: string): Promise<void> {
    const response = await fetch(`${API_BASE_URL}/config`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ key, value })
    })
    if (!response.ok) throw new Error('Failed to update config')
  }
}
