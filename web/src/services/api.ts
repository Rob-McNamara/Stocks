const API_BASE_URL = import.meta.env.VITE_API_URL ?? 'http://localhost:3001/api/v1'
/**
 * Compiled into the JavaScript bundle, so anyone who can load the page can
 * read it. It keeps other devices on the network from calling the API
 * directly; it is not a secret from the browser. Serve the page only where
 * you'd be happy for its users to hold the token — localhost, by default
 * (`vite` without `--host`).
 */
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
  /** Null on bars stored before OHLC ingest existed — clients must tolerate it. */
  open: number | null
  high: number | null
  low: number | null
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
  /** Cash account this trade settles against, when it has one. */
  cash_account_id: number | null
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
  /** The side of the comparison `diff` is measured from — equals `price`
   *  unless the list compares volume. Absent from an API older than the
   *  compare field, where the comparison was always the price. */
  compare_value?: number
  field_value: number
  diff: number
  pct_diff: number
  currency: string | null
  /** True when field_value is a trailing-sell trigger rather than a manual stop loss */
  is_trailing: boolean
  /** Which table the symbol came from, so a list spanning both sends each row
   *  to the right screen. Absent from an API older than indicator lists, where
   *  every row in a list shared the list's own `field_source`. */
  origin?: 'holdings' | 'watchlist'
  /** Trading days since the crossing. Only a crossover list reports one. */
  days?: number | null
  /** Volume on the crossing day vs the preceding 20-day average, in percent. */
  volume_cross_pct?: number | null
}

export interface CustomListResult {
  key: string
  label: string
  source: string
  /** Where the entry symbols live: 'holdings' or 'watchlist' — drives navigation */
  field_source: string
  operator: string
  /** What the field is compared against: 'price' or 'volume'. */
  compare: string
  /** The column the rows are ranked by: 'pct_diff' (the default and the only
   *  value an older API sends), 'days' or 'volume_cross_pct'. */
  metric?: string
  field_label: string
  /** Direction the server actually ranked by (request override, else config). */
  sort: 'asc' | 'desc'
  /** True when more rows qualified than `limit` allowed through. */
  truncated: boolean
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
  /** 40-week EMA over weekly closes, in the symbol's own currency */
  ema40w: number | null
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

/** The periods a holding's price move is reported over, for the heat map. */
export type MovePeriod = '1w' | '1m' | '3m' | '1y' | '2y'

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
  /** A price exists but no FX rate does, so the AUD figures are absent. */
  fx_missing?: boolean
  price_date: string | null
  change: number | null
  change_percent: number | null
  volume: number | null
  /**
   * Price move over each heat map period, in percent: from the last close on
   * or before the period's start to the quote, in the stock's own currency —
   * the basis of `change_percent`. Null where the history is too short.
   * Optional because an API older than the periods omits it outright.
   */
  moves?: Partial<Record<MovePeriod, number | null>>
  current_value: number
  dividends: number
  pl: number
  pl_pct: number | null
  /**
   * A foreign holding's P/L by source, in AUD: what the stock's own price did
   * (its P/L in its own currency, at today's rate) and what the exchange rate
   * did since each purchase. With `dividends` they add up to `pl`. Null for a
   * holding bought in AUD and for one measured from a baseline; optional
   * because an older API omits them.
   */
  pl_price?: number | null
  pl_currency?: number | null
  sma50: number | null
  native_sma50: number | null
  sma150: number | null
  /** The same 150-day average in the symbol's own currency */
  native_sma150: number | null
  ema40w: number | null
  native_ema40w: number | null
  /** Today's money for this holding: shares × the quote's change, in AUD */
  day_pl: number | null
  /** Effective stop loss in native currency: manual field, or the trailing-sell trigger */
  stop_loss: number | null
  is_trailing_sell: boolean
  /**
   * Set when the figures above are measured from a baseline date rather than
   * from the purchase — for holdings old enough that their original cost is a
   * record rather than a useful denominator. `invested`, `avg_cost`,
   * `dividends`, `pl` and `pl_pct` all follow the baseline when this is set.
   *
   * Optional rather than nullable: an API older than the baseline omits these
   * outright, and a strict null check would sail past `undefined` into a crash.
   */
  basis_date?: string | null
  /** Close on the first trading day from `basis_date`, in native currency. */
  basis_price?: number | null
}

