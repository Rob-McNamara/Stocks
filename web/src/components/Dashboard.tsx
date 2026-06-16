import { useEffect, useMemo, useState } from 'react'
import { apiClient } from '../services/api'
import { calculateSMA, getLatestSMA } from '../utils/sma'

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
}

interface SymbolSmaEntry {
  symbol: string
  price: number | null
  sma: number | null
  pctDiff: number | null
}

export default function Dashboard({ onLoading, holdingsVersion }: { onLoading: (loading: boolean) => void; holdingsVersion?: number }) {
  const [transactions, setTransactions] = useState<HoldingTransaction[]>([])
  const [holdingPrices, setHoldingPrices] = useState<Record<string, number | null>>({})
  const [holdingSma, setHoldingSma] = useState<Record<string, number | null>>({})
  const [watchlistEntries, setWatchlistEntries] = useState<SymbolSmaEntry[]>([])
  const [symbolInfo, setSymbolInfo] = useState<Record<string, { instrument_type: string | null; currency: string | null }>>({})
  const [appConfig, setAppConfig] = useState<Record<string, string>>({})
  const [usdToAud, setUsdToAud] = useState<number | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    load()
  }, [holdingsVersion])

  const load = async () => {
    try {
      setLoading(true)
      setError(null)
      onLoading(true)

      const [txData, watchlistSymbols, watchlistPricesData, , configData, infoData, fxData] = await Promise.all([
        apiClient.getHoldings(),
        apiClient.getWatchlistSymbols(),
        apiClient.getWatchlistPrices(),
        apiClient.getDividends(),
        apiClient.getConfig(),
        apiClient.getSymbolInfo(),
        apiClient.getFxRates(),
      ])
      setTransactions(txData)
      setAppConfig(configData)
      if (fxData.USDAUD) setUsdToAud(fxData.USDAUD)
      const infoMap: Record<string, { instrument_type: string | null; currency: string | null }> = {}
      infoData.forEach((i) => { infoMap[i.symbol] = { instrument_type: i.instrument_type, currency: i.currency } })
      setSymbolInfo(infoMap)

      const holdingSymbols = Array.from(new Set(txData.map((tx) => tx.symbol)))

      const [pricesData] = await Promise.all([
        holdingSymbols.length > 0 ? apiClient.getCurrentPrices(holdingSymbols) : Promise.resolve([]),
      ])

      const priceMap: Record<string, number | null> = {}
      pricesData.forEach((p) => { priceMap[p.symbol] = p.price })

      // Apply manual prices from config (same as HoldingsManager)
      Object.entries(configData).forEach(([key, value]) => {
        if (key.startsWith('manual_price_')) {
          const symbol = key.replace('manual_price_', '')
          const parsed = parseFloat(value)
          if (!isNaN(parsed) && (priceMap[symbol] == null || priceMap[symbol] === 0)) {
            priceMap[symbol] = parsed
          }
        }
      })

      setHoldingPrices(priceMap)

      // Fetch SMA for holdings (150-day) and watchlist (50-day) in parallel
      const watchlistSymbolNames = watchlistSymbols.map((s) => s.symbol)
      const holdingOnlySymbols = holdingSymbols.filter((s) => !watchlistSymbolNames.includes(s))
      const watchlistOnlySymbols = watchlistSymbolNames.filter((s) => !holdingSymbols.includes(s))
      const sharedSymbols = holdingSymbols.filter((s) => watchlistSymbolNames.includes(s))

      const [holdingResults, watchlistResults, sharedResults] = await Promise.all([
        Promise.all(holdingOnlySymbols.map(async (sym) => {
          try {
            const history = await apiClient.getPriceHistory(sym, 300)
            return { symbol: sym, sma150: getLatestSMA(calculateSMA(history, 150)), sma50: null as number | null }
          } catch { return { symbol: sym, sma150: null, sma50: null } }
        })),
        Promise.all(watchlistOnlySymbols.map(async (sym) => {
          try {
            const history = await apiClient.getPriceHistory(sym, 200)
            return { symbol: sym, sma150: null as number | null, sma50: getLatestSMA(calculateSMA(history, 50)) }
          } catch { return { symbol: sym, sma150: null, sma50: null } }
        })),
        Promise.all(sharedSymbols.map(async (sym) => {
          try {
            const history = await apiClient.getPriceHistory(sym, 300)
            return { symbol: sym, sma150: getLatestSMA(calculateSMA(history, 150)), sma50: getLatestSMA(calculateSMA(history, 50)) }
          } catch { return { symbol: sym, sma150: null, sma50: null } }
        })),
      ])

      const sma150Map: Record<string, number | null> = {}
      const sma50Map: Record<string, number | null> = {}
      ;[...holdingResults, ...sharedResults].forEach(({ symbol, sma150 }) => { sma150Map[symbol] = sma150 })
      ;[...watchlistResults, ...sharedResults].forEach(({ symbol, sma50 }) => { sma50Map[symbol] = sma50 })
      setHoldingSma(sma150Map)

      const watchlistWithSma: SymbolSmaEntry[] = watchlistPricesData.map((p) => {
        const sma = sma50Map[p.symbol] ?? null
        const pctDiff = p.price && sma ? ((p.price - sma) / sma) * 100 : null
        return { symbol: p.symbol, price: p.price, sma, pctDiff }
      })
      setWatchlistEntries(watchlistWithSma)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load dashboard')
    } finally {
      setLoading(false)
      onLoading(false)
    }
  }

  const portfolio = useMemo(() => {
    const etfTypes = new Set(['ETF', 'MUTUALFUND'])
    const getType = (symbol: string) => appConfig[`instrument_type_${symbol}`] || symbolInfo[symbol]?.instrument_type || ''
    const isEtf = (symbol: string) => etfTypes.has(getType(symbol))
    const toAud = (price: number, symbol: string) => {
      const currency = symbolInfo[symbol]?.currency?.toUpperCase()
      if (currency && currency !== 'AUD' && usdToAud) return price * usdToAud
      return price
    }

    const groupedBySymbol: Record<string, HoldingTransaction[]> = {}
    transactions.forEach((tx) => {
      if (!groupedBySymbol[tx.symbol]) groupedBySymbol[tx.symbol] = []
      groupedBySymbol[tx.symbol].push(tx)
    })

    let stockCount = 0
    let equityCount = 0, etfCount = 0, soldCount = 0
    let totalValue = 0
    let holdingsPL = 0
    let holdingsDividends = 0
    let soldPL = 0
    let soldDividendsTotal = 0
    let soldProceeds = 0

    let equityValue = 0, equityDividends = 0, equityPL = 0, equityCost = 0
    let etfValue = 0, etfDividends = 0, etfPL = 0, etfCost = 0
    let holdingsCost = 0
    let soldCost = 0

    Object.entries(groupedBySymbol).forEach(([symbol, txs]) => {
      const sorted = [...txs].sort((a, b) => a.date.localeCompare(b.date) || a.id - b.id)

      let dividends = 0
      let dividendsFromTotal = 0
      sorted.forEach((tx) => {
        if (tx.dividends_total > 0) dividendsFromTotal = tx.dividends_total
        else if (tx.transaction_type === 'dividend' && tx.amount) dividends += tx.amount
      })
      const symbolDividends = dividendsFromTotal > 0 ? dividendsFromTotal : dividends

      const totalSoldQty = sorted.reduce((s, tx) =>
        tx.transaction_type === 'sale' && tx.quantity ? s + tx.quantity : s, 0)

      const lots: Array<{ quantity: number; price: number }> = []
      let symbolSoldPL = 0
      let symbolSoldDividends = 0
      let symbolSoldProceeds = 0

      sorted.forEach((tx) => {
        if (tx.transaction_type === 'purchase' && tx.quantity && tx.price) {
          lots.push({ quantity: tx.quantity, price: tx.price })
        } else if (tx.transaction_type === 'sale' && tx.quantity && tx.price) {
          let remaining = tx.quantity
          let costBasis = 0
          while (remaining > 0 && lots.length > 0) {
            const used = Math.min(remaining, lots[0].quantity)
            costBasis += used * lots[0].price
            lots[0].quantity -= used
            remaining -= used
            if (lots[0].quantity <= 0) lots.shift()
          }
          const saleDividends = totalSoldQty > 0 ? (tx.quantity / totalSoldQty) * symbolDividends : 0
          symbolSoldPL += tx.quantity * tx.price - (tx.brokerage ?? 0) - costBasis + saleDividends
          symbolSoldDividends += saleDividends
          symbolSoldProceeds += tx.quantity * tx.price
        }
      })

      const remainingShares = lots.reduce((s, l) => s + l.quantity, 0)
      const remainingCost = lots.reduce((s, l) => s + l.quantity * l.price, 0)

      if (remainingShares > 0) {
        stockCount++
        const rawPrice = holdingPrices[symbol]
        const price = rawPrice ? toAud(rawPrice, symbol) : null
        const currentValue = price ? remainingShares * price : 0
        if (price) totalValue += currentValue
        const symPL = currentValue - remainingCost + symbolDividends
        holdingsPL += symPL
        holdingsDividends += symbolDividends
        holdingsCost += remainingCost

        if (isEtf(symbol)) {
          etfCount++
          etfValue += currentValue
          etfDividends += symbolDividends
          etfPL += symPL
          etfCost += remainingCost
        } else {
          equityCount++
          equityValue += currentValue
          equityDividends += symbolDividends
          equityPL += symPL
          equityCost += remainingCost
        }
      }

      if (symbolSoldProceeds > 0) {
        soldCount++
        soldCost += symbolSoldProceeds - symbolSoldPL + symbolSoldDividends
      }
      soldPL += symbolSoldPL
      soldDividendsTotal += symbolSoldDividends
      soldProceeds += symbolSoldProceeds
    })

    return {
      stockCount, equityCount, etfCount, soldCount,
      totalValue, totalPL: holdingsPL + soldPL,
      holdingsPL, holdingsDividends, holdingsCost,
      soldPL, soldDividendsTotal, soldProceeds, soldCost,
      equityValue, equityDividends, equityPL, equityCost,
      etfValue, etfDividends, etfPL, etfCost,
    }
  }, [transactions, holdingPrices, symbolInfo, appConfig, usdToAud])

  const worstHoldings = useMemo((): SymbolSmaEntry[] => {
    const bySymbol: Record<string, number> = {}
    transactions.forEach((tx) => {
      if (!bySymbol[tx.symbol]) bySymbol[tx.symbol] = 0
      if (tx.transaction_type === 'purchase' && tx.quantity) bySymbol[tx.symbol] += tx.quantity
      if (tx.transaction_type === 'sale' && tx.quantity) bySymbol[tx.symbol] -= tx.quantity
    })

    const toAud = (price: number, symbol: string) => {
      const currency = symbolInfo[symbol]?.currency?.toUpperCase()
      if (currency && currency !== 'AUD' && usdToAud) return price * usdToAud
      return price
    }
    return Object.entries(bySymbol)
      .filter(([, shares]) => shares > 0)
      .map(([symbol]) => {
        const rawPrice = holdingPrices[symbol] ?? null
        const price = rawPrice !== null ? toAud(rawPrice, symbol) : null
        const rawSma = holdingSma[symbol] ?? null
        const sma = rawSma !== null ? toAud(rawSma, symbol) : null
        const pctDiff = price && sma ? ((price - sma) / sma) * 100 : null
        return { symbol, price, sma, pctDiff }
      })
      .filter((item): item is SymbolSmaEntry & { pctDiff: number } => item.pctDiff !== null)
      .sort((a, b) => a.pctDiff - b.pctDiff)
      .slice(0, 10)
  }, [transactions, holdingPrices, holdingSma, symbolInfo, usdToAud])

  const bestWatchlist = useMemo(() => {
    return [...watchlistEntries]
      .filter((item): item is SymbolSmaEntry & { pctDiff: number } => item.pctDiff !== null)
      .sort((a, b) => b.pctDiff - a.pctDiff)
      .slice(0, 10)
  }, [watchlistEntries])

  if (loading) return <p className="loading-text">Loading dashboard...</p>
  if (error) return <div className="alert alert-error">❌ {error}</div>

  return (
    <div className="dashboard">
      <div className="dashboard-stats">
        <div className="stat-card">
          <div className="stat-label">Holdings</div>
          <div className="stat-value">{portfolio.stockCount}</div>
          <div className="stat-sub">stocks</div>
        </div>
        <div className="stat-card">
          <div className="stat-label">Total Value</div>
          <div className="stat-value">${portfolio.totalValue.toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}</div>
        </div>
        <div className="stat-card">
          <div className="stat-label">Profit / Loss</div>
          <div className={`stat-value ${portfolio.totalPL >= 0 ? 'positive' : 'negative'}`}>
            {portfolio.totalPL >= 0 ? '+' : '−'}${Math.abs(portfolio.totalPL).toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
          </div>
        </div>
      </div>

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
                    <td><strong>{item.symbol}</strong></td>
                    <td>{item.price !== null ? `$${item.price.toFixed(2)}` : '—'}</td>
                    <td>{item.sma !== null ? `$${item.sma.toFixed(2)}` : '—'}</td>
                    <td style={{ color: (item.pctDiff as number) >= 0 ? '#4caf50' : '#f44336', fontWeight: 600 }}>
                      {(item.pctDiff as number) >= 0 ? '+' : ''}{(item.pctDiff as number).toFixed(2)}%
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>

        <div className="manager-card">
          <h2>Best Watchlist — 50SMA</h2>
          <p className="dashboard-list-desc">Watchlist stocks trading furthest above their 50-day moving average</p>
          {bestWatchlist.length === 0 ? (
            <p className="empty-text">No SMA data available for watchlist.</p>
          ) : (
            <table className="holdings-table compact">
              <thead>
                <tr>
                  <th>Symbol</th>
                  <th>Price</th>
                  <th>50SMA</th>
                  <th>Difference</th>
                </tr>
              </thead>
              <tbody>
                {bestWatchlist.map((item) => (
                  <tr key={item.symbol}>
                    <td><strong>{item.symbol}</strong></td>
                    <td>{item.price !== null ? `$${item.price.toFixed(2)}` : '—'}</td>
                    <td>{item.sma !== null ? `$${item.sma.toFixed(2)}` : '—'}</td>
                    <td style={{ color: (item.pctDiff as number) >= 0 ? '#4caf50' : '#f44336', fontWeight: 600 }}>
                      {(item.pctDiff as number) >= 0 ? '+' : ''}{(item.pctDiff as number).toFixed(2)}%
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </div>

    </div>
  )
}
