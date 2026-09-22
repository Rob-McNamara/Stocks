import { useEffect, useMemo, useState } from 'react'
import { apiClient, type RiskRow, type RiskTotals } from '../services/api'
import { getEarliestRemainingPurchaseDate, getRemainingPurchaseLots } from '../utils/holdings'
import PriceChart from './PriceChart'

// Thin client: FIFO cost basis, stop-loss/trailing-sell triggers, SMAs and
// the 30-day high are all computed by the API server
// (GET /api/portfolio/risk). This component renders server rows and manages
// the chart and the stop-loss edit modal.

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

interface AnalysisRow {
  symbol: string
  currentPrice: number | null
  purchasePrice: number | null
  plPct: number | null
  stopLoss: number | null
  stopLossPct: number | null
  stopLossDollar: number | null
  /**
   * How far the current price sits above its stop, as a percentage of the
   * stop. Same formula as the Dashboard's Difference column so the two screens
   * agree. Being a ratio of two same-currency figures, it needs no FX handling.
   */
  stopLossDiffPct: number | null
  /**
   * The same gap in dollars across the whole position: (price − stop) × shares.
   * Position-level rather than per-share, matching `stopLossDollar` — the
   * per-share gap is already readable straight off the Price and Stop Loss
   * columns, whereas the amount at risk is not.
   */
  stopLossDiffDollar: number | null
  totalInvested: number | null
  isTrailingSell: boolean
  sma50: number | null
  sma150: number | null
  ema40w: number | null
  high30d: number | null
  currency: string | null
}