export interface RiskRow {
  symbol: string
  currency: string
  /** The stock's trading currency — what a manual stop loss is entered in. */
  native_currency?: string
  current_price: number | null
  purchase_price: number | null
  pl_pct: number | null
  stop_loss: number | null
  is_trailing_sell: boolean
  stop_loss_pct: number | null
  stop_loss_dollar: number | null
  sma50: number | null
  sma150: number | null
  /** 40-week exponential moving average, over weekly closes. */
  ema40w: number | null
  high30d: number | null
  total_invested: number
  /** Shares still held, for position-level dollar figures. */
  shares: number
}

export interface CashAccount {
  id: number
  name: string
  currency: string
  /** Nominal annual rate, for display only — interest is recorded as credited. */
  interest_rate: number | null
  /** Whether this account counts toward portfolio value and growth. */
  include_in_portfolio: boolean
  notes: string | null
  created_at: string
  /** Balance in the account's own currency. */
  balance: number
  /** The same balance in AUD, or null when no rate is stored for that currency. */
  balance_aud: number | null
  transaction_count: number
}

export interface CashTransaction {
  id: number
  account_id: number
  account_name: string
  currency: string
  date: string
  /** Signed, in the account's currency: positive is money in. */
  amount: number
  kind: string
  /** Set when this entry was written by the trade it settles. */
  holding_tx_id: number | null
  /** Pairs the two legs of a currency conversion. */
  transfer_group_id: string | null
  notes: string | null
  created_at: string
}

export interface PortfolioHistoryPoint {
  date: string
  /** Market value of shares held, in AUD. */
  stocks: number
  /** Cash across accounts counted toward the portfolio, in AUD. */
  cash: number
  total: number
  /** Money in (+) or out (−) that day. Contributions only, never earnings. */
  flow: number
}

export interface PortfolioHistorySummary {
  start_date: string | null
  end_date: string | null
  /** Value carried into the window, before any of its contributions. */
  opening_value: number
  end_value: number
  net_contributions: number
  /** end_value − opening_value − net_contributions. */
  gain: number
  /** Time-weighted return as a percentage; null until there is capital. */
  twr_pct: number | null
}

