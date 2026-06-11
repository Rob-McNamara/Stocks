import { useEffect, useState } from 'react'
import { apiClient } from '../services/api'

interface EventLogEntry {
  id: number
  timestamp: string
  level: string
  source: string
  event_type: string
  symbol?: string | null
  details?: string | null
}

interface EventLogViewerProps {
  onLoading: (loading: boolean) => void
}

export default function EventLogViewer({ onLoading }: EventLogViewerProps) {
  const [events, setEvents] = useState<EventLogEntry[]>([])
  const [total, setTotal] = useState(0)
  const [page, setPage] = useState(1)
  const [size] = useState(20)
  const [level, setLevel] = useState('')
  const [source, setSource] = useState('')
  const [eventType, setEventType] = useState('')
  const [symbol, setSymbol] = useState('')
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    loadEvents({ page, level, source, eventType, symbol })
  }, [page])

  const loadEvents = async (opts: { page: number; level: string; source: string; eventType: string; symbol: string }) => {
    try {
      setLoading(true)
      setError(null)
      onLoading(true)
      const result = await apiClient.getEventLog({
        page: opts.page,
        size,
        level: opts.level || undefined,
        source: opts.source || undefined,
        event_type: opts.eventType || undefined,
        symbol: opts.symbol || undefined,
      })
      setEvents(result.items)
      setTotal(result.total)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load event log')
    } finally {
      setLoading(false)
      onLoading(false)
    }
  }

  const handleFilterSubmit = async (event: React.FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    setPage(1)
    await loadEvents({ page: 1, level, source, eventType, symbol })
  }

  const handleResetFilters = async () => {
    setLevel('')
    setSource('')
    setEventType('')
    setSymbol('')
    setPage(1)
    await loadEvents({ page: 1, level: '', source: '', eventType: '', symbol: '' })
  }

  const handlePrevPage = () => {
    if (page > 1) {
      setPage(page - 1)
    }
  }

  const handleNextPage = () => {
    if (page * size < total) {
      setPage(page + 1)
    }
  }

  return (
    <div className="event-log-viewer">
      <div className="manager-card event-log-card">
        <div className="card-header">
          <h2>Event Log</h2>
          <p className="event-log-summary">Showing {events.length} of {total} events</p>
        </div>

        <form className="event-log-filters" onSubmit={handleFilterSubmit}>
          <div className="filter-group">
            <label htmlFor="level">Level</label>
            <select id="level" value={level} onChange={(e) => setLevel(e.target.value)}>
              <option value="">All</option>
              <option value="info">Info</option>
              <option value="warn">Warn</option>
              <option value="error">Error</option>
            </select>
          </div>
          <div className="filter-group">
            <label htmlFor="source">Source</label>
            <input
              id="source"
              value={source}
              onChange={(e) => setSource(e.target.value)}
              placeholder="daemon, api, watchlist"
            />
          </div>
          <div className="filter-group">
            <label htmlFor="event_type">Event Type</label>
            <input
              id="event_type"
              value={eventType}
              onChange={(e) => setEventType(e.target.value)}
              placeholder="price_update, dividend_fetch"
            />
          </div>
          <div className="filter-group">
            <label htmlFor="symbol">Symbol</label>
            <input
              id="symbol"
              value={symbol}
              onChange={(e) => setSymbol(e.target.value.toUpperCase())}
              placeholder="BHP.AX"
            />
          </div>
          <div className="filter-actions">
            <button type="submit" className="btn btn-primary" disabled={loading}>
              Apply Filters
            </button>
            <button type="button" className="btn btn-secondary" onClick={handleResetFilters} disabled={loading}>
              Reset
            </button>
          </div>
        </form>

        {error && (
          <div className="alert alert-error">
            ❌ {error}
          </div>
        )}

        <div className="event-log-table-wrapper">
          <table className="event-log-table">
            <thead>
              <tr>
                <th>ID</th>
                <th>Timestamp</th>
                <th>Level</th>
                <th>Source</th>
                <th>Event Type</th>
                <th>Symbol</th>
                <th>Details</th>
              </tr>
            </thead>
            <tbody>
              {events.length === 0 ? (
                <tr>
                  <td colSpan={7} className="empty-text">
                    {loading ? 'Loading events...' : 'No events found for the selected filters.'}
                  </td>
                </tr>
              ) : (
                events.map((event) => (
                  <tr key={event.id}>
                    <td>{event.id}</td>
                    <td>{new Date(event.timestamp).toLocaleString()}</td>
                    <td className={`event-level ${event.level}`}>{event.level}</td>
                    <td>{event.source}</td>
                    <td>{event.event_type}</td>
                    <td>{event.symbol || '—'}</td>
                    <td>{event.details || '—'}</td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>

        <div className="event-log-pagination">
          <button className="btn btn-secondary" onClick={handlePrevPage} disabled={loading || page === 1}>
            Previous
          </button>
          <span>
            Page {page} of {Math.max(1, Math.ceil(total / size))}
          </span>
          <button className="btn btn-secondary" onClick={handleNextPage} disabled={loading || page * size >= total}>
            Next
          </button>
        </div>
      </div>
    </div>
  )
}