export default function Analysis({ onLoading, holdingsVersion }: { onLoading: (loading: boolean) => void; holdingsVersion?: number }) {
  const [transactions, setTransactions] = useState<HoldingTransaction[]>([])
  const [prices, setPrices] = useState<Record<string, number | null>>({})
  const [volumes, setVolumes] = useState<Record<string, number | null>>({})
  // The trading date each cached price belongs to. Without it the chart dates
  // the live price "today" in UTC, which runs a day ahead of the last US close
  // for most of the Sydney day and invents a bar no candle can be drawn for.
  const [priceDates, setPriceDates] = useState<Record<string, string | null>>({})
  const [symbolFields, setSymbolFields] = useState<Record<string, Record<string, string>>>({})
  const [symbolInfo, setSymbolInfo] = useState<Record<string, { instrument_type: string | null; long_name: string | null; currency: string | null }>>({})
  /** Server-computed risk rows from /api/portfolio/risk */
  const [riskRows, setRiskRows] = useState<RiskRow[]>([])
  const [riskTotals, setRiskTotals] = useState<RiskTotals | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [selectedSymbol, setSelectedSymbol] = useState('')
  const [sortColumn, setSortColumn] = useState<string | null>(null)
  const [sortDirection, setSortDirection] = useState<'asc' | 'desc'>('asc')
  const [editingStopLossSymbol, setEditingStopLossSymbol] = useState<string | null>(null)
  const [editStopLoss, setEditStopLoss] = useState('')
  const [editTrailingSellPct, setEditTrailingSellPct] = useState('')
  const [editTrailingSellDate, setEditTrailingSellDate] = useState('')

  useEffect(() => {
    loadData()
  }, [holdingsVersion])

  const loadData = async () => {
    try {
      setLoading(true)
      setError(null)
      onLoading(true)

      const [txData, symFields, infoData, riskData] = await Promise.all([
        apiClient.getHoldings(),
        apiClient.getHoldingsSymbolFields(),
        apiClient.getSymbolInfo(),
        apiClient.getPortfolioRisk(),
      ])
      setTransactions(txData)
      setSymbolFields(symFields)
      const infoMap: Record<string, { instrument_type: string | null; long_name: string | null; currency: string | null }> = {}
      infoData.forEach((i) => { infoMap[i.symbol] = { instrument_type: i.instrument_type, long_name: i.long_name, currency: i.currency } })
      setSymbolInfo(infoMap)
      setRiskRows(riskData.rows)
      setRiskTotals(riskData.totals)

      const activeSymbols = riskData.rows.map((r) => r.symbol)
      if (!selectedSymbol && activeSymbols.length > 0) setSelectedSymbol(activeSymbols[0])

      // Native cached prices/volumes are only needed as chart inputs
      const cachedPrices = await apiClient.getCachedPrices(activeSymbols)
      const priceMap: Record<string, number | null> = {}
      const volumeMap: Record<string, number | null> = {}
      const dateMap: Record<string, string | null> = {}
      cachedPrices.forEach((p) => {
        priceMap[p.symbol] = p.price
        volumeMap[p.symbol] = p.volume
        dateMap[p.symbol] = p.price_date ?? null
      })
      setPrices(priceMap)
      setVolumes(volumeMap)
      setPriceDates(dateMap)

      setLoading(false)
      onLoading(false)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load analysis data')
      setLoading(false)
      onLoading(false)
    }
  }

  // Adapt server risk rows to the shape the JSX renders
  const rows: AnalysisRow[] = useMemo(() => riskRows.map((r) => ({
    symbol: r.symbol,
    currentPrice: r.current_price,
    purchasePrice: r.purchase_price,
    plPct: r.pl_pct,
    stopLoss: r.stop_loss,
    stopLossPct: r.stop_loss_pct,
    stopLossDollar: r.stop_loss_dollar,
    stopLossDiffPct:
      r.current_price !== null && r.stop_loss !== null && r.stop_loss !== 0
        ? ((r.current_price - r.stop_loss) / r.stop_loss) * 100
        : null,
    stopLossDiffDollar:
      r.current_price !== null && r.stop_loss !== null && r.shares > 0
        ? (r.current_price - r.stop_loss) * r.shares
        : null,
    totalInvested: r.total_invested,
    isTrailingSell: r.is_trailing_sell,
    sma50: r.sma50,
    sma150: r.sma150,
    ema40w: r.ema40w,
    high30d: r.high30d,
    currency: r.currency,
  })), [riskRows])

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

  const sortedRows = useMemo(() => {
    if (!sortColumn) return rows
    return [...rows].sort((a, b) => {
      const getValue = (row: AnalysisRow): number | string | null => {
        switch (sortColumn) {
          case 'symbol': return row.symbol
          case 'plPct': return row.plPct
          case 'stopLossDollar': return row.stopLossDollar
          case 'purchasePrice': return row.purchasePrice
          case 'currentPrice': return row.currentPrice
          case 'stopLoss': return row.stopLoss
          case 'sma50': return row.sma50
          case 'sma150': return row.sma150
          case 'ema40w': return row.ema40w
          case 'high30d': return row.high30d
          case 'stopLossPct': return row.stopLossPct
          case 'stopLossDiffPct': return row.stopLossDiffPct
          case 'stopLossDiffDollar': return row.stopLossDiffDollar
          default: return null
        }
      }
      let aVal = getValue(a)
      let bVal = getValue(b)
      if (aVal === null) aVal = -Infinity
      if (bVal === null) bVal = -Infinity
      if (typeof aVal === 'string' && typeof bVal === 'string') {
        return sortDirection === 'asc' ? aVal.localeCompare(bVal) : bVal.localeCompare(aVal)
      }
      return sortDirection === 'asc' ? (aVal as number) - (bVal as number) : (bVal as number) - (aVal as number)
    })
  }, [rows, sortColumn, sortDirection])

  const selectedRow = rows.find((r) => r.symbol === selectedSymbol)

  if (loading && rows.length === 0) return <p className="loading-text">Loading analysis...</p>
  if (error) return <div className="alert alert-error">{error}</div>

  return (
    <div className="holdings-manager">
      {selectedSymbol && (
        <div className="manager-card" style={{ marginBottom: 24 }}>
          <div className="card-header" style={{ marginBottom: 8 }}>
            <h3 style={{ margin: 0 }}>Stock Chart</h3>
            <div className="chart-select">
              <label htmlFor="analysis-chart-symbol">Symbol</label>
              <select
                id="analysis-chart-symbol"
                value={selectedSymbol}
                onChange={(e) => setSelectedSymbol(e.target.value)}
                disabled={loading}
              >
                {rows.map((r) => (
                  <option key={r.symbol} value={r.symbol}>{r.symbol}</option>
                ))}
              </select>
            </div>
          </div>
          <PriceChart
            symbol={selectedSymbol}
            currency={symbolInfo[selectedSymbol]?.currency?.toUpperCase() ?? 'AUD'}
            onLoading={onLoading}
            purchasePrice={selectedRow?.purchasePrice ?? null}
            purchaseDate={getEarliestRemainingPurchaseDate(transactions, selectedSymbol)}
            purchases={getRemainingPurchaseLots(transactions, selectedSymbol)}
            currentPrice={prices[selectedSymbol] ?? null}
            currentVolume={volumes[selectedSymbol] ?? null}
            currentPriceDate={priceDates[selectedSymbol] ?? null}
            markerPrice={selectedRow?.stopLoss ?? null}
            markerLabel={selectedRow?.isTrailingSell ? 'Trailing Sell' : 'Stop Loss'}
            markerMode="stoploss"
          />
        </div>
      )}

      <div className="manager-card">
        <h2>Active Holdings Analysis</h2>
        {rows.length === 0 ? (
          <p className="empty-text">No active holdings.</p>
        ) : (
          <div className="holdings-table-wrapper resizable-table-wrapper">
            <table className="holdings-table" style={{ position: 'relative' }}>
              {/* Two-level header: three groups (Current, Stop Loss, SMA) over
                  their child columns. Ungrouped columns use rowSpan={2} so their
                  labels stay vertically centred across both rows. */}
              <thead style={{ position: 'sticky', top: 0, zIndex: 1 }}>
                <tr style={{ background: '#fff' }}>
                  <th rowSpan={2} scope="col" className="sortable-header" onClick={() => handleSort('symbol')}>Symbol{sortIndicator('symbol')}</th>
                  <th colSpan={2} scope="colgroup" className="column-group rule-left">Current</th>
                  <th colSpan={5} scope="colgroup" className="column-group rule-left">Stop Loss</th>
                  <th colSpan={3} scope="colgroup" className="column-group rule-left">Moving Avgs</th>
                  <th rowSpan={2} scope="col" className="rule-left">30d High</th>
                </tr>
                {/* Second-row cells appear in column order across all groups:
                    Current's two, then Stop Loss's four, then SMA's two. Both
                    "Price" and "P/L%" appear twice — the group above them is
                    what tells the two apart. */}
                <tr style={{ background: '#fff' }}>
                  <th scope="col" className="rule-left" title="Current price">Price</th>
                  <th scope="col" className="sortable-header" onClick={() => handleSort('plPct')} title="Profit/loss percentage at the current price">P/L%{sortIndicator('plPct')}</th>
                  <th scope="col" className="rule-left" title="Stop loss trigger price">Price</th>
                  <th
                    scope="col"
                    className="sortable-header"
                    onClick={() => handleSort('stopLossDiffPct')}
                    title="How far the current price is above its stop loss, as a percentage of the stop"
                  >
                    Diff%{sortIndicator('stopLossDiffPct')}
                  </th>
                  <th
                    scope="col"
                    className="sortable-header"
                    onClick={() => handleSort('stopLossDiffDollar')}
                    title="Value given up across the whole position if the stop loss triggered at today's price"
                  >
                    Diff${sortIndicator('stopLossDiffDollar')}
                  </th>
                  <th scope="col" className="sortable-header" onClick={() => handleSort('stopLossPct')} title="Profit/loss percentage if sold at the stop loss">P/L%{sortIndicator('stopLossPct')}</th>
                  <th scope="col" className="sortable-header" onClick={() => handleSort('stopLossDollar')} title="Profit/loss in dollars if sold at the stop loss">P/L${sortIndicator('stopLossDollar')}</th>
                  <th scope="col" className="rule-left" title="40-week exponential moving average (weekly closes)">40we</th>
                  <th scope="col" title="50-day simple moving average">50s</th>
                  <th scope="col" title="150-day simple moving average">150s</th>
                </tr>
              </thead>
              <tbody>
                {sortedRows.map((row) => (
                  <tr
                    key={row.symbol}
                    onClick={() => setSelectedSymbol(row.symbol)}
                    style={{ cursor: 'pointer', background: selectedSymbol === row.symbol ? '#e3f2fd' : undefined }}
                  >
                    <td>
                      <button
                        onClick={(e) => { e.stopPropagation(); setSelectedSymbol(row.symbol) }}
                        style={{ background: 'none', border: 'none', padding: 0, cursor: 'pointer', fontWeight: 700, color: '#1565c0', textDecoration: 'underline', fontSize: 'inherit' }}
                      >
                        {row.symbol}
                      </button>
                      {row.currency && row.currency !== 'AUD' && (
                        <span style={{ fontSize: 10, fontWeight: 600, padding: '1px 4px', borderRadius: 3, background: '#fff3e0', color: '#e65100', marginLeft: 4 }}>
                          {row.currency}
                        </span>
                      )}
                    </td>
                    <td className="rule-left">{row.currentPrice !== null ? `$${row.currentPrice.toFixed(2)}` : '—'}</td>
                    <td style={{ color: row.plPct !== null ? (row.plPct >= 0 ? '#4caf50' : '#f44336') : undefined, fontWeight: 600 }}>
                      {row.plPct !== null ? `${row.plPct >= 0 ? '+' : ''}${row.plPct.toFixed(1)}%` : '—'}
                    </td>
                    <td
                      className="rule-left"
                      style={{ cursor: 'pointer' }}
                      title="Double-click to edit"
                      onDoubleClick={(e) => {
                        e.stopPropagation()
                        setEditingStopLossSymbol(row.symbol)
                        setEditStopLoss(symbolFields[row.symbol]?.['stop_loss'] ?? '')
                        setEditTrailingSellPct(symbolFields[row.symbol]?.['trailing_sell_pct'] ?? '')
                        setEditTrailingSellDate(symbolFields[row.symbol]?.['trailing_sell_date'] ?? '')
                      }}
                    >{row.stopLoss !== null ? `$${row.stopLoss.toFixed(2)}${row.isTrailingSell ? '*' : ''}` : '—'}</td>
                    <td style={{ color: row.stopLossDiffPct !== null ? (row.stopLossDiffPct >= 0 ? '#4caf50' : '#f44336') : undefined, fontWeight: 600 }}>
                      {row.stopLossDiffPct !== null ? `${row.stopLossDiffPct >= 0 ? '+' : ''}${row.stopLossDiffPct.toFixed(2)}%` : '—'}
                    </td>
                    <td style={{ color: row.stopLossDiffDollar !== null ? (row.stopLossDiffDollar >= 0 ? '#4caf50' : '#f44336') : undefined, fontWeight: 600 }}>
                      {row.stopLossDiffDollar !== null ? `${row.stopLossDiffDollar >= 0 ? '+' : '-'}$${Math.abs(row.stopLossDiffDollar).toFixed(2)}` : '—'}
                    </td>
                    <td style={{ color: row.stopLossPct !== null ? (row.stopLossPct >= 0 ? '#4caf50' : '#f44336') : undefined, fontWeight: 600 }}>
                      {row.stopLossPct !== null ? `${row.stopLossPct >= 0 ? '+' : ''}${row.stopLossPct.toFixed(1)}%` : '—'}
                    </td>
                    <td style={{ color: row.stopLossDollar !== null ? (row.stopLossDollar >= 0 ? '#4caf50' : '#f44336') : undefined, fontWeight: 600 }}>
                      {row.stopLossDollar !== null ? `${row.stopLossDollar >= 0 ? '+' : '-'}$${Math.abs(row.stopLossDollar).toFixed(2)}` : '—'}
                    </td>
                    <td className="rule-left" style={{ color: row.ema40w !== null && row.currentPrice !== null && row.currentPrice < row.ema40w ? '#f44336' : undefined }}>
                      {row.ema40w !== null ? `$${row.ema40w.toFixed(2)}` : '—'}
                    </td>
                    <td style={{ color: row.sma50 !== null && row.currentPrice !== null && row.currentPrice < row.sma50 ? '#f44336' : undefined }}>
                      {row.sma50 !== null ? `$${row.sma50.toFixed(2)}` : '—'}
                    </td>
                    <td style={{ color: row.sma150 !== null && row.currentPrice !== null && row.currentPrice < row.sma150 ? '#f44336' : undefined }}>
                      {row.sma150 !== null ? `$${row.sma150.toFixed(2)}` : '—'}
                    </td>
                    <td className="rule-left">{row.high30d !== null ? `$${row.high30d.toFixed(2)}` : '—'}</td>
                  </tr>
                ))}
              </tbody>
              {(() => {
                const totalInvested = riskTotals?.total_invested ?? 0
                const totalSlDollar = riskTotals?.total_sl_dollar ?? 0
                const totalSlPct = totalInvested > 0 ? (totalSlDollar / totalInvested) * 100 : null
                return (
                  <tfoot>
                    <tr style={{ borderTop: '2px solid #ccc', fontWeight: 700 }}>
                      {/* Cells are split on the group boundaries rather than
                          merged into one wide spacer, so the vertical group
                          rules run unbroken from the header through to here.
                          The label therefore starts at the Current group rather
                          than the Symbol column. */}
                      <td />
                      <td colSpan={2} className="rule-left" style={{ textAlign: 'right' }}>Total if all sold at Stop Loss:</td>
                      <td className="rule-left" />
                      <td />
                      <td />
                      <td style={{ color: totalSlPct !== null ? (totalSlPct >= 0 ? '#4caf50' : '#f44336') : undefined }}>
                        {totalSlPct !== null ? `${totalSlPct >= 0 ? '+' : ''}${totalSlPct.toFixed(1)}%` : '—'}
                      </td>
                      <td style={{ color: totalSlDollar >= 0 ? '#4caf50' : '#f44336' }}>
                        {`${totalSlDollar >= 0 ? '+' : '-'}$${Math.abs(totalSlDollar).toFixed(2)}`}
                      </td>
                      {/* SMA and 30d High have no total */}
                      <td colSpan={3} className="rule-left" />
                      <td className="rule-left" />
                    </tr>
                  </tfoot>
                )
              })()}
            </table>
            <div style={{ fontSize: 11, color: '#999', marginTop: 6 }}>* Trailing Sell — trigger price calculated from highest price since placement date</div>
          </div>
        )}
      </div>

      {editingStopLossSymbol && (
        <div className="modal-overlay"
          onClick={() => setEditingStopLossSymbol(null)}>
          <div className="modal-card" style={{ minWidth: 320 }}
            onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3 style={{ margin: 0 }}>Edit Stop Loss — {editingStopLossSymbol}</h3>
            </div>
            <div className="modal-body">
            <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 13, color: '#666' }}>Stop Loss Price</label>
                <input
                  type="number"
                  min="0"
                  step="0.01"
                  value={editStopLoss}
                  onChange={(e) => setEditStopLoss(e.target.value)}
                  className="symbol-input"
                  style={{ width: '100%' }}
                />
              </div>
              <div style={{ borderTop: '1px solid #eee', paddingTop: 12 }}>
                <label style={{ fontSize: 13, color: '#666', fontWeight: 600 }}>— or Trailing Sell —</label>
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 13, color: '#666' }}>Trailing Sell %</label>
                <input
                  type="number"
                  min="0"
                  step="0.1"
                  value={editTrailingSellPct}
                  onChange={(e) => setEditTrailingSellPct(e.target.value)}
                  className="symbol-input"
                  style={{ width: '100%' }}
                />
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 13, color: '#666' }}>Trailing Sell Date</label>
                <input
                  type="date"
                  value={editTrailingSellDate}
                  onChange={(e) => setEditTrailingSellDate(e.target.value)}
                  className="symbol-input"
                  style={{ width: '100%' }}
                />
              </div>
            </div>
            </div>
            <div className="modal-footer">
              <button className="btn btn-outline" onClick={() => setEditingStopLossSymbol(null)}>Cancel</button>
              <button
                className="btn btn-primary"
                onClick={async () => {
                  if (!editingStopLossSymbol) return
                  const fields: Record<string, string> = {
                    stop_loss: editStopLoss.trim(),
                    trailing_sell_pct: editTrailingSellPct.trim(),
                    trailing_sell_date: editTrailingSellDate.trim(),
                  }
                  try {
                    await apiClient.updateHoldingsSymbolFields(
                      editingStopLossSymbol,
                      symbolFields[editingStopLossSymbol]?.['_notes'] ?? null,
                      fields,
                    )
                    setSymbolFields((prev) => ({
                      ...prev,
                      [editingStopLossSymbol]: { ...prev[editingStopLossSymbol], ...fields },
                    }))
                    setEditingStopLossSymbol(null)
                    // Recompute stop-loss/trailing rows server-side
                    apiClient.getPortfolioRisk().then((d) => {
                      setRiskRows(d.rows)
                      setRiskTotals(d.totals)
                    }).catch(() => {})
                  } catch {
                    // keep dialog open on error
                  }
                }}
              >
                Save
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