export interface PortfolioHistory {
  series: PortfolioHistoryPoint[]
  summary: PortfolioHistorySummary
  /**
   * Earliest date the server will report, when a floor is configured. Optional
   * rather than nullable: an API older than the setting omits it entirely.
   */
  start_floor?: string | null
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

/**
 * Why a hindsight price point has no number. The three empty cases mean
 * different things: `pending` fills in on its own, `delisted` never will, and
 * `no_data` is a gap in the price history.
 */
export type HindsightStatus = 'ok' | 'pending' | 'delisted' | 'no_data'

export interface HindsightPoint {
  value: number | null
  /** The bar actually used, which for a weekend target is the trading day before it. */
  date: string | null
  status: HindsightStatus
  /** Native-currency difference against the sale, across the shares sold. */
  delta: number | null
  pct: number | null
}

/** One sale, priced at six later moments. All figures native — never AUD. */
export interface HindsightRow {
  symbol: string
  currency: string
  sale_date: string
  quantity: number
  sale_price: number
  purchase_price: number | null
  realised_pct: number | null
  delisted_on: string | null
  points: {
    week1: HindsightPoint
    week6: HindsightPoint
    month3: HindsightPoint
    peak: HindsightPoint
    low: HindsightPoint
    current: HindsightPoint
  }
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
  /** Settlement account the trade draws on; null for dividend events and unlinked trades */
  cash_account_id: number | null
  /** Tax withheld from a dividend, in its currency; null when not recorded */
  withholding_amount?: number | null
  /** A dividend's franked amount and franking credit (AUD) from its statement */
  franked_amount?: number | null
  franking_credit?: number | null
  /** A dividend's type from its statement: Interim, Final, Special… */
  dividend_type?: string | null
  /**
   * Set on rows a corporate action stands for (a merger's two sides). They are
   * worked out, not stored: change or delete the action, not the row.
   */
  corporate_action?: CorporateActionLink | null
  /** For a parcel a merger handed on with roll-over: when the original shares were bought */
  acquired_date?: string | null
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
  /** Sold positions are priced on their own schedule; Hindsight reads this. */
  sold_prices_updated_at?: string | null
  /** Stamped by the price daemon's daily close run */
  daily_prices_updated_at: string | null
  last_full_refresh_at: string | null
  server_time: string
}

/** Periods the sector rotation map measures, shortest first. */
export type RotationPeriod = '1d' | '1w' | '1m' | '3m'

/** One figure per period, in percent; null where there was no price to measure from. */
export type RotationReturns = Record<RotationPeriod, number | null>

export interface SectorRotationEntry {
  symbol: string
  /** A name from the app's sector list. */
  sector: string
  long_name: string | null
  /** Newest stored close. Before the set's `as_of`, the returns are all null. */
  last_date: string | null
  returns: RotationReturns
  /** `returns` minus the benchmark's, in percentage points. */
  relative: RotationReturns
}

/** One set of sectors as GET /api/sector-rotation measures it. */
export interface SectorRotationSet {
  name: string
  /** The day every figure in the set runs to: the benchmark's newest close. */
  as_of: string | null
  benchmark: {
    symbol: string
    name: string
    long_name: string | null
    last_date: string | null
    returns: RotationReturns
  }
  sectors: SectorRotationEntry[]
}

/** One of a fund's top holdings, with its moves over each period. */
export interface SectorHolding {
  /** Null for a position Yahoo gives no ticker for, such as cash. */
  symbol: string | null
  name: string
  /** Percent of the fund. */
  weight: number
  last_date: string | null
  returns: RotationReturns
  /** `returns` minus the set's benchmark's, in percentage points. */
  relative: RotationReturns
}

/** GET /api/sector-rotation/holdings: a sector tile's fund, opened up. */
export interface SectorHoldings {
  set: string
  symbol: string
  /** The sector's name, or the benchmark's. */
  name: string
  as_of: string | null
  benchmark: { symbol: string; name: string }
  /** How much of the fund the listed holdings make up, in percent. */
  top_weight: number
  holdings: SectorHolding[]
}

/** One set as stored in the `sector_rotation` config value. */
export interface SectorRotationSetConfig {
  name: string
  benchmark: { symbol: string; name: string }
  sectors: { symbol: string; sector: string }[]
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
  /**
   * Cash account this trade settles against. Omit to record the trade without
   * touching the cash ledger — which is how every pre-ledger trade is stored.
   * The account's currency must match the trade's.
   */
  cash_account_id?: number | null
  /**
   * A dividend's tax figures from its statement. Omit to keep what is stored;
   * null clears it.
   */
  withholding_amount?: number | null
  franked_amount?: number | null
  franking_credit?: number | null
  /** A dividend's payment date (YYYY-MM-DD) and type. Omit to keep; null clears. */
  payment_date?: string | null
  dividend_type?: string | null
  /** Acknowledge an over-sell warning (server responds 409 otherwise) */
  confirm?: boolean
}

/**
 * Download an account's ledger as CSV, saved under the server's filename.
 *
 * Fetched rather than linked: a plain link cannot carry the Authorization
 * header, so with API_TOKEN set the download came back 401. The running
 * balance is still computed and ordered by the server.
 */
export async function downloadCashAccountCsv(accountId: number): Promise<void> {
  const response = await apiFetch(`${API_BASE_URL}/cash/accounts/${accountId}/transactions.csv`)
  if (!response.ok) {
    throw new Error((await apiErrorMessage(response)) || 'Failed to export the account')
  }
  const disposition = response.headers.get('Content-Disposition') ?? ''
  const filename = /filename="([^"]+)"/.exec(disposition)?.[1] ?? `cash-account-${accountId}.csv`
  const url = URL.createObjectURL(await response.blob())
  const link = document.createElement('a')
  link.href = url
  link.download = filename
  document.body.appendChild(link)
  link.click()
  link.remove()
  // Revoked after a beat: Safari starts the download asynchronously and fails
  // it if the URL is already gone.
  setTimeout(() => URL.revokeObjectURL(url), 1000)
}

