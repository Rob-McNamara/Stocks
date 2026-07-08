import { useState, useEffect, useRef } from 'react'
import './App.css'
import { apiClient } from './services/api'
import { getActiveHoldingSymbols } from './utils/holdings'
import WatchlistManager from './components/WatchlistManager'
import ConfigPanel from './components/ConfigPanel'
import HoldingsManager from './components/HoldingsManager'
import EventLogViewer from './components/EventLogViewer'
import Dashboard from './components/Dashboard'
import SoldStocks from './components/SoldStocks'
import Transactions from './components/Transactions'
import Analysis from './components/Analysis'

type Tab = 'dashboard' | 'watchlist' | 'holdings' | 'analysis' | 'sold' | 'transactions' | 'events' | 'config'

function App() {
  const [activeTab, setActiveTab] = useState<Tab>('dashboard')
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [holdingsVersion, setHoldingsVersion] = useState(0)
  const [configVersion, setConfigVersion] = useState(0)
  const [watchlistFocusSymbol, setWatchlistFocusSymbol] = useState<string | null>(null)
  const [holdingsPrefill, setHoldingsPrefill] = useState<{ symbol: string; price?: number; notes?: string; customFields?: Record<string, string> } | null>(null)
  // Set when a "Move to Holdings" transaction is saved — tells the watchlist
  // it is now safe to remove the symbol's memberships.
  const [watchlistRemoveSymbol, setWatchlistRemoveSymbol] = useState<string | null>(null)
  const startupRefreshTriggered = useRef(false)

  const handleNavigateToWatchlist = (symbol: string) => {
    setWatchlistFocusSymbol(symbol)
    setActiveTab('watchlist')
  }

  const handleMoveToHoldings = (data: { symbol: string; price?: number; notes?: string; customFields?: Record<string, string> }) => {
    setHoldingsPrefill(data)
    setActiveTab('holdings')
  }

  useEffect(() => {
    testBackendConnection()
  }, [])

  // On first load, refresh all prices and dividends in the background
  useEffect(() => {
    if (startupRefreshTriggered.current) return
    startupRefreshTriggered.current = true

    const doStartupRefresh = async () => {
      try {
        const holdings = await apiClient.getHoldings()
        const holdingSymbols = getActiveHoldingSymbols(holdings)

        // Run all refreshes in parallel, bump version once when all complete
        const refreshes = [
          apiClient.getWatchlistPrices().catch((err) => console.error('Watchlist price refresh failed:', err)),
          apiClient.refreshDividends().catch((err) => console.error('Dividend refresh failed:', err)),
        ]
        if (holdingSymbols.length > 0) {
          refreshes.push(apiClient.getCurrentPrices(holdingSymbols).catch((err) => console.error('Holdings price refresh failed:', err)))
        }
        Promise.allSettled(refreshes).then(() => setHoldingsVersion((v) => v + 1))
      } catch (err) {
        console.error('Startup refresh failed:', err)
      }
    }
    doStartupRefresh()
  }, [])

  const testBackendConnection = async () => {
    const healthy = await apiClient.checkHealth()
    if (!healthy) {
      setError('Cannot connect to backend. Make sure the API server is running.')
    }
  }

  return (
    <div className="app-container">
      <header className="app-header">
        <h1>Stock Manager</h1>
        <p className="subtitle">Manage ASX Stock Price Daemon Settings</p>
      </header>

      {error && (
        <div className="error-banner">
          ⚠️ {error}
        </div>
      )}

      <nav className="tab-navigation">
        <button
          className={`tab-button ${activeTab === 'dashboard' ? 'active' : ''}`}
          onClick={() => setActiveTab('dashboard')}
          disabled={loading}
        >
          Dashboard
        </button>
        <button
          className={`tab-button ${activeTab === 'watchlist' ? 'active' : ''}`}
          onClick={() => setActiveTab('watchlist')}
          disabled={loading}
        >
          Watchlist
        </button>
        <button
          className={`tab-button ${activeTab === 'holdings' ? 'active' : ''}`}
          onClick={() => setActiveTab('holdings')}
          disabled={loading}
        >
          Holdings
        </button>
        <button
          className={`tab-button ${activeTab === 'analysis' ? 'active' : ''}`}
          onClick={() => setActiveTab('analysis')}
          disabled={loading}
        >
          Analysis
        </button>
        <button
          className={`tab-button ${activeTab === 'sold' ? 'active' : ''}`}
          onClick={() => setActiveTab('sold')}
          disabled={loading}
        >
          Sold Stocks
        </button>
        <button
          className={`tab-button ${activeTab === 'transactions' ? 'active' : ''}`}
          onClick={() => setActiveTab('transactions')}
          disabled={loading}
        >
          Transactions
        </button>
        <button
          className={`tab-button ${activeTab === 'events' ? 'active' : ''}`}
          onClick={() => setActiveTab('events')}
          disabled={loading}
        >
          Event Log
        </button>
        <button
          className={`tab-button ${activeTab === 'config' ? 'active' : ''}`}
          onClick={() => setActiveTab('config')}
          disabled={loading}
        >
          Configuration
        </button>
      </nav>

      <main className="app-content">
        <div style={{ display: activeTab === 'dashboard' ? 'block' : 'none' }}>
          <Dashboard onLoading={setLoading} holdingsVersion={holdingsVersion} onNavigateToWatchlist={handleNavigateToWatchlist} />
        </div>
        <div style={{ display: activeTab === 'watchlist' ? 'block' : 'none' }}>
          <WatchlistManager onLoading={setLoading} initialSymbol={watchlistFocusSymbol} onInitialSymbolConsumed={() => setWatchlistFocusSymbol(null)} onMoveToHoldings={handleMoveToHoldings} removeSymbolRequest={watchlistRemoveSymbol} onRemoveSymbolConsumed={() => setWatchlistRemoveSymbol(null)} />
        </div>
        <div style={{ display: activeTab === 'holdings' ? 'block' : 'none' }}>
          <HoldingsManager onLoading={setLoading} onTransactionsChanged={() => setHoldingsVersion((v) => v + 1)} configVersion={configVersion} prefill={holdingsPrefill} onPrefillConsumed={() => setHoldingsPrefill(null)} onPrefillSaved={(symbol) => setWatchlistRemoveSymbol(symbol)} />
        </div>
        <div style={{ display: activeTab === 'analysis' ? 'block' : 'none' }}>
          <Analysis onLoading={setLoading} holdingsVersion={holdingsVersion} />
        </div>
        <div style={{ display: activeTab === 'sold' ? 'block' : 'none' }}>
          <SoldStocks onLoading={setLoading} holdingsVersion={holdingsVersion} />
        </div>
        <div style={{ display: activeTab === 'transactions' ? 'block' : 'none' }}>
          <Transactions onLoading={setLoading} holdingsVersion={holdingsVersion} />
        </div>
        <div style={{ display: activeTab === 'events' ? 'block' : 'none' }}>
          <EventLogViewer onLoading={setLoading} />
        </div>
        <div style={{ display: activeTab === 'config' ? 'block' : 'none' }}>
          <ConfigPanel onLoading={setLoading} onConfigChanged={() => setConfigVersion((v) => v + 1)} />
        </div>
      </main>

      <footer className="app-footer">
        <p>Stock Daemon v1.0 | Changes apply on next daemon cycle</p>
      </footer>
    </div>
  )
}

export default App
