import { useEffect, useMemo, useState } from 'react'
import { apiClient } from '../services/api'
import { calculateSMA } from '../utils/sma'

interface PriceHistoryPoint {
  date: string
  close: number | null
  volume: number | null
}

interface PriceChartProps {
  symbol: string
  onLoading: (loading: boolean) => void
}

function buildPath(points: Array<{ x: number; y: number | null }>) {
  const filtered = points.filter((point) => point.y !== null) as Array<{
    x: number
    y: number
  }>
  if (filtered.length === 0) {
    return ''
  }
  return filtered.map((point, index) => `${index === 0 ? 'M' : 'L'} ${point.x} ${point.y}`).join(' ')
}

export default function PriceChart({ symbol, onLoading }: PriceChartProps) {
  const [history, setHistory] = useState<PriceHistoryPoint[]>([])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [smaPeriod, setSmaPeriod] = useState<50 | 150>(150)

  useEffect(() => {
    if (!symbol) {
      setHistory([])
      return
    }

    const loadHistory = async () => {
      try {
        setLoading(true)
        setError(null)
        onLoading(true)

        const data = await apiClient.getPriceHistory(symbol, 300)
        setHistory(data)
      } catch (err) {
        setError(err instanceof Error ? err.message : 'Failed to load price history')
      } finally {
        setLoading(false)
        onLoading(false)
      }
    }

    loadHistory()
  }, [symbol])

  const trimmedHistory = useMemo(() => {
    if (history.length <= smaPeriod) {
      return history
    }
    return history.slice(history.length - smaPeriod)
  }, [history, smaPeriod])

  const sma = useMemo(() => calculateSMA(history, smaPeriod), [history, smaPeriod])

  const priceValues = trimmedHistory.map((item) => item.close).filter((v): v is number => v !== null)
  const latestPrice = priceValues[priceValues.length - 1]

  const latestSma = sma[history.length - 1]

  const chartData = useMemo(() => {
    const width = 1040
    const height = 260
    const left = 50
    const right = 20
    const top = 20
    const bottom = 20
    const plotWidth = width - left - right
    const plotHeight = height - top - bottom

    const closeValues = trimmedHistory.map((item) => item.close)
    const validValues = closeValues.filter((val): val is number => val !== null)
    const minValue = Math.min(...validValues, 0)
    const maxValue = Math.max(...validValues, 0)
    const priceRange = maxValue - minValue || 1

    const points = trimmedHistory.map((item, index) => {
      const x = left + (plotWidth * index) / Math.max(trimmedHistory.length - 1, 1)
      const y = item.close !== null ? top + plotHeight - ((item.close - minValue) / priceRange) * plotHeight : null
      return { x, y }
    })

    const smaPoints = trimmedHistory.map((_, index) => {
      const globalIndex = history.length - trimmedHistory.length + index
      const value = sma[globalIndex]
      const x = left + (plotWidth * index) / Math.max(trimmedHistory.length - 1, 1)
      const y = value !== null ? top + plotHeight - ((value - minValue) / priceRange) * plotHeight : null
      return { x, y }
    })

    const volumeValues = trimmedHistory.map((item) => item.volume ?? 0)
    const maxVolume = Math.max(...volumeValues, 1)
    const volumeHeight = 100
    const volumeTop = height + 40
    const volumePlotHeight = volumeHeight - 20

    const volumeBars = trimmedHistory.map((item, index) => {
      const x = left + (plotWidth * index) / Math.max(trimmedHistory.length - 1, 1)
      const barWidth = Math.max(4, plotWidth / trimmedHistory.length - 2)
      const volume = item.volume ?? 0
      const barHeight = (volume / maxVolume) * volumePlotHeight
      const y = volumeTop + volumePlotHeight - barHeight
      
      // Determine color based on price movement
      let color = '#8fb9ff' // default neutral
      if (index > 0) {
        const prevClose = trimmedHistory[index - 1].close
        const currClose = item.close
        if (prevClose !== null && currClose !== null) {
          color = currClose >= prevClose ? '#4caf50' : '#f44336' // green up, red down
        }
      }
      
      return { x: x - barWidth / 2, y, width: barWidth, height: barHeight, color }
    })

    return {
      width: 1100,
      height: volumeTop + volumeHeight,
      points,
      smaPoints,
      volumeBars,
      left,
      right,
      top,
      bottom,
      pricePlotHeight: plotHeight,
    }
  }, [trimmedHistory, history.length, sma])

  if (!symbol) {
    return <p className="chart-message">Select a watchlist symbol to display the Simple Moving Average chart.</p>
  }

  if (loading) {
    return <p className="chart-message">Loading chart for {symbol}...</p>
  }

  if (error) {
    return <div className="alert alert-error">{error}</div>
  }

  if (trimmedHistory.length === 0) {
    return <p className="chart-message">No historical price data available for {symbol}.</p>
  }

  return (
    <div className="price-chart-card">
      <div className="chart-summary">
        <div>
          <span className="chart-symbol">{symbol}</span>
          <span className="chart-value">{latestPrice ? `$${latestPrice.toFixed(2)}` : 'Price unavailable'}</span>
        </div>
        <div>
          <span className="chart-detail">{smaPeriod}-day SMA: {latestSma ? `$${latestSma.toFixed(2)}` : 'N/A'}</span>
          <span className="chart-detail">Last: {trimmedHistory[trimmedHistory.length - 1]?.date}</span>
          <div className="sma-selector">
            <button
              className={`sma-button ${smaPeriod === 50 ? 'active' : ''}`}
              onClick={() => setSmaPeriod(50)}
            >
              50-day
            </button>
            <button
              className={`sma-button ${smaPeriod === 150 ? 'active' : ''}`}
              onClick={() => setSmaPeriod(150)}
            >
              150-day
            </button>
          </div>
        </div>
      </div>
      <div className="chart-frame">
        <svg viewBox={`0 0 ${chartData.width} ${chartData.height}`} className="chart-svg">
          <rect x="0" y="0" width={chartData.width} height={chartData.height} fill="#ffffff" rx="18" />
          <line x1={chartData.left} y1={chartData.top} x2={chartData.left} y2={chartData.top + chartData.pricePlotHeight} stroke="#e1e7f1" strokeWidth="1" />
          <line x1={chartData.left} y1={chartData.top + chartData.pricePlotHeight} x2={chartData.width - chartData.right} y2={chartData.top + chartData.pricePlotHeight} stroke="#e1e7f1" strokeWidth="1" />
          <path d={buildPath(chartData.points)} fill="none" stroke="#2f5ce4" strokeWidth="2" />
          <path d={buildPath(chartData.smaPoints)} fill="none" stroke="#ff9800" strokeWidth="2" strokeDasharray="8 6" />
          {chartData.volumeBars.map((bar, index) => (
            <rect
              key={index}
              x={bar.x}
              y={bar.y}
              width={bar.width}
              height={bar.height}
              fill={bar.color}
              opacity="0.85"
            />
          ))}
        </svg>
      </div>
      <div className="chart-legend">
        <span className="legend-item">
          <span className="legend-swatch price-line" /> Closing Price
        </span>
        <span className="legend-item">
          <span className="legend-swatch sma-line" /> {smaPeriod}-day SMA
        </span>
        <span className="legend-item">
          <span className="legend-swatch" style={{ background: '#4caf50' }} /> Volume Up
        </span>
        <span className="legend-item">
          <span className="legend-swatch" style={{ background: '#f44336' }} /> Volume Down
        </span>
      </div>
    </div>
  )
}