// ---------------------------------------------------------------------------
// Tax report
// ---------------------------------------------------------------------------

export const TAX_SECTION_KEYS = ['checks', 'cgt', 'dividends', 'foreign', 'interest', 'trusts', 'deductions', 'planning'] as const
export type TaxSectionKey = (typeof TAX_SECTION_KEYS)[number]

export interface TaxYearOption {
  /** The year the income year ends in: 2026 is 2025–26 */
  fy: number
  label: string
  start: string
  end: string
  in_progress: boolean
}

/** 'entry' is a blank box for the user to fill in; its cells are always null. */
export type TaxColumnKind = 'text' | 'date' | 'quantity' | 'money' | 'price' | 'rate' | 'integer' | 'entry'

export interface TaxTable {
  title: string
  columns: { label: string; kind: TaxColumnKind }[]
  rows: (string | number | null)[][]
}

export interface TaxSummaryLine {
  /** The return label, such as "18A"; null for a working figure */
  item: string | null
  label: string
  amount: number | null
  /** A non-numeric answer, such as "Yes" */
  value: string | null
  note: string | null
}

export interface TaxReportSection {
  key: string
  title: string
  summary: TaxSummaryLine[]
  tables: TaxTable[]
  notes: string[]
  /** The section as plain text, ready to copy */
  text: string
}

export interface TaxCheck {
  severity: 'warn' | 'info'
  section: string
  message: string
}

export type IncomeKind = 'company' | 'trust' | 'foreign'

export interface SymbolTaxSettings {
  symbol: string
  name: string | null
  currency: string
  income_kind: IncomeKind
  /** false when the income type was worked out rather than chosen */
  income_kind_set: boolean
  franking_pct: number | null
  withholding_pct: number | null
  /** The user's share of the holding, 0–100; null is all of it. 0 leaves it out. */
  share_pct: number | null
}

export interface AccountTaxSettings {
  id: number
  name: string
  currency: string
  /** The user's share of the account, 0–100; null is all of it. */
  share_pct: number | null
  /** Interest credited in the year (AUD), before the share is applied */
  interest_aud: number
}

/** A symbol or cash account set to less than 100% the user's. */
export interface PartialOwnership {
  kind: 'symbol' | 'account'
  name: string
  share_pct: number
}

/** A trust's annual tax statement, AUD, keyed by return label. */
export interface TaxStatementFigures {
  non_primary_income?: number | null
  franked_distributions?: number | null
  franking_credits?: number | null
  tfn_withheld?: number | null
  total_capital_gains?: number | null
  net_capital_gain?: number | null
  assessable_foreign_income?: number | null
  other_net_foreign_income?: number | null
  foreign_tax_offset?: number | null
  amit_increase?: number | null
  amit_decrease?: number | null
  notes?: string | null
}

export interface TaxStatement extends TaxStatementFigures {
  symbol: string
  fy: number
  updated_at: string
}

export interface TaxStatementRow {
  symbol: string
  name: string | null
  /** Distributions assessed in the year, AUD */
  cash_aud: number
  distributions: number
  statement: TaxStatement | null
}

export interface TaxReport {
  fy: number
  label: string
  start: string
  end: string
  entity: string
  generated_at: string
  in_progress: boolean
  detail: 'full' | 'summary'
  last_exported_at: string | null
  losses_carried_in: number
  losses_carried_in_source: 'computed' | 'entered'
  included: TaxSectionKey[]
  checks: TaxCheck[]
  sections: TaxReportSection[]
  symbols: SymbolTaxSettings[]
  statements: TaxStatementRow[]
  accounts: AccountTaxSettings[]
  /** What is left out or counted only in part, because it isn't all the user's */
  ownership: string[]
  /** What the user still has to fill in: tables with blank 'entry' columns */
  worksheet: TaxTable[]
  /** The whole report as plain text, ready to copy */
  text: string
}

export interface TaxReportParams {
  fy: number
  sections: TaxSectionKey[]
  detail: 'full' | 'summary'
  /** Net capital losses carried in; omit to have them worked out */
  lossesCarriedIn?: number | null
}

