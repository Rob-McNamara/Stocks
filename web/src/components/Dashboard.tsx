import { useEffect, useMemo, useState } from 'react'
import { apiClient, type CashAccount, type PortfolioOverview, type CustomListEntry, type CustomListResult, type PortfolioHistory, type PortfolioHolding } from '../services/api'
import PortfolioHistoryChart from './PortfolioHistoryChart'
import HoldingsHeatMap from './HoldingsHeatMap'
import CollapsibleCard from './CollapsibleCard'

// Thin client: every number on this screen — totals, breakdowns, sectors,
// worst holdings, best watchlist and custom lists — comes pre-computed from
// GET /api/portfolio/overview.

/**
 * Windows offered for the portfolio value chart, as
 * `[value, label, months back]`. `null` months means the whole history.
 */
const HISTORY_RANGES = [
  ['3m', '3M', 3],
  ['6m', '6M', 6],
  ['12m', '12M', 12],
  ['2y', '2Y', 24],
  ['5y', '5Y', 60],
  ['all', 'All', null],
] as const

type HistoryRange = (typeof HISTORY_RANGES)[number][0]

/** Start date a range asks for, or undefined for the unbounded one. */
function rangeStart(range: HistoryRange): string | undefined {
  const months = HISTORY_RANGES.find(([value]) => value === range)?.[2]
  if (months == null) return undefined
  const cutoff = new Date()
  cutoff.setMonth(cutoff.getMonth() - months)
  return cutoff.toISOString().slice(0, 10)
}

/** Column heading for the quantity a custom list compares against its field. */
const compareLabel = (list: CustomListResult) => (list.compare === 'volume' ? 'Volume' : 'Price')

/** The same quantity in running prose, for the list's description line. */
const compareNoun = (list: CustomListResult) => (list.compare === 'volume' ? 'volume' : 'price')

/** What a list selects on, in a sentence. */
function listCriterion(list: CustomListResult): string {
  const subject = compareNoun(list)
  const field = list.field_label
  switch (list.operator) {
    case 'above': return `${subject} is above ${field}`
    case 'below': return `${subject} is below ${field}`
    case 'pct_below': return `${field} is % below ${subject}`
    case 'pct_above': return `${field} is % above ${subject}`
    case 'days_above': return `${subject} has been above ${field}, most recent first`
    case 'days_below': return `${subject} has been below ${field}, most recent first`
    case 'volume_cross_pct': return `${subject} crossed above ${field}, ranked by the volume behind it`
    default: return ''
  }
}

/**
 * The compared figure, falling back to the price. An API predating the compare
 * field sends no `compare_value` and only ever compared the price — without the
 * fallback the whole dashboard throws on the first row it renders.
 */
const compareValue = (entry: CustomListEntry) => entry.compare_value ?? entry.price

/**
 * Trailing columns for a custom list, in order, with the ranked one flagged.
 *
 * A crossover list is about *when* the price crossed and how much volume was
 * behind it, so the percentage gap it happens to sit at says nothing useful —
 * those lists trade the Difference column for the pair that does. Only the
 * ranked column is sortable, because reversing the list re-ranks on the server
 * by that metric alone.
 */
function metricColumns(list: CustomListResult) {
  const metric = list.metric ?? 'pct_diff'
  const columns =
    metric === 'days' || metric === 'volume_cross_pct'
      ? [
          { key: 'days', label: list.operator === 'days_below' ? 'Days Below' : 'Days Above' },
          { key: 'volume_cross_pct', label: 'Vol on Cross' },
        ]
      : [{ key: 'pct_diff', label: 'Difference' }]
  return columns.map((c) => ({ ...c, ranked: c.key === metric }))
}

/** One trailing cell, formatted for whichever metric the column holds. */
function metricCell(key: string, entry: CustomListEntry) {
  if (key === 'days') {
    return entry.days == null
      ? <span style={{ color: '#888' }}>—</span>
      : <span style={{ color: '#2e7d32', fontWeight: 600 }}>{entry.days}d</span>
  }
  if (key === 'volume_cross_pct') {
    const v = entry.volume_cross_pct
    return v == null
      ? <span style={{ color: '#888' }}>—</span>
      : <span style={{ color: v >= 0 ? '#2e7d32' : '#c62828', fontWeight: 600 }}>{v >= 0 ? '+' : ''}{v.toFixed(0)}%</span>
  }
  return (
    <span style={{ color: entry.pct_diff >= 0 ? '#4caf50' : '#f44336', fontWeight: 600 }}>
      {entry.pct_diff >= 0 ? '+' : ''}{entry.pct_diff.toFixed(2)}%
    </span>
  )
}

