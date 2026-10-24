import { useEffect, useState } from 'react'
import { apiClient, type RotationPeriod, type SectorHoldings as Holdings } from '../services/api'
import { labelColor, rgb, tileColor } from '../utils/heatColors'
import { formatSessionDate } from '../utils/sessionDate'

type Loaded =
  | { status: 'loading' }
  | { status: 'error'; message: string }
  | { status: 'ready'; data: Holdings }

const formatPct = (v: number | null) => (v == null ? '—' : `${v >= 0 ? '+' : ''}${v.toFixed(1)}%`)
const formatPoints = (v: number | null) => (v == null ? '—' : `${v >= 0 ? '+' : ''}${v.toFixed(1)} pp`)

/**
 * A sector tile opened up: its fund's top holdings, each with its weight and
 * how it moved over the period the map is showing.
 *
 * The parent keys this by set and symbol, so each tile's holdings are fetched
 * once per opening, and an answer for a tile that has since been closed is
 * never drawn. The figure the map is coloured by is coloured the same way
 * here, on the same scale, so a holding reads against its tile at a glance.
 */
export default function SectorHoldings({
  setName,
  symbol,
  period,
  periodLabel,
  mode,
  cap,
  onClose,
}: {
  setName: string
  symbol: string
  period: RotationPeriod
  periodLabel: string
  mode: 'relative' | 'return'
  cap: number
  onClose: () => void
}) {
  const [loaded, setLoaded] = useState<Loaded>({ status: 'loading' })

  useEffect(() => {
    let current = true
    apiClient
      .getSectorHoldings(setName, symbol)
      .then((data) => {
        if (current) setLoaded({ status: 'ready', data })
      })
      .catch((err) => {
        if (current) setLoaded({ status: 'error', message: err instanceof Error ? err.message : 'Failed to load the holdings' })
      })
    return () => {
      current = false
    }
  }, [setName, symbol])

  const data = loaded.status === 'ready' ? loaded.data : null
  const benchmarkLabel = data ? data.benchmark.name || data.benchmark.symbol : 'benchmark'

  const shaded = (value: number | null, text: string) => {
    const fill = tileColor(value, cap)
    return (
      <td className="rotation-holdings-figure" style={{ background: rgb(fill), color: labelColor(fill) }}>
        {text}
      </td>
    )
  }

  return (
    <div className="rotation-holdings" id="rotation-holdings" role="region" aria-label={`Top holdings of ${symbol}`}>
      <div className="rotation-holdings-header">
        <div>
          <strong>{symbol}</strong>
          {data && <span className="rotation-holdings-name">{data.name}</span>}
          {data && (
            <span className="rotation-holdings-note">
              Top {data.holdings.length} holdings · {data.top_weight.toFixed(1)}% of the fund
            </span>
          )}
        </div>
        <button type="button" className="btn btn-outline btn-small" onClick={onClose}>
          Close
        </button>
      </div>

      {loaded.status === 'loading' && <p className="loading-text">Loading holdings…</p>}
      {loaded.status === 'error' && <p className="rotation-holdings-error">{loaded.message}</p>}
      {data && (
        <table className="holdings-table compact rotation-holdings-table">
          <thead>
            <tr>
              <th>Code</th>
              <th>Name</th>
              <th>Weight</th>
              <th>{periodLabel} return</th>
              <th>vs {benchmarkLabel}</th>
            </tr>
          </thead>
          <tbody>
            {data.holdings.map((holding) => {
              const ret = holding.returns[period]
              const rel = holding.relative[period]
              // Blank for the same reason a tile is: no close on the day the
              // set's figures run to.
              const behind =
                data.as_of && holding.last_date && holding.last_date < data.as_of
                  ? `No price for ${formatSessionDate(data.as_of)} — the newest is ${formatSessionDate(holding.last_date)}`
                  : undefined
              return (
                <tr key={holding.symbol ?? holding.name} title={behind}>
                  <td style={{ fontWeight: 600 }}>{holding.symbol ?? '—'}</td>
                  <td>
                    {holding.name}
                    {behind && holding.last_date && (
                      <span className="rotation-holdings-stale">Last price {formatSessionDate(holding.last_date)}</span>
                    )}
                  </td>
                  <td>{holding.weight.toFixed(1)}%</td>
                  {mode === 'return' ? shaded(ret, formatPct(ret)) : <td>{formatPct(ret)}</td>}
                  {mode === 'relative' ? shaded(rel, formatPoints(rel)) : <td>{formatPoints(rel)}</td>}
                </tr>
              )
            })}
          </tbody>
        </table>
      )}
    </div>
  )
}
