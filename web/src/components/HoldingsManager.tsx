import { useEffect, useMemo, useRef, useState } from 'react'
import { apiClient, type HoldingTransactionPayload, type PortfolioHolding, type PortfolioLot, type CashAccount } from '../services/api'
import { getActiveHoldingSymbols, getEarliestRemainingPurchaseDate, getRemainingPurchaseLots } from '../utils/holdings'
import { settlementAccountsFor } from '../utils/cash'
import { SECTORS } from '../utils/sectors'
import PriceChart from './PriceChart'
import HoldingsHeatMap from './HoldingsHeatMap'
import CollapsibleCard from './CollapsibleCard'
import type { HoldingScope } from '../utils/holdingScope'
import { priceParts } from '../utils/priceDisplay'

/**
 * The sections each screen is organised by, in display order.
 *
 * Classification (ETF vs equity, domestic vs international) is computed
 * server-side, including config overrides; this only names the combinations and
 * says which screen each belongs to. It is the single source of truth for both:
 * the per-section heat maps and the summary groups read the same list, and a
 * holding that landed in one section's map but another's card grid would be a
 * quietly wrong screen.
 */
const SECTIONS_BY_SCOPE = {
  local: ['Equities', 'ETFs'],
  international: ['International Equities', 'International ETFs'],
} as const satisfies Record<HoldingScope, readonly string[]>

type HoldingSection = (typeof SECTIONS_BY_SCOPE)[HoldingScope][number]

/** How each screen names itself when it has nothing to show. */
const SCOPE_NOUN: Record<HoldingScope, string> = { local: 'local', international: 'international' }

const sectionOf = (isEtf: boolean, isInternational: boolean): HoldingSection =>
  isEtf
    ? isInternational ? 'International ETFs' : 'ETFs'
    : isInternational ? 'International Equities' : 'Equities'

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
const BUILT_IN_HOLDINGS_KEYS = ['stop_loss', 'trailing_sell_pct', 'trailing_sell_date', 'sector', 'pl_basis_date', 'pl_basis_price']


interface HoldingsPrefill {
  symbol: string
  price?: number
  notes?: string
  customFields?: Record<string, string>
}

