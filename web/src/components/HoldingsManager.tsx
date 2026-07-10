import { useEffect, useMemo, useRef, useState } from 'react'
import { apiClient, type PortfolioHolding, type PortfolioLot } from '../services/api'
import { getActiveHoldingSymbols, getEarliestRemainingPurchaseDate } from '../utils/holdings'
import { SECTORS } from '../utils/sectors'
import PriceChart from './PriceChart'

// Thin client: FIFO, cost basis, dividends, FX conversion, manual-price and
// instrument-type overrides, and SMA are all computed by the API server
// (GET /api/portfolio/holdings and /api/portfolio/lots). This component only
// renders server data and manages the transaction form.

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

interface HoldingsFieldDef {
  key: string
  label: string
  type: 'text' | 'number' | 'date'
  actions: string[]
}

const SUPPORTED_CURRENCIES = ['AUD', 'USD', 'GBP', 'EUR', 'JPY', 'CAD', 'HKD', 'SGD', 'NZD']

// Symbol-level fields with dedicated inputs — excluded from the user-defined
// custom-field plumbing.
const BUILT_IN_HOLDINGS_KEYS = ['stop_loss', 'trailing_sell_pct', 'trailing_sell_date', 'sector']

interface HoldingsPrefill {
  symbol: string
  price?: number
  notes?: string
  customFields?: Record<string, string>
}