function taxReportUrl(params: TaxReportParams, format: 'json' | 'csv'): string {
  const qs = new URLSearchParams({
    fy: String(params.fy),
    sections: params.sections.join(','),
    detail: params.detail,
    format,
  })
  if (params.lossesCarriedIn != null) qs.set('losses_carried_in', String(params.lossesCarriedIn))
  return `${API_BASE_URL}/tax/report?${qs.toString()}`
}

/**
 * Download a year's tax report as CSV, saved under the server's filename.
 * Fetched rather than linked, for the same reason as the cash export: a link
 * cannot carry the bearer token.
 */
export async function downloadTaxReportCsv(params: TaxReportParams): Promise<void> {
  const response = await apiFetch(taxReportUrl(params, 'csv'))
  if (!response.ok) {
    throw new Error((await apiErrorMessage(response)) || 'Failed to download the tax report')
  }
  const disposition = response.headers.get('Content-Disposition') ?? ''
  const filename = /filename="([^"]+)"/.exec(disposition)?.[1] ?? `tax-report-${params.fy}.csv`
  const url = URL.createObjectURL(await response.blob())
  const link = document.createElement('a')
  link.href = url
  link.download = filename
  document.body.appendChild(link)
  link.click()
  link.remove()
  // Revoked after a beat: Safari starts the download asynchronously.
  setTimeout(() => URL.revokeObjectURL(url), 1000)
}

// ---------------------------------------------------------------------------
// Corporate actions
// ---------------------------------------------------------------------------

export interface CorporateActionLink {
  id: number
  kind: string
  /** The other side: the symbol merged into, or merged from */
  counterpart: string
  rollover: boolean
}

export interface CorporateAction {
  id: number
  kind: 'merger'
  /** The implementation date */
  date: string
  from_symbol: string
  to_symbol: string
  from_quantity: number
  to_quantity: number
  /** Scrip-for-scrip roll-over: cost bases and purchase dates carry over */
  rollover: boolean
  /** Without roll-over: the AUD value of the shares received */
  market_value: number | null
  notes: string | null
  created_at: string
}

export interface CorporateActionPayload {
  kind?: 'merger'
  date: string
  from_symbol: string
  to_symbol: string
  from_quantity: number
  to_quantity: number
  rollover?: boolean
  market_value?: number | null
  notes?: string | null
}

export interface MergerPlan {
  /** Shares of the old company held on the date */
  held: number
  parcels: { purchase_id: number; acquired: string; given_up: number; received: number; cost: number; native_cost: number }[]
  cost: number
  currency: string
}

// ---------------------------------------------------------------------------
// Dividend import
// ---------------------------------------------------------------------------

export interface DividendImportChange {
  field: string
  from: string | null
  to: string | null
}

export interface DividendImportRow {
  /** The file's line number; the header is line 1 */
  line: number
  symbol: string
  ex_date: string
  status: 'update' | 'create' | 'unchanged' | 'error'
  transaction_id: number | null
  /** The recorded dividend's ex-date, when the file corrects it */
  matched_date: string | null
  changes: DividendImportChange[]
  warnings: string[]
  error: string | null
  /** Ticked in the preview: a clean update or a new dividend */
  selected: boolean
  applied: boolean
}

export interface DividendImportResult {
  applied: boolean
  updated: number
  created: number
  counts: { update: number; create: number; unchanged: number; error: number }
  rows: DividendImportRow[]
}

export interface DividendImportOptions {
  /** A statement total lower than recorded is tax withheld: keep the gross, record the gap */
  shortfall_is_withholding: boolean
}

export interface ChartDrawing {
  id: number
  symbol: string
  kind: 'horizontal' | 'trend'
  /** In the symbol's own currency — the chart applies the FX rate at render. */
  price: number
  label: string | null
  colour: string | null
  /** Trendlines only: anchors are (start_date, price) and (end_date, end_price). */
  start_date: string | null
  end_date: string | null
  end_price: number | null
  created_at: string
}

export interface NewTrendline {
  startDate: string
  startPrice: number
  endDate: string
  endPrice: number
  label?: string
}

