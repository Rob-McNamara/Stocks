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

export default function HoldingsManager({ onLoading, onTransactionsChanged }: { onLoading: (loading: boolean) => void; onTransactionsChanged?: () => void }) {
  const [transactions, setTransactions] = useState<HoldingTransaction[]>([])
  const [currentPrices, setCurrentPrices] = useState<Record<string, number | null>>({})
  const [manualPriceSymbols, setManualPriceSymbols] = useState<Set<string>>(new Set())
  const [smaPrices, setSmaPrices] = useState<Record<string, number | null>>({})
  const [symbol, setSymbol] = useState('')
  const [transactionType, setTransactionType] = useState<HoldingTransaction['transaction_type']>('purchase')
  const [date, setDate] = useState(() => new Date().toISOString().slice(0, 10))
  const [quantity, setQuantity] = useState('')
  const [price, setPrice] = useState('')
  const [amount, setAmount] = useState('')
  const [brokerage, setBrokerage] = useState('')
  const [notes, setNotes] = useState('')
  const [editingId, setEditingId] = useState<number | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [success, setSuccess] = useState<string | null>(null)

  useEffect(() => {
    loadHoldings()
  }, [])

  const loadHoldings = async () => {
    try {
      setLoading(true)
      setError(null)
      onLoading(true)
      const data = await apiClient.getHoldings()
      setTransactions(data)

      // Fetch current prices for all unique symbols
      const symbols = Array.from(new Set(data.map((tx) => tx.symbol)))
      if (symbols.length > 0) {
        try {
          const [prices, config] = await Promise.all([
            apiClient.getCurrentPrices(symbols),
            apiClient.getConfig(),
          ])
          const priceMap = prices.reduce(
            (acc, p) => {
              acc[p.symbol] = p.price
              return acc
            },
            {} as Record<string, number | null>
          )
          // Fill in manual prices for symbols with no auto-fetched price
          const manualSymbols = new Set<string>()
          symbols.forEach((sym) => {
            if (!priceMap[sym]) {
              const manual = config[`manual_price_${sym}`]
              if (manual) {
                priceMap[sym] = parseFloat(manual)
                manualSymbols.add(sym)
              }
            }
          })
          setCurrentPrices(priceMap)
          setManualPriceSymbols(manualSymbols)
          
          // Fetch and calculate SMA for each symbol
          const smaMap: Record<string, number | null> = {}
          for (const sym of symbols) {
            try {
              const history = await apiClient.getPriceHistory(sym, 300)
              const smaArray = calculateSMA(history, 150)
              const sma150 = getLatestSMA(smaArray)
              smaMap[sym] = sma150
            } catch (err) {
              // If SMA calculation fails, just leave it as null
              smaMap[sym] = null
            }
          }
          setSmaPrices(smaMap)
        } catch (err) {
          // Continue even if price fetch fails
          console.error('Failed to fetch prices:', err)
        }
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

      payload.quantity = parsedQuantity
      payload.price = parsedPrice
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
      const result = editingId
        ? await apiClient.updateHoldingTransaction(editingId, payload)
        : await apiClient.addHoldingTransaction(payload)

      setTransactions((current) => {
        if (editingId) {
          return current.map((tx) => (tx.id === editingId ? result : tx))
        }
        return [result, ...current]
      })

      onTransactionsChanged?.()
      setSuccess(editingId ? 'Transaction updated successfully' : 'Transaction recorded successfully')
      setSymbol('')
      setQuantity('')
      setPrice('')
      setAmount('')
      setBrokerage('')
      setNotes('')
      setDate(new Date().toISOString().slice(0, 10))
      setEditingId(null)
      setTimeout(() => setSuccess(null), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save transaction')
    } finally {
      setLoading(false)
      onLoading(false)
    }
  }

  const startEditingTransaction = (tx: HoldingTransaction) => {
    setEditingId(tx.id)
    setSymbol(tx.symbol)
    setTransactionType(tx.transaction_type)
    setDate(tx.date)
    setQuantity(tx.quantity !== null ? tx.quantity.toString() : '')
    setPrice(tx.price !== null ? tx.price.toString() : '')
    setAmount(tx.amount !== null ? tx.amount.toString() : '')
    setBrokerage(tx.brokerage !== null ? tx.brokerage.toString() : '')
    setNotes(tx.notes ?? '')
    setSuccess(null)
    setError(null)
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
  }

  const handleDeleteTransaction = async (id: number) => {
    if (editingId === id) {
      setEditingId(null)
      setSymbol('')
      setTransactionType('purchase')
      setDate(new Date().toISOString().slice(0, 10))
      setQuantity('')
      setPrice('')
      setAmount('')
      setBrokerage('')
      setNotes('')
    }

    if (!confirm('Delete this transaction?')) {
      return
    }

    try {
      setLoading(true)
      setError(null)
      onLoading(true)
      await apiClient.removeHoldingTransaction(id)
      setTransactions((current) => current.filter((tx) => tx.id !== id))
      onTransactionsChanged?.()
      setSuccess('Transaction deleted')
      setTimeout(() => setSuccess(null), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to delete transaction')
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
      const symbols = Array.from(new Set(transactions.map((tx) => tx.symbol)))
      const prices = await apiClient.getCurrentPrices(symbols)
      const priceMap = prices.reduce((acc, p) => {
        acc[p.symbol] = p.price
        return acc
      }, {} as Record<string, number | null>)
      setCurrentPrices(priceMap)
      
      // Fetch and calculate SMA for each symbol
      const smaMap: Record<string, number | null> = {}
      for (const sym of symbols) {
        try {
          const history = await apiClient.getPriceHistory(sym, 300)
          const smaArray = calculateSMA(history, 150)
          const sma150 = getLatestSMA(smaArray)
          smaMap[sym] = sma150
        } catch (err) {
          smaMap[sym] = null
        }
      }
      setSmaPrices(smaMap)

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

  const summary = useMemo(() => {
    const groupedBySymbol: Record<string, HoldingTransaction[]> = {}
    transactions.forEach((tx) => {
      if (!groupedBySymbol[tx.symbol]) groupedBySymbol[tx.symbol] = []
      groupedBySymbol[tx.symbol].push(tx)
    })

    return Object.entries(groupedBySymbol).map(([symbol, txs]) => {
      const sorted = [...txs].sort((a, b) => a.date.localeCompare(b.date) || a.id - b.id)

      // FIFO: track remaining lots to get correct cost basis of remaining shares
      const lots: Array<{ quantity: number; price: number }> = []
      let dividends = 0
      let dividendsFromTotal = 0

      sorted.forEach((tx) => {
        if (tx.transaction_type === 'purchase' && tx.quantity && tx.price) {
          lots.push({ quantity: tx.quantity, price: tx.price })
        } else if (tx.transaction_type === 'sale' && tx.quantity) {
          let remaining = tx.quantity
          while (remaining > 0 && lots.length > 0) {
            const used = Math.min(remaining, lots[0].quantity)
            lots[0].quantity -= used
            remaining -= used
            if (lots[0].quantity <= 0) lots.shift()
          }
        }
        if (tx.dividends_total > 0) dividendsFromTotal = tx.dividends_total
        else if (tx.transaction_type === 'dividend' && tx.amount) dividends += tx.amount
      })

      const shares = lots.reduce((s, l) => s + l.quantity, 0)
      if (shares <= 0) return null

      const invested = lots.reduce((s, l) => s + l.quantity * l.price, 0)
      const currentPrice = currentPrices[symbol] || null
      const sma150 = smaPrices[symbol] || null
      const currentValue = currentPrice ? shares * currentPrice : 0
      const avgCost = shares > 0 ? invested / shares : null

      return {
        symbol,
        shares,
        invested,
        dividends: dividendsFromTotal > 0 ? dividendsFromTotal : dividends,
        currentPrice,
        sma150,
        currentValue,
        avgCost,
      }
    }).filter((item): item is NonNullable<typeof item> => item !== null)
  }, [transactions, currentPrices, smaPrices])

  const dividendTotalsBySymbol = useMemo(() => {
    return transactions.reduce<Record<string, number>>((acc, tx) => {
      if (acc[tx.symbol] === undefined) {
        acc[tx.symbol] = 0
      }
      if (tx.dividends_total > 0) {
        acc[tx.symbol] = tx.dividends_total
      } else if (tx.transaction_type === 'dividend' && tx.amount) {
        acc[tx.symbol] += tx.amount
      }
      return acc
    }, {})
  }, [transactions])

  const remainingQuantities = useMemo(() => {
    const result: Record<number, number> = {}
    const groupedBySymbol: Record<string, HoldingTransaction[]> = {}

    transactions.forEach((tx) => {
      if (!groupedBySymbol[tx.symbol]) groupedBySymbol[tx.symbol] = []
      groupedBySymbol[tx.symbol].push(tx)
    })

    Object.values(groupedBySymbol).forEach((group) => {
      const sorted = [...group].sort((a, b) => a.date.localeCompare(b.date) || a.id - b.id)
      const lots: Array<{ id: number; quantity: number }> = []

      sorted.forEach((tx) => {
        if (tx.transaction_type === 'purchase' && tx.quantity !== null) {
          lots.push({ id: tx.id, quantity: tx.quantity })
          result[tx.id] = tx.quantity
        } else if (tx.transaction_type === 'sale' && tx.quantity !== null) {
          let remaining = tx.quantity
          while (remaining > 0 && lots.length > 0) {
            const lot = lots[0]
            const used = Math.min(remaining, lot.quantity)
            lot.quantity -= used
            result[lot.id] = lot.quantity
            remaining -= used
            if (lot.quantity <= 0) lots.shift()
          }
        }
      })
    })

    return result
  }, [transactions])

  const transactionDetails = useMemo(() => {
    const details: Record<number, { currentValue: number | null; profitLoss: number | null }> = {}
    const groupedBySymbol: Record<string, HoldingTransaction[]> = {}

    transactions.forEach((tx) => {
      if (!groupedBySymbol[tx.symbol]) {
        groupedBySymbol[tx.symbol] = []
      }
      groupedBySymbol[tx.symbol].push(tx)
    })

    Object.values(groupedBySymbol).forEach((group) => {
      const sortedGroup = [...group].sort((a, b) => {
        const dateCompare = a.date.localeCompare(b.date)
        return dateCompare !== 0 ? dateCompare : a.id - b.id
      })
      const lots: Array<{ quantity: number; totalCost: number }> = []
      const currentPrice = currentPrices[sortedGroup[0].symbol] ?? null

      sortedGroup.forEach((tx) => {
        let currentValue: number | null = null
        let profitLoss: number | null = null

        if (tx.transaction_type === 'purchase' && tx.quantity !== null && tx.price !== null) {
          const remaining = remainingQuantities[tx.id] ?? 0
          if (currentPrice !== null && remaining > 0) {
            currentValue = remaining * currentPrice
            profitLoss = currentValue - (remaining * tx.price + (tx.brokerage ?? 0))
          }
          lots.push({ quantity: tx.quantity, totalCost: tx.quantity * tx.price })
        } else if (tx.transaction_type === 'sale' && tx.quantity !== null && tx.price !== null) {
          let remaining = tx.quantity
          let costBasis = 0

          while (remaining > 0 && lots.length > 0) {
            const lot = lots[0]
            const used = Math.min(remaining, lot.quantity)
            costBasis += used * (lot.totalCost / lot.quantity)
            remaining -= used
            if (used >= lot.quantity) {
              lots.shift()
            } else {
              lots[0].quantity -= used
            }
          }

          profitLoss = tx.quantity * tx.price - (tx.brokerage ?? 0) - costBasis
        }

        details[tx.id] = { currentValue, profitLoss }
      })
    })

    return details
  }, [transactions, currentPrices, remainingQuantities])

  const [sortColumn, setSortColumn] = useState<string | null>(null)
  const [sortDirection, setSortDirection] = useState<'asc' | 'desc'>('asc')

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
      (tx) => tx.transaction_type === 'purchase' && (remainingQuantities[tx.id] ?? 0) > 0
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
        aVal = transactionDetails[a.id]?.currentValue ?? -Infinity
        bVal = transactionDetails[b.id]?.currentValue ?? -Infinity
      } else if (sortColumn === 'profitLoss') {
        aVal = transactionDetails[a.id]?.profitLoss ?? -Infinity
        bVal = transactionDetails[b.id]?.profitLoss ?? -Infinity
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
  }, [transactions, sortColumn, sortDirection, transactionDetails, dividendTotalsBySymbol, remainingQuantities])

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
              placeholder="ASX symbol (e.g. CBA, BHP)"
              className="symbol-input"
              disabled={loading}
              maxLength={6}
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
              <input
                type="number"
                min="0"
                step="any"
                value={price}
                onChange={(e) => setPrice(e.target.value)}
                placeholder="Price per share"
                className="symbol-input"
                disabled={loading}
              />
            </div>
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
            <button type="submit" className="btn btn-primary" disabled={loading}>
              {loading ? 'Saving...' : editingId ? 'Save Changes' : 'Record Transaction'}
            </button>
            {editingId !== null && (
              <button type="button" className="btn btn-outline btn-small" onClick={cancelEditing} disabled={loading}>
                Cancel
              </button>
            )}
          </div>
        </form>
      </div>

      {error && <div className="alert alert-error">❌ {error}</div>}
      {success && <div className="alert alert-success">✓ {success}</div>}

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
                    return (
                      <span style={{ fontSize: 13, color: totalPL >= 0 ? '#4caf50' : '#f44336', fontWeight: 600 }}>
                        P/L: {totalPL >= 0 ? '+' : '-'}${Math.abs(totalPL).toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}
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
          <>
            <div className="holdings-summary-grid">
              {summary.map((item) => (
                <div key={item.symbol} className="holdings-summary-card">
                  <strong>{item.symbol}</strong>
                  <div style={{ color: manualPriceSymbols.has(item.symbol) ? '#2196f3' : undefined }}>
                    {item.shares % 1 === 0 ? item.shares.toFixed(0) : item.shares.toFixed(2)}@{item.currentPrice ? `$${item.currentPrice.toFixed(2)}` : '—'}
                    {manualPriceSymbols.has(item.symbol) && <span style={{ fontSize: 11, marginLeft: 4 }}>(manual)</span>}
                  </div>
                  {item.sma150 !== null && (
                    <div style={{ color: item.currentPrice !== null && item.sma150 > item.currentPrice ? '#f44336' : undefined }}>
                      150SMA: ${item.sma150.toFixed(2)}
                    </div>
                  )}
                  <div>Current value: ${item.currentValue.toFixed(2)}</div>
                  <div>Dividends: ${item.dividends.toFixed(2)}</div>
                  {(() => {
                    const pl = item.currentValue - item.invested + item.dividends
                    return (
                      <div style={{ color: pl >= 0 ? '#4caf50' : '#f44336', fontWeight: 600 }}>
                        P/L: {pl >= 0 ? '+' : '-'}${Math.abs(pl).toFixed(2)}
                      </div>
                    )
                  })()}
                </div>
              ))}
            </div>

            <div className="holdings-table-wrapper">
              <h3>Active Holdings</h3>
              <table className="holdings-table">
                <thead>
                  <tr>
                    <th className="sortable-header" onClick={() => handleSort('symbol')}>Symbol{sortIndicator('symbol')}</th>
                    <th className="sortable-header" onClick={() => handleSort('date')}>Date{sortIndicator('date')}</th>
                    <th>Quantity</th>
                    <th>Price</th>
                    <th className="sortable-header" onClick={() => handleSort('currentValue')}>Current Value{sortIndicator('currentValue')}</th>
                    <th className="sortable-header" onClick={() => handleSort('profitLoss')}>Unrealised P/L{sortIndicator('profitLoss')}</th>
                    <th className="sortable-header" onClick={() => handleSort('dividends')}>Total Dividends{sortIndicator('dividends')}</th>
                    <th>Brokerage</th>
                    <th>Notes</th>
                    <th></th>
                  </tr>
                </thead>
                <tbody>
                  {activeTransactions.map((tx) => {
                    const details = transactionDetails[tx.id] || {
                      currentValue: null,
                      profitLoss: null,
                    }

                    return (
                      <tr key={tx.id}>
                        <td>{tx.symbol}</td>
                        <td>{new Date(tx.date).toLocaleDateString()}</td>
                        <td>{(remainingQuantities[tx.id] ?? 0).toFixed(2)}</td>
                        <td>{tx.price !== null ? `$${tx.price.toFixed(2)}` : '—'}</td>
                        <td>
                          {details.currentValue !== null
                            ? `$${details.currentValue.toFixed(2)}`
                            : '—'}
                        </td>
                        <td>
                          {details.profitLoss !== null
                            ? `${details.profitLoss >= 0 ? '+' : '-'}$${Math.abs(details.profitLoss).toFixed(2)}`
                            : '—'}
                        </td>
                        <td>
                          {dividendTotalsBySymbol[tx.symbol] !== undefined
                            ? `$${dividendTotalsBySymbol[tx.symbol].toFixed(2)}`
                            : '—'}
                        </td>
                        <td>{tx.brokerage !== null ? `$${tx.brokerage.toFixed(2)}` : '—'}</td>
                        <td>{tx.notes || '—'}</td>
                        <td>
                          <button
                            className="btn btn-secondary btn-small"
                            onClick={() => startEditingTransaction(tx)}
                            disabled={loading}
                          >
                            Edit
                          </button>
                          <button
                            className="btn btn-danger btn-small"
                            onClick={() => handleDeleteTransaction(tx.id)}
                            disabled={loading}
                          >
                            Delete
                          </button>
                        </td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>

          </>
        )}
      </div>
    </div>
  )
}