export default function HoldingsManager({ onLoading, onTransactionsChanged, configVersion, prefill, onPrefillConsumed, onPrefillSaved, focusSymbol, onFocusSymbolConsumed }: { onLoading: (loading: boolean) => void; onTransactionsChanged?: () => void; configVersion?: number; prefill?: HoldingsPrefill | null; onPrefillConsumed?: () => void; onPrefillSaved?: (symbol: string) => void; focusSymbol?: string | null; onFocusSymbolConsumed?: () => void }) {
  const [transactions, setTransactions] = useState<HoldingTransaction[]>([])
  /** Server-computed per-symbol summaries from /api/portfolio/holdings */
  const [serverHoldings, setServerHoldings] = useState<PortfolioHolding[]>([])
  /** Server-computed per-purchase remaining/unrealised P/L, keyed by transaction id */
  const [lotMap, setLotMap] = useState<Record<number, PortfolioLot>>({})
  /** Custom field definitions from /api/meta */
  const [holdingsFieldDefs, setHoldingsFieldDefs] = useState<HoldingsFieldDef[]>([])
  // symbolInfo is kept only for auto-detecting the currency of symbols typed
  // into the form (which may not be held yet, so aren't in serverHoldings)
  const [symbolInfo, setSymbolInfo] = useState<Record<string, { instrument_type: string | null; long_name: string | null; currency: string | null }>>({})
  const [symbol, setSymbol] = useState('')
  const [transactionType, setTransactionType] = useState<HoldingTransaction['transaction_type']>('purchase')
  const [date, setDate] = useState(() => new Date().toISOString().slice(0, 10))
  const [quantity, setQuantity] = useState('')
  const [price, setPrice] = useState('')
  const [amount, setAmount] = useState('')
  const [brokerage, setBrokerage] = useState('')
  const [notes, setNotes] = useState('')
  const [currency, setCurrency] = useState('AUD')
  const [fxRate, setFxRate] = useState<number | null>(null)
  const [fxRateDate, setFxRateDate] = useState<string | null>(null)
  const [fxLoading, setFxLoading] = useState(false)
  const [customFieldValues, setCustomFieldValues] = useState<Record<string, string>>({})
  const [holdingsSymbolFields, setHoldingsSymbolFields] = useState<Record<string, Record<string, string>>>({})
  const prefillJustApplied = useRef(false)
  // Symbol that arrived via "Move to Holdings" — the watchlist entry is only
  // removed (via onPrefillSaved) once a transaction for it is actually saved.
  const prefillPendingSymbol = useRef<string | null>(null)
  const [editingSymbolCard, setEditingSymbolCard] = useState<string | null>(null)
  const [editCardSymbol, setEditCardSymbol] = useState('')
  const [stopLossPrice, setStopLossPrice] = useState('')
  const [trailingSellPct, setTrailingSellPct] = useState('')
  const [trailingSellDate, setTrailingSellDate] = useState('')
  const [sector, setSector] = useState('')
  // Server-driven sector list from /api/meta; static list is the offline fallback
  const [sectorOptions, setSectorOptions] = useState<string[]>([...SECTORS])
  const [editCardNotes, setEditCardNotes] = useState('')
  const [editCardFields, setEditCardFields] = useState<Record<string, string>>({})
  const [editingId, setEditingId] = useState<number | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [success, setSuccess] = useState<string | null>(null)

  useEffect(() => {
    loadHoldings()
  }, [configVersion])

  // Pre-fill form when navigating from watchlist
  useEffect(() => {
    if (!prefill) return
    setEditingId(null)
    setSymbol(prefill.symbol)
    setTransactionType('purchase')
    setDate(new Date().toISOString().slice(0, 10))
    setQuantity('')
    if (prefill.price != null) setPrice(prefill.price.toString())
    else setPrice('')
    setAmount('')
    setBrokerage('')
    setNotes(prefill.notes ?? '')
    // Map watchlist custom fields to holdings custom fields by matching key names
    const mappedFields: Record<string, string> = {}
    if (prefill.customFields) {
      const holdingsKeys = new Set([...holdingsFieldDefs.map((d) => d.key), 'stop_loss'])
      for (const [key, value] of Object.entries(prefill.customFields)) {
        if (holdingsKeys.has(key) && value) {
          mappedFields[key] = value
        }
      }
    }
    setCustomFieldValues(mappedFields)
    // Carry the watchlist sector across to the holding
    setSector(prefill.customFields?.['sector'] ?? '')
    // If stop_loss came through prefill, also set it as a symbol-level field
    if (prefill.customFields?.['stop_loss']) {
      const updated = { ...holdingsSymbolFields }
      if (!updated[prefill.symbol]) updated[prefill.symbol] = {}
      updated[prefill.symbol]['stop_loss'] = prefill.customFields['stop_loss']
      setHoldingsSymbolFields(updated)
      apiClient.updateHoldingsSymbolFields(prefill.symbol, null, { stop_loss: prefill.customFields['stop_loss'] }).catch(() => {})
    }
    prefillJustApplied.current = true
    prefillPendingSymbol.current = prefill.symbol.trim().toUpperCase()
    onPrefillConsumed?.()
  }, [prefill])

  // Auto-detect currency from symbolInfo when adding a new transaction
  useEffect(() => {
    if (editingId !== null) return // don't override currency when editing
    const detected = symbolInfo[symbol]?.currency?.toUpperCase()
    if (detected && detected !== 'AUD' && SUPPORTED_CURRENCIES.includes(detected)) {
      setCurrency(detected)
    } else if (detected === 'AUD') {
      setCurrency('AUD')
    }
  }, [symbol, symbolInfo])

  // Pre-populate custom fields from per-symbol values when entering a new transaction
  useEffect(() => {
    if (editingId !== null) return
    if (prefillJustApplied.current) {
      prefillJustApplied.current = false
      return
    }
    const symFields = holdingsSymbolFields[symbol.trim().toUpperCase()]
    if (symFields) {
      // Built-in symbol-level fields (_notes, stop_loss, sector, ...) have
      // dedicated inputs — only user-defined fields go into the custom inputs.
      const custom: Record<string, string> = {}
      Object.entries(symFields).forEach(([k, v]) => {
        if (!BUILT_IN_HOLDINGS_KEYS.includes(k) && k !== '_notes') custom[k] = v
      })
      setCustomFieldValues(custom)
      setSector(symFields['sector'] ?? '')
    } else {
      setCustomFieldValues({})
      setSector('')
    }
  }, [symbol, holdingsSymbolFields, editingId])

  useEffect(() => {
    if (currency === 'AUD') {
      setFxRate(null)
      setFxRateDate(null)
      return
    }
    setFxLoading(true)
    apiClient.getFxRateForDate(currency, date).then((result) => {
      if (result) {
        setFxRate(result.rate)
        setFxRateDate(result.date)
      } else {
        setFxRate(null)
        setFxRateDate(null)
      }
    }).finally(() => setFxLoading(false))
  }, [currency, date])

  const loadPortfolioData = async () => {
    const [ph, pl] = await Promise.all([
      apiClient.getPortfolioHoldings(),
      apiClient.getPortfolioLots(),
    ])
    setServerHoldings(ph.holdings)
    const lots: Record<number, PortfolioLot> = {}
    pl.lots.forEach((l) => { lots[l.transaction_id] = l })
    setLotMap(lots)
    setSelectedChartSymbol((prev) => prev || ph.holdings[0]?.symbol || '')
  }

  const loadHoldings = async () => {
    try {
      setLoading(true)
      setError(null)
      onLoading(true)
      apiClient.getMeta().then((m) => {
        if (m.sectors?.length) setSectorOptions(m.sectors)
        setHoldingsFieldDefs(((m.holdings_custom_fields ?? []) as HoldingsFieldDef[]).filter((d) => !BUILT_IN_HOLDINGS_KEYS.includes(d.key)))
      }).catch(() => {})
      const data = await apiClient.getHoldings()
      setTransactions(data)
      try {
        const [infoData, symFields] = await Promise.all([
          apiClient.getSymbolInfo(),
          apiClient.getHoldingsSymbolFields(),
        ])
        const infoMap: Record<string, { instrument_type: string | null; long_name: string | null; currency: string | null }> = {}
        infoData.forEach((i) => { infoMap[i.symbol] = { instrument_type: i.instrument_type, long_name: i.long_name, currency: i.currency } })
        setSymbolInfo(infoMap)
        setHoldingsSymbolFields(symFields)
        await loadPortfolioData()
      } catch (err) {
        console.error('Failed to fetch portfolio data:', err)
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load holdings')
    } finally {
      setLoading(false)
      onLoading(false)
    }
  }

  const handleSaveTransaction = async (event: React.FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    setError(null)
    setSuccess(null)

    if (!symbol.trim()) {
      setError('Symbol is required')
      return
    }

    const payload: any = {
      symbol: symbol.trim(),
      transaction_type: transactionType,
      date,
      brokerage: brokerage ? parseFloat(brokerage) : undefined,
      notes: notes.trim() || undefined,
      custom_fields: Object.keys(customFieldValues).length > 0 ? customFieldValues : undefined,
    }

    if (transactionType === 'purchase' || transactionType === 'sale') {
      const parsedQuantity = parseFloat(quantity)
      const parsedPrice = parseFloat(price)

      if (Number.isNaN(parsedQuantity) || parsedQuantity <= 0) {
        setError('Quantity must be a positive number')
        return
      }
      if (Number.isNaN(parsedPrice) || parsedPrice <= 0) {
        setError('Price must be a positive number')
        return
      }

      if (transactionType === 'sale' && editingId === null) {
        const sym = symbol.trim().toUpperCase()
        const netShares: Record<string, number> = {}
        transactions.forEach((tx) => {
          if (!netShares[tx.symbol]) netShares[tx.symbol] = 0
          if (tx.transaction_type === 'purchase' && tx.quantity) netShares[tx.symbol] += tx.quantity
          if (tx.transaction_type === 'sale' && tx.quantity) netShares[tx.symbol] -= tx.quantity
        })
        const held = netShares[sym] ?? 0
        if (held <= 0) {
          if (!confirm(`You don't currently hold any ${sym}. Record this sale anyway?`)) return
        } else if (parsedQuantity > held) {
          if (!confirm(`You're selling ${parsedQuantity} shares but only hold ${held.toFixed(2)} ${sym}. Record anyway?`)) return
        }
        // The API also guards over-sells (409); the dialog above is the acknowledgement
        payload.confirm = true
      }

      payload.quantity = parsedQuantity

      if (currency !== 'AUD') {
        // Never silently record a foreign-currency price as AUD
        if (!fxRate) {
          setError(`No ${currency}/AUD exchange rate available for ${date} — cannot save. Retry, or pick a different date.`)
          return
        }
        payload.currency = currency
        payload.original_price = parsedPrice
        payload.fx_rate = fxRate
        payload.price = parsedPrice * fxRate
      } else {
        payload.currency = 'AUD'
        payload.price = parsedPrice
      }
    }

    if (transactionType === 'dividend') {
      const parsedAmount = parseFloat(amount)
      if (Number.isNaN(parsedAmount) || parsedAmount <= 0) {
        setError('Dividend amount must be a positive number')
        return
      }
      payload.amount = parsedAmount
    }

    try {
      setLoading(true)
      onLoading(true)
      // A stock arriving via "Move to Holdings" is recorded atomically —
      // the server creates the transaction and removes the watchlist entries
      // in one call.
      const fromWatchlist = !editingId && prefillPendingSymbol.current === symbol.trim().toUpperCase()
      const result = editingId
        ? await apiClient.updateHoldingTransaction(editingId, payload)
        : fromWatchlist
          ? (await apiClient.addHoldingFromWatchlist(payload)).transaction
          : await apiClient.addHoldingTransaction(payload)

      // Save built-in symbol-level fields if provided
      const builtInUpdates: Record<string, string> = {}
      if (stopLossPrice.trim()) builtInUpdates['stop_loss'] = stopLossPrice.trim()
      if (trailingSellPct.trim()) builtInUpdates['trailing_sell_pct'] = trailingSellPct.trim()
      if (trailingSellDate.trim()) builtInUpdates['trailing_sell_date'] = trailingSellDate.trim()
      if (sector) builtInUpdates['sector'] = sector
      if (Object.keys(builtInUpdates).length > 0) {
        const sym = symbol.trim().toUpperCase()
        await apiClient.updateHoldingsSymbolFields(sym, holdingsSymbolFields[sym]?.['_notes'] ?? null, builtInUpdates)
        setHoldingsSymbolFields((prev) => ({
          ...prev,
          [sym]: { ...prev[sym], ...builtInUpdates },
        }))
      }
      onTransactionsChanged?.()
      // If this symbol came from "Move to Holdings", the watchlist entry can
      // now be removed safely — the holding actually exists.
      if (!editingId && prefillPendingSymbol.current && prefillPendingSymbol.current === result.symbol) {
        onPrefillSaved?.(result.symbol)
        prefillPendingSymbol.current = null
      }
      // Refresh transactions and server-computed portfolio data
      const refreshed = await apiClient.getHoldings()
      setTransactions(refreshed)
      await loadPortfolioData()
      setSuccess(editingId ? 'Transaction updated successfully' : 'Transaction recorded successfully')
      setSymbol('')
      setQuantity('')
      setPrice('')
      setAmount('')
      setBrokerage('')
      setNotes('')
      setStopLossPrice('')
      setTrailingSellPct('')
      setTrailingSellDate('')
      setSector('')
      setCustomFieldValues({})
      setDate(new Date().toISOString().slice(0, 10))
      setCurrency('AUD')
      setFxRate(null)
      setFxRateDate(null)
      setEditingId(null)
      setTimeout(() => setSuccess(null), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save transaction')
    } finally {
      setLoading(false)
      onLoading(false)
    }
  }

  const cancelEditing = () => {
    setEditingId(null)
    setSymbol('')
    setTransactionType('purchase')
    setDate(new Date().toISOString().slice(0, 10))
    setQuantity('')
    setPrice('')
    setAmount('')
    setBrokerage('')
    setNotes('')
    setCustomFieldValues({})
    setStopLossPrice('')
    setTrailingSellPct('')
    setTrailingSellDate('')
    setSector('')
    setCurrency('AUD')
    setFxRate(null)
    setFxRateDate(null)
  }

  const refreshHoldingPrices = async () => {
    if (transactions.length === 0) {
      setSuccess('No holdings to refresh')
      setTimeout(() => setSuccess(null), 2000)
      return
    }

    try {
      setLoading(true)
      onLoading(true)
      setError(null)
      setSuccess(null)
      const symbols = getActiveHoldingSymbols(transactions)
      // Trigger a live Yahoo fetch server-side (updates the price cache),
      // then pull the recomputed portfolio data.
      const prices = await apiClient.getCurrentPrices(symbols)
      await loadPortfolioData()

      // Reload symbol info now that fetch has populated it
      const infoData = await apiClient.getSymbolInfo()
      const infoMap: Record<string, { instrument_type: string | null; long_name: string | null; currency: string | null }> = {}
      infoData.forEach((i) => { infoMap[i.symbol] = { instrument_type: i.instrument_type, long_name: i.long_name, currency: i.currency } })
      setSymbolInfo(infoMap)

      const hasPrice = prices.some((item) => item.price !== null)
      const errorDetails = prices
        .filter((item) => item.error)
        .map((item) => `${item.symbol}: ${item.error}`)
        .join(' | ')

      if (!hasPrice) {
        setError(
          errorDetails
            ? `Update Holdings Prices failed: ${errorDetails}`
            : 'Update Holdings Prices failed: no valid prices were returned'
        )
      } else {
        setSuccess('Holdings prices updated')
        if (errorDetails) {
          setError(`Partial errors: ${errorDetails}`)
        }
        setTimeout(() => setSuccess(null), 2000)
      }
    } catch (err) {
      setError(
        err instanceof Error
          ? `Update Holdings Prices failed: ${err.message}`
          : 'Update Holdings Prices failed'
      )
    } finally {
      setLoading(false)
      onLoading(false)
    }
  }

  const refreshDividends = async () => {
    try {
      setLoading(true)
      onLoading(true)
      setError(null)
      setSuccess(null)
      const result = await apiClient.refreshDividends()
      if (result.errors.length > 0) {
        setError(`Dividend errors: ${result.errors.join(' | ')}`)
      }
      if (result.updated > 0) {
        setSuccess(`Dividends updated for ${result.updated} symbol${result.updated !== 1 ? 's' : ''}`)
        setTimeout(() => setSuccess(null), 3000)
        await loadHoldings()
      } else if (result.errors.length === 0) {
        setSuccess('No holdings to update dividends for')
        setTimeout(() => setSuccess(null), 3000)
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to update dividends')
    } finally {
      setLoading(false)
      onLoading(false)
    }
  }


  // Card items adapted from the server response (all values pre-computed)
  const summary = useMemo(() => serverHoldings.map((h) => ({
    symbol: h.symbol,
    shares: h.shares,
    invested: h.invested,
    dividends: h.dividends,
    currentPrice: h.current_price,
    nativePrice: h.native_current_price,
    priceSource: h.price_source,
    change: h.change,
    changePercent: h.change_percent,
    sma150: h.sma150,
    currentValue: h.current_value,
    avgCost: h.avg_cost,
    nativeAvgCost: h.native_avg_cost,
    pl: h.pl,
    plPct: h.pl_pct,
    longName: h.long_name,
    instrumentType: h.instrument_type,
    isEtf: h.is_etf,
    isInternational: h.is_international,
    currency: h.currency,
  })), [serverHoldings])

  const dividendTotalsBySymbol = useMemo(() => {
    const map: Record<string, number> = {}
    serverHoldings.forEach((h) => { map[h.symbol] = h.dividends })
    return map
  }, [serverHoldings])

  const [selectedChartSymbol, setSelectedChartSymbol] = useState<string>('')
  const [sortColumn, setSortColumn] = useState<string | null>(null)
  const [sortDirection, setSortDirection] = useState<'asc' | 'desc'>('asc')

  // When navigated to from the Dashboard, focus that holding's chart.
  useEffect(() => {
    if (!focusSymbol) return
    setSelectedChartSymbol(focusSymbol)
    onFocusSymbolConsumed?.()
  }, [focusSymbol])

  const handleSort = (column: string) => {
    if (sortColumn === column) {
      setSortDirection((d) => (d === 'asc' ? 'desc' : 'asc'))
    } else {
      setSortColumn(column)
      setSortDirection('asc')
    }
  }

  const sortIndicator = (column: string) => {
    if (sortColumn !== column) return ' ↕'
    return sortDirection === 'asc' ? ' ↑' : ' ↓'
  }

  const activeTransactions = useMemo(() => {
    const filtered = transactions.filter(
      (tx) => tx.transaction_type === 'purchase' && (lotMap[tx.id]?.remaining ?? 0) > 0
    )
    if (!sortColumn) return filtered
    return [...filtered].sort((a, b) => {
      let aVal: number | string | null = null
      let bVal: number | string | null = null
      if (sortColumn === 'symbol') {
        aVal = a.symbol
        bVal = b.symbol
      } else if (sortColumn === 'date') {
        aVal = a.date
        bVal = b.date
      } else if (sortColumn === 'currentValue') {
        aVal = lotMap[a.id]?.current_value ?? -Infinity
        bVal = lotMap[b.id]?.current_value ?? -Infinity
      } else if (sortColumn === 'profitLoss') {
        aVal = lotMap[a.id]?.unrealised_pl ?? -Infinity
        bVal = lotMap[b.id]?.unrealised_pl ?? -Infinity
      } else if (sortColumn === 'dividends') {
        aVal = dividendTotalsBySymbol[a.symbol] ?? 0
        bVal = dividendTotalsBySymbol[b.symbol] ?? 0
      }
      if (aVal === null) aVal = -Infinity
      if (bVal === null) bVal = -Infinity
      if (typeof aVal === 'string' && typeof bVal === 'string') {
        return sortDirection === 'asc' ? aVal.localeCompare(bVal) : bVal.localeCompare(aVal)
      }
      return sortDirection === 'asc' ? (aVal as number) - (bVal as number) : (bVal as number) - (aVal as number)
    })
  }, [transactions, sortColumn, sortDirection, lotMap, dividendTotalsBySymbol])

  return (
    <div className="holdings-manager">
      <div className="manager-card">
        <h2>Record Stock Holding</h2>
        <form onSubmit={handleSaveTransaction} className="add-symbol-form">
          <div className="form-group">
            <input
              type="text"
              value={symbol}
              onChange={(e) => setSymbol(e.target.value.toUpperCase())}
              placeholder="Symbol (e.g. BHP.AX, AAPL)"
              className="symbol-input"
              disabled={loading}
              maxLength={12}
            />
            <select
              value={transactionType}
              onChange={(e) => setTransactionType(e.target.value as HoldingTransaction['transaction_type'])}
              className="config-input"
              disabled={loading}
            >
              <option value="purchase">Purchase</option>
              <option value="sale">Sale</option>
              <option value="dividend">Dividend</option>
            </select>
            <input
              type="date"
              value={date}
              onChange={(e) => setDate(e.target.value)}
              className="config-input"
              disabled={loading}
            />
          </div>

          {(transactionType === 'purchase' || transactionType === 'sale') && (
            <>
              <div className="form-group">
                <input
                  type="number"
                  min="0"
                  step="0.01"
                  value={quantity}
                  onChange={(e) => setQuantity(e.target.value)}
                  placeholder="Quantity"
                  className="symbol-input"
                  disabled={loading}
                />
                <select
                  value={currency}
                  onChange={(e) => setCurrency(e.target.value)}
                  className="config-input"
                  disabled={loading}
                  style={{ minWidth: 80 }}
                >
                  {SUPPORTED_CURRENCIES.map((c) => (
                    <option key={c} value={c}>{c}</option>
                  ))}
                </select>
                <input
                  type="number"
                  min="0"
                  step="any"
                  value={price}
                  onChange={(e) => setPrice(e.target.value)}
                  placeholder={currency !== 'AUD' ? `Price per share (${currency})` : 'Price per share (AUD)'}
                  className="symbol-input"
                  disabled={loading}
                />
              </div>
              {currency !== 'AUD' && (
                <div className="form-group" style={{ alignItems: 'center', fontSize: 13, color: '#666', gap: 8 }}>
                  {fxLoading && <span>Fetching {currency}/AUD rate...</span>}
                  {!fxLoading && fxRate && price && !isNaN(parseFloat(price)) && (
                    <>
                      <span>
                        Rate: 1 {currency} = {fxRate.toFixed(4)} AUD
                        {fxRateDate && ` (${fxRateDate})`}
                      </span>
                      <span style={{ fontWeight: 600, color: '#333' }}>
                        → AUD {(parseFloat(price) * fxRate).toFixed(4)} per share
                      </span>
                    </>
                  )}
                  {!fxLoading && !fxRate && (
                    <span style={{ color: '#e53935' }}>Could not fetch {currency}/AUD rate for {date}</span>
                  )}
                </div>
              )}
            </>
          )}

          {transactionType === 'dividend' && (
            <div className="form-group">
              <input
                type="number"
                min="0"
                step="0.01"
                value={amount}
                onChange={(e) => setAmount(e.target.value)}
                placeholder="Dividend amount"
                className="symbol-input"
                disabled={loading}
              />
            </div>
          )}

          <div className="form-group">
            <input
              type="number"
              min="0"
              step="0.01"
              value={brokerage}
              onChange={(e) => setBrokerage(e.target.value)}
              placeholder="Brokerage fee (optional)"
              className="symbol-input"
              disabled={loading}
            />
            <input
              type="text"
              value={notes}
              onChange={(e) => setNotes(e.target.value)}
              placeholder="Notes (optional)"
              className="symbol-input"
              disabled={loading}
            />
            <select
              value={sector}
              onChange={(e) => setSector(e.target.value)}
              className="config-input"
              disabled={loading}
              title="Sector"
              style={{ minWidth: 140 }}
            >
              <option value="">Sector (optional)</option>
              {sector && !sectorOptions.includes(sector) && (
                <option value={sector}>{sector}</option>
              )}
              {sectorOptions.map((s) => (
                <option key={s} value={s}>{s}</option>
              ))}
            </select>
            <input
              type="number"
              min="0"
              step="0.01"
              value={stopLossPrice}
              onChange={(e) => setStopLossPrice(e.target.value)}
              placeholder="Stop Loss Price (optional)"
              className="symbol-input"
              disabled={loading}
            />
            <input
              type="number"
              min="0"
              step="0.1"
              value={trailingSellPct}
              onChange={(e) => setTrailingSellPct(e.target.value)}
              placeholder="Trailing Sell % (optional)"
              className="symbol-input"
              disabled={loading}
            />
            <input
              type="date"
              value={trailingSellDate}
              onChange={(e) => setTrailingSellDate(e.target.value)}
              placeholder="Trailing Sell Date"
              className="symbol-input"
              disabled={loading}
              title="Date trailing sell was placed"
            />
            <button type="submit" className="btn btn-primary" disabled={loading}>
              {loading ? 'Saving...' : editingId ? 'Save Changes' : 'Record Transaction'}
            </button>
            {editingId !== null && (
              <button type="button" className="btn btn-outline btn-small" onClick={cancelEditing} disabled={loading}>
                Cancel
              </button>
            )}
          </div>
          {holdingsFieldDefs.filter((def) => def.actions.includes(transactionType)).length > 0 && (
            <div className="form-group" style={{ flexWrap: 'wrap' }}>
              {holdingsFieldDefs
                .filter((def) => def.actions.includes(transactionType))
                .map((def) => (
                  <input
                    key={def.key}
                    type={def.type}
                    value={customFieldValues[def.key] ?? ''}
                    onChange={(e) => setCustomFieldValues((prev) => ({ ...prev, [def.key]: e.target.value }))}
                    placeholder={def.label}
                    className="symbol-input"
                    disabled={loading}
                    style={{ flex: 1, minWidth: 120 }}
                  />
                ))}
            </div>
          )}
        </form>
      </div>

      {error && <div className="alert alert-error">❌ {error}</div>}
      {success && <div className="alert alert-success">✓ {success}</div>}

      {selectedChartSymbol && (
        <div className="manager-card chart-card">
          <div className="card-header">
            <h2>Simple Moving Average Chart</h2>
            <div className="chart-select">
              <label htmlFor="holdings-chart-symbol">Symbol</label>
              <select
                id="holdings-chart-symbol"
                value={selectedChartSymbol}
                onChange={(e) => setSelectedChartSymbol(e.target.value)}
                disabled={loading}
              >
                {summary.map((item) => (
                  <option key={item.symbol} value={item.symbol}>
                    {item.symbol}
                  </option>
                ))}
              </select>
            </div>
          </div>
          {(() => {
            const sel = serverHoldings.find((h) => h.symbol === selectedChartSymbol)
            return (
              <PriceChart
                symbol={selectedChartSymbol}
                currency={sel?.currency ?? 'AUD'}
                onLoading={onLoading}
                purchasePrice={sel?.native_avg_cost ?? null}
                purchaseDate={getEarliestRemainingPurchaseDate(transactions, selectedChartSymbol)}
                currentPrice={sel?.native_current_price ?? null}
                currentVolume={sel?.volume ?? null}
                currentPriceDate={sel?.price_date ?? null}
                markerPrice={(() => { const v = holdingsSymbolFields[selectedChartSymbol]?.['stop_loss']; return v ? parseFloat(v) : null })()}
                markerLabel="Stop Loss"
                markerMode="stoploss"
              />
            )
          })()}
        </div>
      )}

      <div className="manager-card holdings-card">
        <div className="card-header">
          <h2>Holdings Summary</h2>
          <div style={{ display: 'flex', alignItems: 'center', gap: '20px', flexWrap: 'wrap' }}>
            {summary.length > 0 && (() => {
              const totalInvested = summary.reduce((s, i) => s + i.invested, 0)
              const totalValue = summary.reduce((s, i) => s + i.currentValue, 0)
              const totalDividends = summary.reduce((s, i) => s + i.dividends, 0)
              return (
                <>
                  <span style={{ fontSize: 13, color: '#666' }}>
                    Net Invested: <strong>${totalInvested.toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}</strong>
                  </span>
                  <span style={{ fontSize: 13, color: totalValue < totalInvested ? '#f44336' : '#666' }}>
                    Current Value: <strong>${totalValue.toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}</strong>
                  </span>
                  <span style={{ fontSize: 13, color: '#666' }}>
                    Dividends: <strong>${totalDividends.toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}</strong>
                  </span>
                  {(() => {
                    const totalPL = totalValue - totalInvested + totalDividends
                    const pct = totalInvested > 0 ? (totalPL / totalInvested) * 100 : null
                    return (
                      <span style={{ fontSize: 13, color: totalPL >= 0 ? '#4caf50' : '#f44336', fontWeight: 600 }}>
                        P/L: {totalPL >= 0 ? '+' : '-'}${Math.abs(totalPL).toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
                        {pct !== null && <span style={{ fontWeight: 400, marginLeft: 4 }}>({pct >= 0 ? '+' : ''}{pct.toFixed(1)}%)</span>}
                      </span>
                    )
                  })()}
                </>
              )
            })()}
            <div style={{ display: 'flex', gap: '8px' }}>
              <button className="btn btn-outline btn-small" onClick={refreshHoldingPrices} disabled={loading || transactions.length === 0}>
                Update Prices
              </button>
              <button className="btn btn-outline btn-small" onClick={refreshDividends} disabled={loading || transactions.length === 0}>
                Update Dividends
              </button>
            </div>
          </div>
        </div>

        {loading && transactions.length === 0 ? (
          <p className="loading-text">Loading holdings...</p>
        ) : transactions.length === 0 ? (
          <p className="empty-text">No holdings configured.</p>
        ) : (
          <div style={{ maxHeight: 600, overflowY: 'auto' }}>
            {(() => {
              // Classification (ETF vs equity, domestic vs international) is
              // computed server-side, including config overrides.
              const domesticEquities = summary.filter((i) => !i.isEtf && !i.isInternational)
              const intlEquities = summary.filter((i) => !i.isEtf && i.isInternational)
              const domesticETFs = summary.filter((i) => i.isEtf && !i.isInternational)
              const intlETFs = summary.filter((i) => i.isEtf && i.isInternational)

              const renderCard = (item: typeof summary[0]) => {
                const symCurrency = item.currency
                const isForeign = item.isInternational
                const isSelected = selectedChartSymbol === item.symbol
                return (
                <div
                  key={item.symbol}
                  className="holdings-summary-card"
                  onClick={() => setSelectedChartSymbol(item.symbol)}
                  style={{ cursor: 'pointer', outline: isSelected ? '2px solid #1976d2' : undefined, outlineOffset: isSelected ? '-2px' : undefined, position: 'relative' }}
                >
                  <button
                    onClick={(e) => {
                      e.stopPropagation()
                      setEditingSymbolCard(item.symbol)
                      setEditCardSymbol(item.symbol)
                      setEditCardNotes(holdingsSymbolFields[item.symbol]?.['_notes'] ?? '')
                      const fields: Record<string, string> = { stop_loss: holdingsSymbolFields[item.symbol]?.['stop_loss'] ?? '', trailing_sell_pct: holdingsSymbolFields[item.symbol]?.['trailing_sell_pct'] ?? '', trailing_sell_date: holdingsSymbolFields[item.symbol]?.['trailing_sell_date'] ?? '', sector: holdingsSymbolFields[item.symbol]?.['sector'] ?? '' }
                      holdingsFieldDefs.forEach((def) => {
                        fields[def.key] = holdingsSymbolFields[item.symbol]?.[def.key] ?? ''
                      })
                      setEditCardFields(fields)
                    }}
                    className="btn btn-outline btn-small"
                    style={{ position: 'absolute', top: 4, right: 4, padding: '2px 6px', fontSize: 12, lineHeight: 1 }}
                    title="Edit notes & fields"
                  >
                    ✏️
                  </button>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 6, flexWrap: 'wrap' }}>
                    <strong>{item.symbol}</strong>
                    {item.instrumentType && (
                      <span style={{ fontSize: 10, fontWeight: 600, padding: '1px 5px', borderRadius: 4, background: item.instrumentType === 'ETF' ? '#e3f2fd' : '#f3e5f5', color: item.instrumentType === 'ETF' ? '#1565c0' : '#6a1b9a' }}>
                        {item.instrumentType}
                      </span>
                    )}
                    {isForeign && (
                      <span style={{ fontSize: 10, fontWeight: 600, padding: '1px 5px', borderRadius: 4, background: '#fff3e0', color: '#e65100' }}>
                        {symCurrency}
                      </span>
                    )}
                  </div>
                  {item.longName && (
                    <div style={{ fontSize: 11, color: '#888', marginBottom: 2 }}>{item.longName}</div>
                  )}
                  {holdingsSymbolFields[item.symbol]?.['_notes'] && (
                    <div style={{ fontSize: 11, color: '#5c6bc0', fontStyle: 'italic', marginBottom: 2 }}>{holdingsSymbolFields[item.symbol]['_notes']}</div>
                  )}
                  <div style={{ color: item.priceSource === 'manual' ? '#2196f3' : undefined }}>
                    {item.shares % 1 === 0 ? item.shares.toFixed(0) : item.shares.toFixed(2)}@{item.currentPrice ? `$${item.currentPrice.toFixed(2)}` : '—'}
                    {isForeign && item.nativePrice != null && (
                      <span style={{ fontSize: 11, color: '#888', marginLeft: 6 }}>
                        ({symCurrency} {item.nativePrice.toFixed(2)})
                      </span>
                    )}
                    {item.priceSource === 'manual' && <span style={{ fontSize: 11, marginLeft: 4 }}>(manual)</span>}
                  </div>
                  {item.change !== null && item.changePercent !== null && (
                    <div style={{ color: item.change >= 0 ? '#4caf50' : '#f44336', fontSize: 12 }}>
                      {item.change >= 0 ? '+' : ''}{item.change.toFixed(2)} ({item.change >= 0 ? '+' : ''}{item.changePercent.toFixed(2)}%)
                    </div>
                  )}
                  {item.sma150 !== null && (
                    <div style={{ color: item.currentPrice !== null && item.sma150 > item.currentPrice ? '#f44336' : undefined }}>
                      150SMA: ${item.sma150.toFixed(2)}
                    </div>
                  )}
                  <div>Current value: ${item.currentValue.toFixed(2)}</div>
                  <div>Dividends: ${item.dividends.toFixed(2)}</div>
                  {holdingsSymbolFields[item.symbol]?.['stop_loss'] && (
                    <div style={{ fontSize: 11, color: '#555' }}>
                      <span style={{ color: '#999' }}>Stop Loss:</span> {holdingsSymbolFields[item.symbol]['stop_loss']}
                    </div>
                  )}
                  {holdingsSymbolFields[item.symbol]?.['trailing_sell_pct'] && (
                    <div style={{ fontSize: 11, color: '#555' }}>
                      <span style={{ color: '#999' }}>Trailing Sell:</span> {holdingsSymbolFields[item.symbol]['trailing_sell_pct']}%
                      {holdingsSymbolFields[item.symbol]?.['trailing_sell_date'] && (
                        <span style={{ marginLeft: 4 }}>({new Date(holdingsSymbolFields[item.symbol]['trailing_sell_date']).toLocaleDateString()})</span>
                      )}
                    </div>
                  )}
                  {holdingsSymbolFields[item.symbol]?.['sector'] && (
                    <div style={{ fontSize: 11, color: '#555' }}>
                      <span style={{ color: '#999' }}>Sector:</span> {holdingsSymbolFields[item.symbol]['sector']}
                    </div>
                  )}
                  {holdingsFieldDefs.map((def) => {
                    const val = holdingsSymbolFields[item.symbol]?.[def.key]
                    return val ? (
                      <div key={def.key} style={{ fontSize: 11, color: '#555' }}>
                        <span style={{ color: '#999' }}>{def.label}:</span> {val}
                      </div>
                    ) : null
                  })}
                  {(() => {
                    const pl = item.currentValue - item.invested + item.dividends
                    const pct = item.invested > 0 ? (pl / item.invested) * 100 : null
                    return (
                      <div style={{ color: pl >= 0 ? '#4caf50' : '#f44336', fontWeight: 600 }}>
                        P/L: {pl >= 0 ? '+' : '-'}${Math.abs(pl).toFixed(2)}
                        {pct !== null && <span style={{ fontWeight: 400, marginLeft: 4 }}>({pct >= 0 ? '+' : ''}{pct.toFixed(1)}%)</span>}
                      </div>
                    )
                  })()}
                </div>
              )}

              const renderGroup = (items: typeof summary, label: string) => {
                if (items.length === 0) return null
                const inv = items.reduce((s, i) => s + i.invested, 0)
                const val = items.reduce((s, i) => s + i.currentValue, 0)
                const div = items.reduce((s, i) => s + i.dividends, 0)
                const pl = val - inv + div
                return (
                  <div style={{ marginBottom: 24 }}>
                    <div style={{ display: 'flex', alignItems: 'center', gap: 16, marginBottom: 8, flexWrap: 'wrap' }}>
                      <h3 style={{ margin: 0, fontSize: 15 }}>{label}</h3>
                      <span style={{ fontSize: 13, color: '#666' }}>Net Invested: <strong>${inv.toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}</strong></span>
                      <span style={{ fontSize: 13, color: val < inv ? '#f44336' : '#666' }}>Current Value: <strong>${val.toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}</strong></span>
                      <span style={{ fontSize: 13, color: '#666' }}>Dividends: <strong>${div.toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}</strong></span>
                      <span style={{ fontSize: 13, color: pl >= 0 ? '#4caf50' : '#f44336', fontWeight: 600 }}>
                        P/L: {pl >= 0 ? '+' : '-'}${Math.abs(pl).toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
                        {inv > 0 && <span style={{ fontWeight: 400, marginLeft: 4 }}>({pl >= 0 ? '+' : ''}{((pl / inv) * 100).toFixed(1)}%)</span>}
                      </span>
                    </div>
                    <div className="holdings-summary-grid">
                      {items.map(renderCard)}
                    </div>
                  </div>
                )
              }

              return (
                <>
                  {renderGroup(domesticEquities, 'Equities')}
                  {renderGroup(intlEquities, 'International Equities')}
                  {renderGroup(domesticETFs, 'ETFs')}
                  {renderGroup(intlETFs, 'International ETFs')}
                </>
              )
            })()}

            <div className="holdings-table-wrapper">
              <h3>Active Holdings</h3>
              <table className="holdings-table">
                <thead>
                  <tr>
                    <th className="sortable-header" onClick={() => handleSort('symbol')}>Symbol{sortIndicator('symbol')}</th>
                    <th className="sortable-header" onClick={() => handleSort('date')}>Date{sortIndicator('date')}</th>
                    <th>Quantity</th>
                    <th>Price (AUD)</th>
                    <th className="sortable-header" onClick={() => handleSort('currentValue')}>Current Value{sortIndicator('currentValue')}</th>
                    <th className="sortable-header" onClick={() => handleSort('profitLoss')}>Unrealised P/L{sortIndicator('profitLoss')}</th>
                    <th className="sortable-header" onClick={() => handleSort('dividends')}>Total Dividends{sortIndicator('dividends')}</th>
                    <th>Brokerage</th>
                    <th>Notes</th>
                    {holdingsFieldDefs.map((def) => (
                      <th key={def.key}>{def.label}</th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {activeTransactions.map((tx) => {
                    const lot = lotMap[tx.id]
                    const currentValue = lot?.current_value ?? null
                    const profitLoss = lot?.unrealised_pl ?? null
                    const symCurrency = symbolInfo[tx.symbol]?.currency?.toUpperCase()
                    const isForeignTx = !!symCurrency && symCurrency !== 'AUD'
                    return (
                      <tr key={tx.id}>
                        <td>
                          <div style={{ display: 'flex', alignItems: 'center', gap: 4 }}>
                            {tx.symbol}
                            {isForeignTx && (
                              <span style={{ fontSize: 10, fontWeight: 600, padding: '1px 4px', borderRadius: 3, background: '#fff3e0', color: '#e65100' }}>
                                {symCurrency}
                              </span>
                            )}
                          </div>
                        </td>
                        <td>{new Date(tx.date).toLocaleDateString()}</td>
                        <td>{(lot?.remaining ?? 0).toFixed(2)}</td>
                        <td>
                          {tx.price !== null ? `$${tx.price.toFixed(2)}` : '—'}
                          {tx.currency !== 'AUD' && tx.original_price !== null && (
                            <span style={{ fontSize: 10, color: '#888', marginLeft: 4 }}>
                              ({tx.currency} {tx.original_price.toFixed(2)})
                            </span>
                          )}
                        </td>
                        <td>{currentValue !== null ? `$${currentValue.toFixed(2)}` : '—'}</td>
                        <td>
                          {profitLoss !== null ? (() => {
                            const pl = profitLoss
                            const costBasis = currentValue !== null ? currentValue - pl : null
                            const pct = costBasis !== null && costBasis > 0 ? (pl / costBasis) * 100 : null
                            return (
                              <span style={{ color: pl >= 0 ? '#4caf50' : '#f44336' }}>
                                {pl >= 0 ? '+' : '-'}${Math.abs(pl).toFixed(2)}
                                {pct !== null && <span style={{ fontWeight: 400, marginLeft: 4 }}>({pct >= 0 ? '+' : ''}{pct.toFixed(1)}%)</span>}
                              </span>
                            )
                          })() : '—'}
                        </td>
                        <td>
                          {dividendTotalsBySymbol[tx.symbol] !== undefined
                            ? `$${dividendTotalsBySymbol[tx.symbol].toFixed(2)}`
                            : '—'}
                        </td>
                        <td>{tx.brokerage !== null ? `$${tx.brokerage.toFixed(2)}` : '—'}</td>
                        <td>{tx.notes || '—'}</td>
                        {holdingsFieldDefs.map((def) => (
                          <td key={def.key}>{tx.custom_fields?.[def.key] || '—'}</td>
                        ))}
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>

          </div>
        )}
      </div>

      {editingSymbolCard && (
        <div style={{ position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.4)', zIndex: 1000, display: 'flex', alignItems: 'center', justifyContent: 'center' }}
          onClick={() => setEditingSymbolCard(null)}>
          <div style={{ background: '#fff', borderRadius: 8, padding: 24, minWidth: 320, boxShadow: '0 4px 24px rgba(0,0,0,0.18)' }}
            onClick={(e) => e.stopPropagation()}>
            <h3 style={{ margin: '0 0 16px' }}>Edit {editingSymbolCard}</h3>
            <div style={{ display: 'flex', flexDirection: 'column', gap: 4, marginBottom: 16 }}>
              <label style={{ fontSize: 13, color: '#666' }}>Stock Symbol</label>
              <input
                type="text"
                value={editCardSymbol}
                onChange={(e) => setEditCardSymbol(e.target.value.toUpperCase())}
                placeholder="e.g. BHP.AX"
                className="symbol-input"
                style={{ width: '100%' }}
                maxLength={12}
              />
              {editCardSymbol.trim().toUpperCase() !== editingSymbolCard && (
                <span style={{ fontSize: 11, color: '#e65100' }}>
                  Renames the symbol across all of this holding's transactions.
                </span>
              )}
            </div>
            <div style={{ display: 'flex', flexDirection: 'column', gap: 4, marginBottom: 16 }}>
              <label style={{ fontSize: 13, color: '#666' }}>Notes</label>
              <input
                type="text"
                value={editCardNotes}
                onChange={(e) => setEditCardNotes(e.target.value)}
                placeholder="Add a note..."
                className="symbol-input"
                style={{ width: '100%' }}
              />
            </div>
            <div style={{ display: 'flex', flexDirection: 'column', gap: 10, marginBottom: 16 }}>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 13, color: '#666' }}>Stop Loss Price</label>
                <input
                  type="number"
                  value={editCardFields['stop_loss'] ?? ''}
                  onChange={(e) => setEditCardFields((prev) => ({ ...prev, stop_loss: e.target.value }))}
                  className="symbol-input"
                  style={{ width: '100%' }}
                />
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 13, color: '#666' }}>Trailing Sell %</label>
                <input
                  type="number"
                  min="0"
                  step="0.1"
                  value={editCardFields['trailing_sell_pct'] ?? ''}
                  onChange={(e) => setEditCardFields((prev) => ({ ...prev, trailing_sell_pct: e.target.value }))}
                  className="symbol-input"
                  style={{ width: '100%' }}
                />
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 13, color: '#666' }}>Trailing Sell Date</label>
                <input
                  type="date"
                  value={editCardFields['trailing_sell_date'] ?? ''}
                  onChange={(e) => setEditCardFields((prev) => ({ ...prev, trailing_sell_date: e.target.value }))}
                  className="symbol-input"
                  style={{ width: '100%' }}
                />
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 13, color: '#666' }}>Sector</label>
                <select
                  value={editCardFields['sector'] ?? ''}
                  onChange={(e) => setEditCardFields((prev) => ({ ...prev, sector: e.target.value }))}
                  className="symbol-input"
                  style={{ width: '100%' }}
                >
                  <option value="">— None —</option>
                  {(editCardFields['sector'] ?? '') !== '' && !sectorOptions.includes(editCardFields['sector']) && (
                    <option value={editCardFields['sector']}>{editCardFields['sector']}</option>
                  )}
                  {sectorOptions.map((s) => (
                    <option key={s} value={s}>{s}</option>
                  ))}
                </select>
              </div>
              {holdingsFieldDefs.map((def) => (
                <div key={def.key} style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                  <label style={{ fontSize: 13, color: '#666' }}>{def.label}</label>
                  <input
                    type={def.type}
                    value={editCardFields[def.key] ?? ''}
                    onChange={(e) => setEditCardFields((prev) => ({ ...prev, [def.key]: e.target.value }))}
                    className="symbol-input"
                    style={{ width: '100%' }}
                  />
                </div>
              ))}
            </div>
            <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
              <button className="btn btn-outline" onClick={() => setEditingSymbolCard(null)}>Cancel</button>
              <button
                className="btn btn-primary"
                disabled={loading}
                onClick={async () => {
                  if (!editingSymbolCard) return
                  const oldSymbol = editingSymbolCard
                  const newSymbol = editCardSymbol.trim().toUpperCase()
                  if (!newSymbol) {
                    setError('Stock symbol is required')
                    return
                  }
                  const symbolChanged = newSymbol !== oldSymbol
                  if (symbolChanged && transactions.some((tx) => tx.symbol === newSymbol)) {
                    if (!confirm(`You already have transactions under ${newSymbol}. Merge ${oldSymbol}'s transactions into ${newSymbol}?`)) return
                  }
                  try {
                    setLoading(true)
                    setError(null)
                    const targetSymbol = symbolChanged ? newSymbol : oldSymbol
                    if (symbolChanged) {
                      await apiClient.renameHoldingSymbol(oldSymbol, newSymbol)
                    }
                    const fields = { ...editCardFields }
                    await apiClient.updateHoldingsSymbolFields(targetSymbol, editCardNotes.trim() || null, fields)
                    setEditingSymbolCard(null)
                    if (symbolChanged) {
                      // Transactions changed — reload everything and notify other tabs
                      if (selectedChartSymbol === oldSymbol) setSelectedChartSymbol(newSymbol)
                      await loadHoldings()
                      onTransactionsChanged?.()
                      setSuccess(`Renamed ${oldSymbol} to ${newSymbol}`)
                    } else {
                      // Update local state in place
                      const updated = { ...holdingsSymbolFields }
                      if (!updated[targetSymbol]) updated[targetSymbol] = {}
                      if (editCardNotes.trim()) {
                        updated[targetSymbol]['_notes'] = editCardNotes.trim()
                      } else {
                        delete updated[targetSymbol]['_notes']
                      }
                      Object.entries(fields).forEach(([k, v]) => {
                        if (v) updated[targetSymbol][k] = v
                        else delete updated[targetSymbol][k]
                      })
                      setHoldingsSymbolFields(updated)
                      setSuccess('Updated fields for ' + targetSymbol)
                    }
                    setTimeout(() => setSuccess(null), 3000)
                  } catch (err) {
                    setError(err instanceof Error ? err.message : 'Failed to update fields')
                  } finally {
                    setLoading(false)
                  }
                }}
              >
                {loading ? 'Saving...' : 'Save'}
              </button>
            </div>
          </div>
        </div>
      )}

    </div>
  )
}