/**
 * The server refused a sale of more shares than are held. Re-sending the same
 * payload with `confirm: true` records it anyway.
 */
export class OversellError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'OversellError'
  }
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

  /**
   * Per-domain last-modified stamps — poll this instead of refetching payloads.
   * Currently unused by the web app (it refetches on navigation); kept for
   * parity with the mobile client, which polls it to decide when to sync.
   */
  async getSyncState(): Promise<SyncState> {
    const response = await apiFetch(`${API_BASE_URL}/sync-state`)
    if (!response.ok) throw new Error('Failed to fetch sync state')
    return response.json()
  },

  /**
   * @param listSort Per-list sort direction keyed by list key. Sent to the
   *   server rather than applied here because each list is ranked and then
   *   truncated to its limit server-side — reordering in the browser would only
   *   reverse the rows that survived the cut.
   */
  async getPortfolioOverview(listSort?: Record<string, 'asc' | 'desc'>): Promise<PortfolioOverview> {
    const pairs = Object.entries(listSort ?? {}).map(([key, dir]) => `${key}:${dir}`)
    const qs = pairs.length > 0 ? `?list_sort=${encodeURIComponent(pairs.join(','))}` : ''
    const response = await apiFetch(`${API_BASE_URL}/portfolio/overview${qs}`)
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

  /** Every configured sector set, measured against its benchmark. */
  async getSectorRotation(): Promise<{ sets: SectorRotationSet[] }> {
    const response = await apiFetch(`${API_BASE_URL}/sector-rotation`)
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to fetch sector rotation')
    }
    return response.json()
  },

  /** A sector tile's fund: its top holdings, and how each one moved. */
  async getSectorHoldings(set: string, symbol: string): Promise<SectorHoldings> {
    const qs = `?set=${encodeURIComponent(set)}&symbol=${encodeURIComponent(symbol)}`
    const response = await apiFetch(`${API_BASE_URL}/sector-rotation/holdings${qs}`)
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to fetch the holdings')
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

  async getChartDrawings(symbol: string): Promise<ChartDrawing[]> {
    const response = await apiFetch(`${API_BASE_URL}/chart-drawings/${encodeURIComponent(symbol)}`)
    if (!response.ok) return []
    return (await response.json()).drawings ?? []
  },

  async addChartDrawing(symbol: string, price: number, label?: string): Promise<ChartDrawing[]> {
    const response = await apiFetch(`${API_BASE_URL}/chart-drawings/${encodeURIComponent(symbol)}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ kind: 'horizontal', price, label: label ?? null }),
    })
    if (!response.ok) {
      throw new Error((await apiErrorMessage(response)) || 'Failed to save the price level')
    }
    return (await response.json()).drawings ?? []
  },

  async addTrendline(symbol: string, line: NewTrendline): Promise<ChartDrawing[]> {
    const response = await apiFetch(`${API_BASE_URL}/chart-drawings/${encodeURIComponent(symbol)}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        kind: 'trend',
        price: line.startPrice,
        start_date: line.startDate,
        end_date: line.endDate,
        end_price: line.endPrice,
        label: line.label ?? null,
      }),
    })
    if (!response.ok) {
      throw new Error((await apiErrorMessage(response)) || 'Failed to save the trendline')
    }
    return (await response.json()).drawings ?? []
  },

  /** Move a horizontal level to a new native price; returns the symbol's drawings. */
  async moveChartDrawing(id: number, price: number): Promise<ChartDrawing[]> {
    const response = await apiFetch(`${API_BASE_URL}/chart-drawings/id/${id}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ price }),
    })
    if (!response.ok) {
      throw new Error((await apiErrorMessage(response)) || 'Failed to move the price level')
    }
    return (await response.json()).drawings ?? []
  },

  /** Move a trendline by restating both anchors (native prices, oldest first). */
  async moveTrendline(id: number, line: Omit<NewTrendline, 'label'>): Promise<ChartDrawing[]> {
    const response = await apiFetch(`${API_BASE_URL}/chart-drawings/id/${id}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        price: line.startPrice,
        start_date: line.startDate,
        end_date: line.endDate,
        end_price: line.endPrice,
      }),
    })
    if (!response.ok) {
      throw new Error((await apiErrorMessage(response)) || 'Failed to move the trendline')
    }
    return (await response.json()).drawings ?? []
  },

  async deleteChartDrawing(id: number): Promise<void> {
    const response = await apiFetch(`${API_BASE_URL}/chart-drawings/id/${id}`, { method: 'DELETE' })
    if (!response.ok) {
      throw new Error((await apiErrorMessage(response)) || 'Failed to remove the price level')
    }
  },

  async getCashAccounts(): Promise<CashAccount[]> {
    const response = await apiFetch(`${API_BASE_URL}/cash/accounts`)
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || 'Failed to fetch cash accounts')
    return response.json()
  },

  async addCashAccount(payload: {
    name: string
    currency: string
    interest_rate?: number
    include_in_portfolio?: boolean
    notes?: string
  }): Promise<{ id: number }> {
    const response = await apiFetch(`${API_BASE_URL}/cash/accounts`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    })
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || 'Failed to add cash account')
    return response.json()
  },

  async deleteCashAccount(id: number): Promise<void> {
    const response = await apiFetch(`${API_BASE_URL}/cash/accounts/${id}`, { method: 'DELETE' })
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || 'Failed to delete cash account')
  },

  async getCashTransactions(accountId?: number): Promise<CashTransaction[]> {
    const qs = accountId ? `?account_id=${accountId}` : ''
    const response = await apiFetch(`${API_BASE_URL}/cash/transactions${qs}`)
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || 'Failed to fetch cash transactions')
    return response.json()
  },

  async addCashTransaction(payload: {
    account_id: number
    date: string
    amount: number
    kind: string
    notes?: string
  }): Promise<{ id: number }> {
    const response = await apiFetch(`${API_BASE_URL}/cash/transactions`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    })
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || 'Failed to record cash transaction')
    return response.json()
  },

  async updateCashTransaction(
    id: number,
    payload: { account_id: number; date: string; amount: number; kind: string; notes?: string },
  ): Promise<{ id: number }> {
    const response = await apiFetch(`${API_BASE_URL}/cash/transactions/${id}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    })
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || 'Failed to update cash transaction')
    return response.json()
  },

  async deleteCashTransaction(id: number): Promise<void> {
    const response = await apiFetch(`${API_BASE_URL}/cash/transactions/${id}`, { method: 'DELETE' })
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || 'Failed to delete cash transaction')
  },

  /** Records both legs of a move between accounts, including across currencies. */
  async addCashTransfer(payload: {
    from_account_id: number
    to_account_id: number
    date: string
    from_amount: number
    to_amount: number
    notes?: string
  }): Promise<{ transfer_group_id: string; implied_rate: number }> {
    const response = await apiFetch(`${API_BASE_URL}/cash/transfer`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    })
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || 'Failed to record transfer')
    return response.json()
  },

  /**
   * Daily portfolio value and time-weighted return.
   * @param from Optional YYYY-MM-DD start; omit for the full history.
   */
  async getPortfolioHistory(from?: string): Promise<PortfolioHistory> {
    const qs = from ? `?from=${encodeURIComponent(from)}` : ''
    const response = await apiFetch(`${API_BASE_URL}/portfolio/history${qs}`)
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to fetch portfolio history')
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

  async getHindsight(): Promise<HindsightRow[]> {
    const response = await apiFetch(`${API_BASE_URL}/hindsight`)
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to fetch hindsight')
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
  async getTaxYears(): Promise<{ years: TaxYearOption[]; default_fy: number; ownership?: PartialOwnership[] }> {
    const response = await apiFetch(`${API_BASE_URL}/tax/years`)
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || 'Failed to load tax years')
    return response.json()
  },

  async getTaxReport(params: TaxReportParams): Promise<TaxReport> {
    const response = await apiFetch(taxReportUrl(params, 'json'))
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || 'Failed to generate the tax report')
    return response.json()
  },

  /** Absent fields are left alone; null clears one (an income type returns to the guess). */
  async updateSymbolTaxSettings(
    symbol: string,
    payload: {
      income_kind?: IncomeKind | null
      franking_pct?: number | null
      dividend_withholding_pct?: number | null
      /** 0–100; 0 leaves the symbol out of the report, null makes it all the user's */
      tax_share_pct?: number | null
    },
  ): Promise<void> {
    const response = await apiFetch(`${API_BASE_URL}/tax/symbols/${encodeURIComponent(symbol)}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    })
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || `Failed to save tax settings for ${symbol}`)
  },

  /** The user's share of a cash account's interest and fees; null makes it all theirs. */
  async updateAccountTaxShare(accountId: number, sharePct: number | null): Promise<void> {
    const response = await apiFetch(`${API_BASE_URL}/tax/accounts/${accountId}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ tax_share_pct: sharePct }),
    })
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || 'Failed to save the account share')
  },

  async saveTaxStatement(symbol: string, fy: number, figures: TaxStatementFigures): Promise<TaxStatement> {
    const response = await apiFetch(`${API_BASE_URL}/tax/statements/${encodeURIComponent(symbol)}/${fy}`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(figures),
    })
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || `Failed to save the ${symbol} statement`)
    return response.json()
  },

  async deleteTaxStatement(symbol: string, fy: number): Promise<void> {
    const response = await apiFetch(`${API_BASE_URL}/tax/statements/${encodeURIComponent(symbol)}/${fy}`, { method: 'DELETE' })
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || `Failed to delete the ${symbol} statement`)
  },

  /** Note that a year's report was copied or downloaded, so later edits to that year are flagged. */
  async recordTaxExport(fy: number): Promise<void> {
    const response = await apiFetch(`${API_BASE_URL}/tax/exports`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ fy }),
    })
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || 'Failed to record the export')
  },

  async getCorporateActions(): Promise<CorporateAction[]> {
    const response = await apiFetch(`${API_BASE_URL}/corporate-actions`)
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || 'Failed to load corporate actions')
    return response.json()
  },

  /** What recording the action would carry across, without recording it. */
  async previewCorporateAction(payload: CorporateActionPayload): Promise<{ action: CorporateAction; plan: MergerPlan }> {
    const response = await apiFetch(`${API_BASE_URL}/corporate-actions/preview`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    })
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || 'Failed to check the corporate action')
    return response.json()
  },

  async addCorporateAction(payload: CorporateActionPayload): Promise<{ action: CorporateAction; plan: MergerPlan }> {
    const response = await apiFetch(`${API_BASE_URL}/corporate-actions`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    })
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || 'Failed to record the corporate action')
    return response.json()
  },

  async deleteCorporateAction(id: number): Promise<void> {
    const response = await apiFetch(`${API_BASE_URL}/corporate-actions/${id}`, { method: 'DELETE' })
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || 'Failed to delete the corporate action')
  },

  /**
   * Preview a dividend CSV (nothing is written), or apply the chosen lines.
   * The server works the plan out afresh from the same file either way.
   */
  async importDividends(
    csv: string,
    options: DividendImportOptions,
    apply?: { lines: number[] },
  ): Promise<DividendImportResult> {
    const response = await apiFetch(`${API_BASE_URL}/dividends/import`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ csv, options, apply: apply !== undefined, lines: apply?.lines }),
    })
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || 'Failed to read the dividend file')
    return response.json()
  },

  async getTransactionsLedger(): Promise<{ rows: LedgerRow[] }> {
    const response = await apiFetch(`${API_BASE_URL}/transactions/ledger`)
    if (!response.ok) {
      const message = await apiErrorMessage(response)
      throw new Error(message || 'Failed to fetch transactions ledger')
    }
    return response.json()
  },

  /** Server-debounced full data refresh (watchlist + holdings prices, dividends). */
  async refreshAll(force = false): Promise<{ skipped: boolean; watchlist_prices?: number; holdings_prices?: number; sold_prices?: number; dividends_updated?: number; errors?: string[] }> {
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
      if (response.status === 409) throw new OversellError(message)
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
    // The API validates the JSON config keys and explains what it refused, so
    // that message is worth far more to the caller than a generic failure.
    if (!response.ok) throw new Error((await apiErrorMessage(response)) || 'Failed to update config')
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
