import { useState, useEffect } from 'react'
import './App.css'
import WatchlistManager from './components/WatchlistManager'
import ConfigPanel from './components/ConfigPanel'
import HoldingsManager from './components/HoldingsManager'
import EventLogViewer from './components/EventLogViewer'

type Tab = 'watchlist' | 'config' | 'holdings' | 'events'

function App() {
  const [activeTab, setActiveTab] = useState<Tab>('watchlist')
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    // Test connection to backend on mount
    testBackendConnection()
  }, [])

  const testBackendConnection = async () => {
    try {
      const response = await fetch('http://localhost:3001/api/health')
      if (!response.ok) {
        setError('Backend server not available')
      }
    } catch (err) {
      setError('Cannot connect to backend. Make sure the API server is running.')
    }
  }

  return (
    <div className="app-container">
      <header className="app-header">
        <h1>Stock Daemon Configuration</h1>
        <p className="subtitle">Manage ASX Stock Price Daemon Settings</p>
      </header>

      {error && (
        <div className="error-banner">
          ⚠️ {error}
        </div>
      )}

      <nav className="tab-navigation">
        <button
          className={`tab-button ${activeTab === 'watchlist' ? 'active' : ''}`}
          onClick={() => setActiveTab('watchlist')}
          disabled={loading}
        >
          Watchlist Manager
        </button>
        <button
          className={`tab-button ${activeTab === 'holdings' ? 'active' : ''}`}
          onClick={() => setActiveTab('holdings')}
          disabled={loading}
        >
          Holdings
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
        {activeTab === 'watchlist' && (
          <WatchlistManager onLoading={setLoading} />
        )}
        {activeTab === 'holdings' && (
          <HoldingsManager onLoading={setLoading} />
        )}
        {activeTab === 'events' && (
          <EventLogViewer onLoading={setLoading} />
        )}
        {activeTab === 'config' && (
          <ConfigPanel onLoading={setLoading} />
        )}
      </main>

      <footer className="app-footer">
        <p>Stock Daemon v1.0 | Changes apply on next daemon cycle</p>
      </footer>
    </div>
  )
}

export default App