export default function HoldingsManager({ scope, onLoading, onTransactionsChanged, configVersion, holdingsVersion, prefill, onPrefillConsumed, onPrefillSaved, focusSymbol, onFocusSymbolConsumed }: { scope: HoldingScope; onLoading: (loading: boolean) => void; onTransactionsChanged?: () => void; configVersion?: number; holdingsVersion?: number; prefill?: HoldingsPrefill | null; onPrefillConsumed?: () => void; onPrefillSaved?: (symbol: string) => void; focusSymbol?: string | null; onFocusSymbolConsumed?: () => void }) {
  /**
   * Whether a holding belongs to this screen. Declared up here because the
   * loaders below reach for it before any of the derived lists exist.
   */
  const inScope = (h: { is_international: boolean }) => h.is_international === (scope === 'international')
  /** International holdings read in their own currency; local ones in AUD. */
  const nativeFirst = scope === 'international'
  const scopeSections = SECTIONS_BY_SCOPE[scope]

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
  const [cashAccountId, setCashAccountId] = useState('')
  const [cashAccounts, setCashAccounts] = useState<CashAccount[]>([])
  // An account in the transaction's currency, or an AUD account that converts.
  // The API applies the same rule, so the picker never offers one it would
  // refuse.
  const settlementAccounts = useMemo(
    () => settlementAccountsFor(cashAccounts, currency),
    [cashAccounts, currency],
  )
  const [fxRate, setFxRate] = useState<number | null>(null)
  const [fxRateDate, setFxRateDate] = useState<string | null>(null)
  const [fxLoading, setFxLoading] = useState(false)
  const [customFieldValues, setCustomFieldValues] = useState<Record<string, string>>({})
  const [holdingsSymbolFields, setHoldingsSymbolFields] = useState<Record<string, Record<string, string>>>({})
  // Symbol whose form values came from a watchlist prefill. While the form
  // still shows that symbol, the per-symbol pre-population effect must not
  // overwrite the prefilled sector/custom fields — including when the
  // holdings symbol fields finish loading after the prefill was applied.
  const prefillAppliedSymbol = useRef<string | null>(null)
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
  // Server-driven currency list from /api/meta; static list is the offline fallback
  const [currencyOptions, setCurrencyOptions] = useState<string[]>([...SUPPORTED_CURRENCIES])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [success, setSuccess] = useState<string | null>(null)
  // The record form lives in a dialog. The card it used to fill now holds only
  // the trigger, so the holdings tables sit at the top of the screen.
  const [showRecordDialog, setShowRecordDialog] = useState(false)

  // Shared by the trade and dividend rows: income arrives in a currency just as
  // a purchase is paid in one, and a foreign dividend recorded as AUD would be
  // silently wrong by the exchange rate.
  const currencySelect = (
    <select
      value={currency}
      onChange={(e) => setCurrency(e.target.value)}
      className="config-input"
      disabled={loading}
      style={{ minWidth: 80 }}
      title="Currency this amount is in"
    >
      {currencyOptions.map((c) => (
        <option key={c} value={c}>{c}</option>
      ))}
    </select>
  )

  // Extracted so a confirmed Cancel clears the form the same way a successful
  // save does — otherwise "discard" would leave the discarded values behind.
  const resetTransactionForm = () => {
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
    setCashAccountId('')
    setFxRate(null)
    setFxRateDate(null)
  }

  const recordFormIsDirty = () =>
    [symbol, quantity, price, amount, brokerage, notes, stopLossPrice, trailingSellPct, trailingSellDate, sector]
      .some((v) => v.trim() !== '') ||
    Object.values(customFieldValues).some((v) => v.trim() !== '')

  // Unlike the short edit dialogs in this file, this form is long enough that
  // losing it to a stray dismissal would be costly — hence the confirm, and no
  // close-on-overlay-click.
  const closeRecordDialog = () => {
    if (recordFormIsDirty() && !confirm('Discard this unsaved transaction?')) return
    resetTransactionForm()
    setError(null)
    setShowRecordDialog(false)
  }

  // A ref keeps the Escape handler reading current form values; subscribing on
  // every render instead would re-bind the listener continuously.
  const closeRecordDialogRef = useRef(closeRecordDialog)
  closeRecordDialogRef.current = closeRecordDialog

  useEffect(() => {
    if (!showRecordDialog) return
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') closeRecordDialogRef.current()
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [showRecordDialog])

  // holdingsVersion, not just configVersion: a holding recorded on the other
  // screen belongs to this one whenever its currency says so, and that screen
  // is already mounted behind this one rather than remounted on the way in.
  useEffect(() => {
    loadHoldings()
  }, [configVersion, holdingsVersion])

  // Pre-fill form when navigating from watchlist
  useEffect(() => {
    if (!prefill) return
    setSymbol(prefill.symbol)
    setTransactionType('purchase')
    setDate(new Date().toISOString().slice(0, 10))
    setQuantity('')
    if (prefill.price != null) setPrice(prefill.price.toString())
    else setPrice('')
    setAmount('')
    setBrokerage('')
    setNotes(prefill.notes ?? '')
    // Map watchlist custom fields to holdings custom fields by matching key
    // names (built-in keys have dedicated inputs and are handled below)
    const mappedFields: Record<string, string> = {}
    if (prefill.customFields) {
      const holdingsKeys = new Set(holdingsFieldDefs.map((d) => d.key))
      for (const [key, value] of Object.entries(prefill.customFields)) {
        if (holdingsKeys.has(key) && value) {
          mappedFields[key] = value
        }
      }
    }
    setCustomFieldValues(mappedFields)
    // Carry the watchlist sector across to the holding
    setSector(prefill.customFields?.['sector'] ?? '')
    // Carry the watchlist stop loss into the dedicated Stop Loss input so it
    // is visible and editable; it's persisted as a symbol-level field when
    // the transaction is saved (like any manually entered stop loss).
    setStopLossPrice(prefill.customFields?.['stop_loss'] ?? '')
    setTrailingSellPct('')
    setTrailingSellDate('')
    prefillAppliedSymbol.current = prefill.symbol.trim().toUpperCase()
    prefillPendingSymbol.current = prefill.symbol.trim().toUpperCase()
    // The form is behind a button now, so a prefill that did not open the
    // dialog would look like "Move to Holdings" had done nothing.
    setShowRecordDialog(true)
    onPrefillConsumed?.()
  }, [prefill])

  // Auto-detect currency from symbolInfo when adding a new transaction
  useEffect(() => {
    const detected = symbolInfo[symbol]?.currency?.toUpperCase()
    if (detected && detected !== 'AUD' && currencyOptions.includes(detected)) {
      setCurrency(detected)
    } else if (detected === 'AUD') {
      setCurrency('AUD')
    }
  }, [symbol, symbolInfo, currencyOptions])

  // Pre-populate custom fields from per-symbol values when entering a new transaction
  useEffect(() => {
    const sym = symbol.trim().toUpperCase()
    if (prefillAppliedSymbol.current) {
      // The prefilled values win while their symbol is in the form. An empty
      // symbol is the render before the prefill's setSymbol lands (or a form
      // reset) — not a user edit, so keep the guard armed through it.
      if (prefillAppliedSymbol.current === sym || sym === '') return
      prefillAppliedSymbol.current = null
    }
    const symFields = holdingsSymbolFields[sym]
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
  }, [symbol, holdingsSymbolFields])

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
    setSelectedChartSymbol((prev) => prev || ph.holdings.find(inScope)?.symbol || '')
  }

  const loadHoldings = async () => {
    try {
      setLoading(true)
      setError(null)
      onLoading(true)
      apiClient.getMeta().then((m) => {
        if (m.sectors?.length) setSectorOptions(m.sectors)
        if (m.currencies?.length) setCurrencyOptions(m.currencies)
        setHoldingsFieldDefs(((m.holdings_custom_fields ?? []) as HoldingsFieldDef[]).filter((d) => !BUILT_IN_HOLDINGS_KEYS.includes(d.key)))
      }).catch(() => {})
      // Settlement accounts for the picker; an empty list simply hides it.
      apiClient.getCashAccounts().then(setCashAccounts).catch(() => setCashAccounts([]))
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

    const payload: HoldingTransactionPayload = {
      symbol: symbol.trim(),
      transaction_type: transactionType,
      date,
      // The brokerage input is hidden for a dividend; a value left over from a
      // trade the user was mid-way through must not ride along with it.
      brokerage: transactionType !== 'dividend' && brokerage ? parseFloat(brokerage) : undefined,
      notes: notes.trim() || undefined,
      custom_fields: Object.keys(customFieldValues).length > 0 ? customFieldValues : undefined,
      cash_account_id: cashAccountId ? Number(cashAccountId) : null,
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

      if (transactionType === 'sale') {
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
      if (currency !== 'AUD') {
        // Never silently record a foreign dividend as AUD.
        if (!fxRate) {
          setError(`No ${currency}/AUD exchange rate available for ${date} — cannot save. Retry, or pick a different date.`)
          return
        }
        // `amount` is stored in AUD, matching `price` on a trade; the rate is
        // kept so the server can recover the native figure when the dividend
        // settles into an account held in that currency.
        payload.currency = currency
        payload.fx_rate = fxRate
        payload.amount = parsedAmount * fxRate
      } else {
        payload.currency = 'AUD'
        payload.amount = parsedAmount
      }
    }

    try {
      setLoading(true)
      onLoading(true)
      // A stock arriving via "Move to Holdings" is recorded atomically —
      // the server creates the transaction and removes the watchlist entries
      // in one call.
      const fromWatchlist = prefillPendingSymbol.current === symbol.trim().toUpperCase()
      const result = fromWatchlist
        ? (await apiClient.addHoldingFromWatchlist(payload)).transaction
        : await apiClient.addHoldingTransaction(payload)

      // Save built-in symbol-level fields if provided. Only a purchase offers
      // these inputs, so only a purchase may write them — otherwise a sale
      // would silently re-save whatever the form still held.
      const builtInUpdates: Record<string, string> = {}
      if (transactionType === 'purchase') {
        if (stopLossPrice.trim()) builtInUpdates['stop_loss'] = stopLossPrice.trim()
        if (trailingSellPct.trim()) builtInUpdates['trailing_sell_pct'] = trailingSellPct.trim()
        if (trailingSellDate.trim()) builtInUpdates['trailing_sell_date'] = trailingSellDate.trim()
        if (sector) builtInUpdates['sector'] = sector
      }
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
      if (prefillPendingSymbol.current && prefillPendingSymbol.current === result.symbol) {
        onPrefillSaved?.(result.symbol)
        prefillPendingSymbol.current = null
      }
      // Refresh transactions and server-computed portfolio data
      const refreshed = await apiClient.getHoldings()
      setTransactions(refreshed)
      await loadPortfolioData()
      setSuccess('Transaction recorded successfully')
      // The prefill is consumed by the save — later manual entry of the same
      // symbol should pre-populate from its stored fields again
      prefillAppliedSymbol.current = null
      resetTransactionForm()
      // Only a success closes the dialog: validation and API failures below
      // must leave it open with the user's input intact.
      setShowRecordDialog(false)
      setTimeout(() => setSuccess(null), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save transaction')
    } finally {
      setLoading(false)
      onLoading(false)
    }
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


  const scopedHoldings = useMemo(
    () => serverHoldings.filter((h) => h.is_international === (scope === 'international')),
    [serverHoldings, scope],
  )

  // Card items adapted from the server response (all values pre-computed)
  const summary = useMemo(() => scopedHoldings.map((h) => ({
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
    nativeSma150: h.native_sma150,
    currentValue: h.current_value,
    avgCost: h.avg_cost,
    nativeAvgCost: h.native_avg_cost,
    pl: h.pl,
    plPct: h.pl_pct,
    basisDate: h.basis_date,
    longName: h.long_name,
    instrumentType: h.instrument_type,
    isEtf: h.is_etf,
    isInternational: h.is_international,
    currency: h.currency,
  })), [scopedHoldings])

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
    const scopedSymbols = new Set(scopedHoldings.map((h) => h.symbol))
    const filtered = transactions.filter(
      (tx) =>
        tx.transaction_type === 'purchase' &&
        (lotMap[tx.id]?.remaining ?? 0) > 0 &&
        scopedSymbols.has(tx.symbol)
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
  }, [transactions, sortColumn, sortDirection, lotMap, dividendTotalsBySymbol, scopedHoldings])

  return (
    <div className="holdings-manager">
      <div className="manager-card">
        <div className="card-header" style={{ marginBottom: 0 }}>
          <h2>Record Stock Holding</h2>
          <button
            type="button"
            className="btn btn-primary"
            onClick={() => setShowRecordDialog(true)}
            disabled={loading}
          >
            Record Holding…
          </button>
        </div>
      </div>

      {showRecordDialog && (
        <div className="modal-overlay">
          <div className="modal-card" style={{ width: 720, maxWidth: '100%' }}>
            {/* The form wraps header/body/footer so the submit button in the
                footer still belongs to it, and carries the card's column
                sizing so the body is what scrolls. */}
            <form
              onSubmit={handleSaveTransaction}
              style={{ display: 'flex', flexDirection: 'column', flex: '1 1 auto', minHeight: 0 }}
            >
              <div className="modal-header">
                <h3 style={{ margin: 0 }}>Record Stock Holding</h3>
              </div>
              <div className="modal-body">
                <div className="add-symbol-form">
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
                        {currencySelect}
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
                    <>
                      <div className="form-group">
                        {currencySelect}
                        <input
                          type="number"
                          min="0"
                          step="0.01"
                          value={amount}
                          onChange={(e) => setAmount(e.target.value)}
                          placeholder={currency !== 'AUD' ? `Dividend amount (${currency})` : 'Dividend amount (AUD)'}
                          className="symbol-input"
                          disabled={loading}
                        />
                      </div>
                      {currency !== 'AUD' && (
                        <div className="form-group" style={{ alignItems: 'center', fontSize: 13, color: '#666', gap: 8 }}>
                          {fxLoading && <span>Fetching {currency}/AUD rate...</span>}
                          {!fxLoading && fxRate && amount && !isNaN(parseFloat(amount)) && (
                            <span>
                              Rate: 1 {currency} = {fxRate.toFixed(4)} AUD{fxRateDate && ` (${fxRateDate})`}
                              {' → '}AUD {(parseFloat(amount) * fxRate).toFixed(2)}
                            </span>
                          )}
                          {!fxLoading && !fxRate && (
                            <span style={{ color: '#c62828' }}>No {currency}/AUD rate for {date} — cannot save.</span>
                          )}
                        </div>
                      )}
                    </>
                  )}

                  {/* Outside the trade branch: a dividend needs somewhere to land just
                      as much as a purchase needs somewhere to draw from. */}
                  {cashAccounts.length > 0 && (
                    <div className="form-group">
                      <select
                        value={settlementAccounts.some((a) => String(a.id) === cashAccountId) ? cashAccountId : ''}
                        onChange={(e) => setCashAccountId(e.target.value)}
                        className="config-input"
                        disabled={loading || settlementAccounts.length === 0}
                        title={transactionType === 'purchase'
                          ? 'Which cash account this purchase is paid from'
                          : 'Which cash account this money is paid into'}
                      >
                        <option value="">
                          {settlementAccounts.length === 0
                            ? `No account can settle ${currency} — this will not touch the ledger`
                            : transactionType === 'purchase'
                              ? 'Pay from… (optional)'
                              : 'Deposit into… (optional)'}
                        </option>
                        {settlementAccounts.map((a) => (
                          <option key={a.id} value={a.id}>
                            {a.name} — {a.currency} {a.balance.toLocaleString('en-AU', { maximumFractionDigits: 2 })}
                          </option>
                        ))}
                      </select>
                      <span style={{ fontSize: 12, color: '#666', alignSelf: 'center' }}>
                        {cashAccountId
                          ? 'Cash will move automatically when this is saved.'
                          : 'Leave blank to record it without touching cash.'}
                      </span>
                    </div>
                  )}

                  <div className="form-group">
                    {/* A dividend is paid, not traded — there is no brokerage on it. */}
                    {transactionType !== 'dividend' && (
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
                    )}
                    <input
                      type="text"
                      value={notes}
                      onChange={(e) => setNotes(e.target.value)}
                      placeholder="Notes (optional)"
                      className="symbol-input"
                      disabled={loading}
                    />
                    {/* These four are symbol-level, not per-transaction: they
                        describe the position you are opening and its exit plan.
                        A sale or a dividend has nothing to say about either. */}
                    {transactionType === 'purchase' && (
                      <>
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
                      </>
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
                </div>
              </div>
              <div className="modal-footer">
                {/* Errors belong here rather than on the page behind the
                    overlay, and in the footer they stay visible however far
                    the body is scrolled. */}
                {error && (
                  <span style={{ color: '#c62828', fontSize: 13, marginRight: 'auto', alignSelf: 'center' }}>
                    ❌ {error}
                  </span>
                )}
                <button type="button" className="btn btn-outline" onClick={closeRecordDialog} disabled={loading}>
                  Cancel
                </button>
                <button type="submit" className="btn btn-primary" disabled={loading}>
                  {loading ? 'Saving...' : 'Record Transaction'}
                </button>
              </div>
            </form>
          </div>
        </div>
      )}

      {error && !showRecordDialog && <div className="alert alert-error">❌ {error}</div>}
      {success && <div className="alert alert-success">✓ {success}</div>}

      {/* One map per section, so each is scaled to its own holdings: a single
          map across the lot sizes every tile against the largest position in
          the portfolio, which leaves a small section unreadable. The map of
          everything lives on the Dashboard. An empty section is skipped rather
          than shown as an empty frame. */}
      {scopeSections.map((section) => {
        const rows = scopedHoldings.filter((h) => sectionOf(h.is_etf, h.is_international) === section)
        if (rows.length === 0) return null
        return (
          // The collapse id is the section name, not the heading, so rewording
          // the heading cannot reopen a card the user had folded away.
          <CollapsibleCard key={section} id={`heatmap-${section}`} title={`${section} Heat Map`}>
            {/* Clicking a tile drives the chart card below rather than opening
                anything of its own — the map is a way into a position. */}
            <HoldingsHeatMap holdings={rows} onSelectSymbol={setSelectedChartSymbol} />
          </CollapsibleCard>
        )
      })}

      {selectedChartSymbol && (
        <div className="manager-card chart-card">
          <div className="card-header">
            <h2>Stock Chart</h2>
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
            purchases={getRemainingPurchaseLots(transactions, selectedChartSymbol)}
                currentPrice={sel?.native_current_price ?? null}
                currentVolume={sel?.volume ?? null}
                currentPriceDate={sel?.price_date ?? null}
                markerPrice={sel?.stop_loss ?? null}
                markerLabel={sel?.is_trailing_sell ? 'Trailing Sell' : 'Stop Loss'}
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
        ) : scopedHoldings.length === 0 ? (
          // Scoped, not "no transactions at all": with the portfolio split over
          // two screens, a full portfolio of the other kind would otherwise
          // render this one as empty section groups above an empty table, with
          // nothing saying why.
          <p className="empty-text">
            {transactions.length === 0
              ? 'No holdings configured.'
              : `No ${SCOPE_NOUN[scope]} holdings — the other Holdings screen has them.`}
          </p>
        ) : (
          <div style={{ maxHeight: 600, overflowY: 'auto' }}>
            {(() => {
              const renderCard = (item: typeof summary[0]) => {
                const symCurrency = item.currency
                const isForeign = item.isInternational
                const isSelected = selectedChartSymbol === item.symbol
                const price = priceParts(item.currentPrice, item.nativePrice, symCurrency, nativeFirst && isForeign)
                const sma = nativeFirst && isForeign && item.nativeSma150 != null
                  ? { value: item.nativeSma150, text: `${symCurrency} ${item.nativeSma150.toFixed(2)}` }
                  : { value: item.sma150, text: item.sma150 !== null ? `$${item.sma150.toFixed(2)}` : '' }
                // `A$` only where it earns its keep: on the International
                // screen the prices above these totals are in the stock's own
                // currency, so a bare `$` would not say which is which.
                const aud = nativeFirst ? 'A$' : '$'
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
                    {item.shares % 1 === 0 ? item.shares.toFixed(0) : item.shares.toFixed(2)}@{price.main}
                    {isForeign && price.aside && (
                      <span style={{ fontSize: 11, color: '#888', marginLeft: 6 }}>
                        {price.aside}
                      </span>
                    )}
                    {item.priceSource === 'manual' && <span style={{ fontSize: 11, marginLeft: 4 }}>(manual)</span>}
                  </div>
                  {item.change !== null && item.changePercent !== null && (
                    <div style={{ color: item.change >= 0 ? '#4caf50' : '#f44336', fontSize: 12 }}>
                      {item.change >= 0 ? '+' : ''}{item.change.toFixed(2)} ({item.change >= 0 ? '+' : ''}{item.changePercent.toFixed(2)}%)
                    </div>
                  )}
                  {/* The average is read against the price above it, so it is
                      quoted in whichever currency that price is in. Comparing
                      the two AUD figures keeps the colour right even when one
                      of the native ones is missing. */}
                  {sma.value !== null && (
                    <div style={{ color: item.currentPrice !== null && item.sma150 !== null && item.sma150 > item.currentPrice ? '#f44336' : undefined }}>
                      150SMA: {sma.text}
                    </div>
                  )}
                  <div>Current value: {aud}{item.currentValue.toFixed(2)}</div>
                  <div>Dividends: {aud}{item.dividends.toFixed(2)}</div>
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
                        {/* A baseline replaces the purchase as the basis, so
                            there is one figure — labelled with the period it
                            actually covers rather than left to be assumed. */}
                        P/L{item.basisDate ? ` since ${item.basisDate}` : ''}: {pl >= 0 ? '+' : '-'}${Math.abs(pl).toFixed(2)}
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
                  <div key={label} style={{ marginBottom: 24 }}>
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
                  {scopeSections.map((section) =>
                    renderGroup(
                      summary.filter((i) => sectionOf(i.isEtf, i.isInternational) === section),
                      section,
                    ),
                  )}
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
                    <th>{nativeFirst ? 'Price' : 'Price (AUD)'}</th>
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
                          {(() => {
                            // The transaction's own currency, not the symbol's:
                            // a foreign stock can have been bought in AUD.
                            const paid = tx.currency !== 'AUD' ? tx.original_price : null
                            const price = priceParts(tx.price, paid, tx.currency, nativeFirst)
                            return (
                              <>
                                {price.main}
                                {price.aside && (
                                  <span style={{ fontSize: 10, color: '#888', marginLeft: 4 }}>{price.aside}</span>
                                )}
                              </>
                            )
                          })()}
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
        <div className="modal-overlay"
          onClick={() => setEditingSymbolCard(null)}>
          <div className="modal-card" style={{ minWidth: 320 }}
            onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3 style={{ margin: 0 }}>Edit {editingSymbolCard}</h3>
            </div>
            <div className="modal-body">
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
                <label style={{ fontSize: 13, color: '#666' }}>Track P/L from</label>
                <input
                  type="date"
                  value={editCardFields['pl_basis_date'] ?? ''}
                  onChange={(e) => setEditCardFields((prev) => ({ ...prev, pl_basis_date: e.target.value }))}
                  className="symbol-input"
                  style={{ width: '100%' }}
                  title="Measure profit and loss from this date instead of the original purchase"
                />
                <span style={{ fontSize: 11, color: '#666' }}>
                  {editCardFields['pl_basis_price']
                    ? `Baseline price $${parseFloat(editCardFields['pl_basis_price']).toFixed(2)} — the transaction record is unchanged.`
                    : 'Optional. For long-held stocks whose original cost no longer says anything useful.'}
                </span>
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
            </div>
            <div className="modal-footer">
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
                    // The baseline price is resolved server-side from the date;
                    // sending back the copy we were shown would assert a value
                    // the client has no business deciding.
                    const { pl_basis_price: _shownBasisPrice, ...fields } = editCardFields
                    const basisChanged =
                      (editCardFields['pl_basis_date'] ?? '') !== (holdingsSymbolFields[oldSymbol]?.['pl_basis_date'] ?? '')
                    await apiClient.updateHoldingsSymbolFields(targetSymbol, editCardNotes.trim() || null, fields)
                    setEditingSymbolCard(null)
                    if (basisChanged && !symbolChanged) {
                      // The rebased figures are server-computed, so the local
                      // field patch below cannot produce them.
                      await loadHoldings()
                    }
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