export default function Dashboard({ onLoading, holdingsVersion, onNavigateToWatchlist, onNavigateToHoldings }: { onLoading: (loading: boolean) => void; holdingsVersion?: number; onNavigateToWatchlist?: (symbol: string) => void; onNavigateToHoldings?: (symbol: string) => void }) {
  const [overview, setOverview] = useState<PortfolioOverview | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  // Sort direction requested per custom list, keyed by list key. Sent to the
  // server on the next fetch; the authoritative direction comes back on each
  // list as `sort`. Empty means "use each list's configured direction".
  const [diffSort, setDiffSort] = useState<Record<string, 'asc' | 'desc'>>({})
  // Which list is mid-resort, so its header can show progress without the
  // whole dashboard dropping to the loading placeholder.
  const [sortingKey, setSortingKey] = useState<string | null>(null)
  const [history, setHistory] = useState<PortfolioHistory | null>(null)
  const [historyError, setHistoryError] = useState<string | null>(null)
  const [historyRange, setHistoryRange] = useState<HistoryRange>('12m')
  const [cashAccounts, setCashAccounts] = useState<CashAccount[]>([])
  /**
   * The whole portfolio, for the heat map. The overview this screen is built on
   * carries only aggregates, so the per-holding rows the map needs come from
   * their own endpoint — and a failure there empties the map alone.
   */
  const [heatMapHoldings, setHeatMapHoldings] = useState<PortfolioHolding[]>([])

  /**
   * A range starting at or before the configured floor returns exactly what
   * "All" returns, so offering it invites the question of why three buttons
   * give the same answer. The floor arrives with the history, so every button
   * shows until the first response lands.
   */
  const visibleRanges = useMemo(() => {
    const floor = history?.start_floor
    if (!floor) return HISTORY_RANGES
    return HISTORY_RANGES.filter(([value]) => {
      const start = rangeStart(value)
      return start === undefined || start > floor
    })
  }, [history])

  // Derived rather than synced back into state: losing the selected range to
  // the floor would otherwise leave no button lit and the chart showing a
  // window nothing claims.
  const activeRange: HistoryRange = visibleRanges.some(([value]) => value === historyRange)
    ? historyRange
    : 'all'

  useEffect(() => {
    load()
  }, [holdingsVersion])

  // Loaded separately from the overview: the overview carries only aggregates,
  // and a failure here should empty the heat map rather than the whole screen.
  useEffect(() => {
    apiClient
      .getPortfolioHoldings()
      .then((r) => setHeatMapHoldings(r.holdings))
      .catch(() => setHeatMapHoldings([]))
  }, [holdingsVersion])

  // Loaded separately from the overview: it sweeps every day of history, so a
  // slow response should not hold up the rest of the dashboard.
  useEffect(() => {
    const from = rangeStart(activeRange)
    setHistoryError(null)
    apiClient
      .getPortfolioHistory(from)
      .then(setHistory)
      .catch((err) => setHistoryError(err instanceof Error ? err.message : 'Failed to load portfolio history'))
  }, [holdingsVersion, activeRange])

  // `quiet` keeps the rendered dashboard on screen while refetching — a sort
  // click should re-rank a table, not blank the entire page.
  const load = async (listSort: Record<string, 'asc' | 'desc'> = diffSort, quiet = false) => {
    try {
      if (!quiet) {
        setLoading(true)
        onLoading(true)
      }
      setError(null)
      setOverview(await apiClient.getPortfolioOverview(listSort))
      // Separate call, and a failure only empties the cash card: the rest of
      // the dashboard predates cash tracking and must not depend on it.
      apiClient.getCashAccounts?.().then(setCashAccounts).catch(() => setCashAccounts([]))
      apiClient.getPortfolioHoldings().then((r) => setHeatMapHoldings(r.holdings)).catch(() => setHeatMapHoldings([]))
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load dashboard')
    } finally {
      if (!quiet) {
        setLoading(false)
        onLoading(false)
      }
    }
  }

  const portfolio = useMemo(() => {
    const t = overview?.totals
    const b = overview?.breakdowns
    return {
      stockCount: t?.stock_count ?? 0,
      totalValue: t?.total_value ?? 0,
      totalPL: t?.total_pl ?? 0,
      holdingsPL: b?.holdings.pl ?? 0,
      holdingsDividends: b?.holdings.dividends ?? 0,
      holdingsCost: b?.holdings.cost ?? 0,
      equityCount: b?.equities.count ?? 0,
      equityValue: b?.equities.value ?? 0,
      equityDividends: b?.equities.dividends ?? 0,
      equityPL: b?.equities.pl ?? 0,
      equityCost: b?.equities.cost ?? 0,
      etfCount: b?.etfs.count ?? 0,
      etfValue: b?.etfs.value ?? 0,
      etfDividends: b?.etfs.dividends ?? 0,
      etfPL: b?.etfs.pl ?? 0,
      etfCost: b?.etfs.cost ?? 0,
      soldCount: b?.sold.count ?? 0,
      soldProceeds: b?.sold.value ?? 0,
      soldDividendsTotal: b?.sold.dividends ?? 0,
      soldPL: b?.sold.pl ?? 0,
      soldCost: b?.sold.cost ?? 0,
      sectors: overview?.sectors ?? [],
    }
  }, [overview])

  const worstHoldings = overview?.worst_holdings ?? []
  const bestWatchlist = overview?.best_watchlist ?? []
  const customLists = overview?.custom_lists ?? []

  // Flip against the direction the server reported, so the first click is
  // correct even when the list's configured default is descending.
  const toggleDiffSort = async (list: CustomListResult) => {
    const next = list.sort === 'asc' ? 'desc' : 'asc'
    const nextSort: Record<string, 'asc' | 'desc'> = { ...diffSort, [list.key]: next }
    setDiffSort(nextSort)
    setSortingKey(list.key)
    try {
      await load(nextSort, true)
    } finally {
      setSortingKey(null)
    }
  }

  if (loading) return <p className="loading-text">Loading dashboard...</p>
  if (error) return <div className="alert alert-error">❌ {error}</div>

  // Navigate to the screen where the symbol actually lives. Watchlist rows go
  // to the Watchlist; holdings rows go to Holdings.
  const symbolButton = (symbol: string, destination: 'watchlist' | 'holdings' = 'watchlist') => {
    const navigate = destination === 'holdings' ? onNavigateToHoldings : onNavigateToWatchlist
    return navigate ? (
      <button
        onClick={() => navigate(symbol)}
        style={{ background: 'none', border: 'none', padding: 0, cursor: 'pointer', fontWeight: 700, color: '#1565c0', textDecoration: 'underline', fontSize: 'inherit' }}
      >
        {symbol}
      </button>
    ) : (
      <strong>{symbol}</strong>
    )
  }

  // Only accounts marked as counting toward the portfolio, converted to AUD.
  // An account with no stored rate for its currency reports balance_aud as
  // null; those are counted separately and surfaced rather than silently
  // dropped, which would understate the total with no hint why.
  const countedCash = cashAccounts.filter((a) => a.include_in_portfolio)
  const cashValue = countedCash.reduce((sum, a) => sum + (a.balance_aud ?? 0), 0)
  const unconvertedCash = countedCash.filter((a) => a.balance_aud === null && a.balance !== 0).length

  return (
    <div className="dashboard">
      <div className="dashboard-stats">
        <div className="stat-card">
          <div className="stat-label">Holdings</div>
          <div className="stat-value">{portfolio.stockCount}</div>
          <div className="stat-sub">stocks</div>
        </div>
        <div className="stat-card">
          <div className="stat-label">Stock Value</div>
          <div className="stat-value">${portfolio.totalValue.toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}</div>
        </div>
        <div className="stat-card">
          <div className="stat-label">Cash Value</div>
          <div className={`stat-value ${cashValue < 0 ? 'negative' : ''}`}>
            {cashValue < 0 ? '−' : ''}${Math.abs(cashValue).toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
          </div>
          {unconvertedCash > 0 && (
            <div className="stat-sub" title="Balances in a currency with no stored exchange rate are not included">
              {unconvertedCash} account{unconvertedCash === 1 ? '' : 's'} not converted
            </div>
          )}
        </div>
        <div className="stat-card">
          <div className="stat-label">Total Value</div>
          <div className="stat-value">${(portfolio.totalValue + cashValue).toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}</div>
          <div className="stat-sub">stocks plus cash</div>
        </div>
        <div className="stat-card">
          <div className="stat-label">Profit / Loss</div>
          <div className={`stat-value ${portfolio.totalPL >= 0 ? 'positive' : 'negative'}`}>
            {portfolio.totalPL >= 0 ? '+' : '−'}${Math.abs(portfolio.totalPL).toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
          </div>
        </div>
      </div>

      <div className="manager-card" style={{ marginBottom: 24 }}>
        <div className="card-header" style={{ marginBottom: 4 }}>
          <h2 style={{ margin: 0 }}>Portfolio Value</h2>
          <div className="sma-selector">
            {visibleRanges.map(([value, label]) => (
              <button
                key={value}
                className={`sma-button ${activeRange === value ? 'active' : ''}`}
                onClick={() => setHistoryRange(value)}
              >
                {label}
              </button>
            ))}
          </div>
        </div>

        {historyError ? (
          <div className="alert alert-error">{historyError}</div>
        ) : !history ? (
          <p className="loading-text">Loading portfolio history…</p>
        ) : (
          <>
            <div className="dashboard-stats" style={{ marginBottom: 12 }}>
              <div className="stat-card">
                <div className="stat-label">Growth</div>
                <div className={`stat-value ${(history.summary.twr_pct ?? 0) >= 0 ? 'positive' : 'negative'}`}>
                  {history.summary.twr_pct === null
                    ? '—'
                    : `${history.summary.twr_pct >= 0 ? '+' : ''}${history.summary.twr_pct.toFixed(2)}%`}
                </div>
                <div className="stat-sub">excludes money added</div>
              </div>
              <div className="stat-card">
                <div className="stat-label">Gain</div>
                <div className={`stat-value ${history.summary.gain >= 0 ? 'positive' : 'negative'}`}>
                  {history.summary.gain >= 0 ? '+' : '−'}${Math.abs(history.summary.gain).toLocaleString('en-AU', { maximumFractionDigits: 0 })}
                </div>
                <div className="stat-sub">what the portfolio earned</div>
              </div>
              <div className="stat-card">
                <div className="stat-label">Contributions</div>
                <div className="stat-value">
                  ${history.summary.net_contributions.toLocaleString('en-AU', { maximumFractionDigits: 0 })}
                </div>
                <div className="stat-sub">money you put in</div>
              </div>
            </div>
            <PortfolioHistoryChart series={history.series} />
          </>
        )}
      </div>

      {/* The portfolio as one map. Holdings breaks the same tiles out by
          section; this is the view across all of them. Clicking a tile opens
          that position on the Holdings screen. */}
      {heatMapHoldings.length > 0 && (
        <CollapsibleCard id="heatmap-overall" title="Heat Map">
          <HoldingsHeatMap holdings={heatMapHoldings} onSelectSymbol={onNavigateToHoldings} />
        </CollapsibleCard>
      )}

      <div className="dashboard-breakdown">
        {[
          { label: 'Equities', count: portfolio.equityCount, value: portfolio.equityValue, dividends: portfolio.equityDividends, pl: portfolio.equityPL, cost: portfolio.equityCost },
          { label: 'ETFs', count: portfolio.etfCount, value: portfolio.etfValue, dividends: portfolio.etfDividends, pl: portfolio.etfPL, cost: portfolio.etfCost },
          { label: 'Holdings', count: portfolio.stockCount, value: portfolio.totalValue, dividends: portfolio.holdingsDividends, pl: portfolio.holdingsPL, cost: portfolio.holdingsCost },
          { label: 'Sold', count: portfolio.soldCount, value: portfolio.soldProceeds, dividends: portfolio.soldDividendsTotal, pl: portfolio.soldPL, cost: portfolio.soldCost },
        ].map(({ label, count, value, dividends, pl, cost }) => {
          const pct = cost > 0 ? (pl / cost) * 100 : null
          return (
          <div key={label} className="breakdown-card">
            <div className="breakdown-label">{label} ({count})</div>
            {value !== null && (
              <div className="breakdown-row">
                <span className="breakdown-key">Value</span>
                <span className="breakdown-val">${value.toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}</span>
              </div>
            )}
            <div className="breakdown-row">
              <span className="breakdown-key">Dividends</span>
              <span className="breakdown-val">${dividends.toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}</span>
            </div>
            <div className="breakdown-row">
              <span className="breakdown-key">P/L</span>
              <span className={`breakdown-val ${pl >= 0 ? 'positive' : 'negative'}`}>
                {pl >= 0 ? '+' : '−'}${Math.abs(pl).toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
                {pct !== null && <span style={{ fontWeight: 400, marginLeft: 4 }}>({pct >= 0 ? '+' : ''}{pct.toFixed(1)}%)</span>}
              </span>
            </div>
          </div>
          )
        })}
      </div>

      {portfolio.sectors.length > 0 && (
        <>
          <h3 style={{ margin: '24px 0 8px', fontSize: 16 }}>Holdings by Sector</h3>
          <div className="dashboard-breakdown">
            {portfolio.sectors.map(({ name, count, value, dividends, pl, cost }) => {
              const pct = cost > 0 ? (pl / cost) * 100 : null
              const weight = portfolio.totalValue > 0 ? (value / portfolio.totalValue) * 100 : null
              return (
                <div key={name} className="breakdown-card">
                  <div className="breakdown-label">{name} ({count})</div>
                  <div className="breakdown-row">
                    <span className="breakdown-key">Value</span>
                    <span className="breakdown-val">
                      ${value.toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
                      {weight !== null && <span style={{ fontWeight: 400, marginLeft: 4, color: '#888' }}>({weight.toFixed(1)}%)</span>}
                    </span>
                  </div>
                  <div className="breakdown-row">
                    <span className="breakdown-key">Dividends</span>
                    <span className="breakdown-val">${dividends.toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}</span>
                  </div>
                  <div className="breakdown-row">
                    <span className="breakdown-key">P/L</span>
                    <span className={`breakdown-val ${pl >= 0 ? 'positive' : 'negative'}`}>
                      {pl >= 0 ? '+' : '−'}${Math.abs(pl).toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
                      {pct !== null && <span style={{ fontWeight: 400, marginLeft: 4 }}>({pct >= 0 ? '+' : ''}{pct.toFixed(1)}%)</span>}
                    </span>
                  </div>
                </div>
              )
            })}
          </div>
        </>
      )}

      <div className="dashboard-lists">
        <div className="manager-card">
          <h2>Worst Holdings — 150SMA</h2>
          <p className="dashboard-list-desc">Holdings trading furthest below their 150-day moving average</p>
          {worstHoldings.length === 0 ? (
            <p className="empty-text">No SMA data available for holdings.</p>
          ) : (
            <table className="holdings-table compact">
              <thead>
                <tr>
                  <th>Symbol</th>
                  <th>Price</th>
                  <th>150SMA</th>
                  <th>Difference</th>
                </tr>
              </thead>
              <tbody>
                {worstHoldings.map((item) => (
                  <tr key={item.symbol}>
                    <td>{symbolButton(item.symbol, 'holdings')}</td>
                    <td>${item.price.toFixed(2)}</td>
                    <td>${item.sma150.toFixed(2)}</td>
                    <td style={{ color: item.pct_diff >= 0 ? '#4caf50' : '#f44336', fontWeight: 600 }}>
                      {item.pct_diff >= 0 ? '+' : ''}{item.pct_diff.toFixed(2)}%
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>

        <div className="manager-card">
          <h2>Best Watchlist — 50SMA</h2>
          <p className="dashboard-list-desc">Watchlist stocks most recently above their 50-day moving average</p>
          {bestWatchlist.length === 0 ? (
            <p className="empty-text">No stocks currently above their 50SMA.</p>
          ) : (
            <table className="holdings-table compact">
              <thead>
                <tr>
                  <th>Symbol</th>
                  <th>Price</th>
                  <th>50SMA</th>
                  <th>Days Above</th>
                  <th>Vol on Cross</th>
                </tr>
              </thead>
              <tbody>
                {bestWatchlist.map((item) => (
                  <tr key={item.symbol}>
                    <td>{symbolButton(item.symbol)}</td>
                    <td>${item.price.toFixed(2)}</td>
                    <td>
                      ${item.sma50.toFixed(2)}
                      {item.sma50_trend != null && (
                        <span style={{ marginLeft: 4, fontSize: 10, color: item.sma50_trend === 'down' ? '#c62828' : '#2e7d32', fontWeight: 600 }}>
                          {item.sma50_trend === 'down' ? '↓' : '↑'}
                        </span>
                      )}
                    </td>
                    <td style={{ color: '#2e7d32', fontWeight: 600 }}>
                      {item.days_since_50sma}d
                    </td>
                    <td style={{ color: item.volume_pct_50sma === null ? undefined : item.volume_pct_50sma >= 0 ? '#2e7d32' : '#c62828', fontWeight: 600 }}>
                      {item.volume_pct_50sma !== null ? `${item.volume_pct_50sma >= 0 ? '+' : ''}${item.volume_pct_50sma.toFixed(0)}%` : '—'}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>

        {customLists.map((list) => (
          <div key={list.key} className="manager-card">
            <h2>{list.label}</h2>
            <p className="dashboard-list-desc">
              {list.source === 'both' ? 'Holdings & Watchlist' : list.source === 'holdings' ? 'Holdings' : 'Watchlist'} stocks where {listCriterion(list)}
            </p>
            {list.entries.length === 0 ? (
              <p className="empty-text">No matching stocks found.</p>
            ) : (
              <table className="holdings-table compact">
                <thead>
                  <tr>
                    <th>Symbol</th>
                    <th>{compareLabel(list)}</th>
                    <th>{list.field_label}</th>
                    {metricColumns(list).map((col) =>
                      col.ranked ? (
                        <th
                          key={col.key}
                          className="sortable-header"
                          onClick={() => toggleDiffSort(list)}
                          title={
                            list.truncated
                              ? `Sorted ${list.sort === 'asc' ? 'ascending' : 'descending'} — click to reverse and reselect the top ${list.entries.length}`
                              : `Sorted ${list.sort === 'asc' ? 'ascending' : 'descending'} — click to reverse`
                          }
                        >
                          {col.label}{sortingKey === list.key ? ' …' : list.sort === 'asc' ? ' ↑' : ' ↓'}
                        </th>
                      ) : (
                        <th key={col.key}>{col.label}</th>
                      )
                    )}
                  </tr>
                </thead>
                <tbody>
                  {list.entries.map((item) => (
                    <tr key={item.symbol}>
                      {/* An indicator list sourced from "both" mixes the two
                          tables, so the row's own origin wins; the list-level
                          field_source is the fallback for an older API. */}
                      <td>{symbolButton(item.symbol, (item.origin ?? list.field_source) === 'holdings' ? 'holdings' : 'watchlist')}</td>
                      <td>
                        {list.compare === 'volume' ? (
                          compareValue(item).toLocaleString('en-AU', { maximumFractionDigits: 0 })
                        ) : (
                          <>
                            ${compareValue(item).toFixed(2)}
                            {item.currency && item.currency.toUpperCase() !== 'AUD' && (
                              <span style={{ fontSize: 10, color: '#e65100', marginLeft: 4 }}>{item.currency.toUpperCase()}</span>
                            )}
                          </>
                        )}
                      </td>
                      <td>
                        ${item.field_value.toFixed(2)}
                        {item.is_trailing && (
                          <span title="Trailing sell trigger" style={{ fontSize: 10, color: '#7a4fd0', marginLeft: 4, fontWeight: 600 }}>T</span>
                        )}
                      </td>
                      {metricColumns(list).map((col) => (
                        <td key={col.key}>{metricCell(col.key, item)}</td>
                      ))}
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </div>
        ))}
      </div>

    </div>
  )
}
