import { useEffect, useRef, useState } from 'react'
import { apiClient } from '../services/api'

interface Message {
  role: string
  content: string
}

interface StockAnalysisProps {
  symbol: string
  symbolName?: string | null
  onClose: () => void
}

export default function StockAnalysis({ symbol, symbolName, onClose }: StockAnalysisProps) {
  const [messages, setMessages] = useState<Message[]>([])
  const [input, setInput] = useState('')
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const messagesEndRef = useRef<HTMLDivElement>(null)
  const inputRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    loadHistory()
  }, [symbol])

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [messages])

  const loadHistory = async () => {
    try {
      const history = await apiClient.getAnalysisHistory(symbol)
      if (history.length > 0) {
        setMessages(history.map((h) => ({ role: h.role, content: h.content })))
      } else {
        sendMessage(`Analyze ${symbol}`)
      }
    } catch {
      sendMessage(`Analyze ${symbol}`)
    }
  }

  const sendMessage = async (text: string) => {
    const userMsg: Message = { role: 'user', content: text }
    const updatedMessages = [...messages, userMsg]
    setMessages(updatedMessages)
    setInput('')
    setLoading(true)
    setError(null)

    try {
      const response = await apiClient.analyzeStock(symbol, updatedMessages)
      setMessages((prev) => [...prev, { role: 'assistant', content: response.content }])
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Analysis failed')
    } finally {
      setLoading(false)
      inputRef.current?.focus()
    }
  }

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    if (!input.trim() || loading) return
    sendMessage(input.trim())
  }

  const handleClearHistory = async () => {
    if (!confirm(`Clear all analysis history for ${symbol}?`)) return
    try {
      await apiClient.clearAnalysisHistory(symbol)
      setMessages([])
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to clear history')
    }
  }

  return (
    <div
      style={{ position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.5)', zIndex: 1000, display: 'flex', alignItems: 'center', justifyContent: 'center' }}
      onClick={onClose}
    >
      <div
        style={{ background: '#fff', borderRadius: 10, width: '90%', maxWidth: 700, height: '80vh', display: 'flex', flexDirection: 'column', boxShadow: '0 8px 32px rgba(0,0,0,0.2)' }}
        onClick={(e) => e.stopPropagation()}
      >
        <div style={{ padding: '16px 20px', borderBottom: '1px solid #e0e0e0', display: 'flex', alignItems: 'center', justifyContent: 'space-between', flexShrink: 0 }}>
          <div>
            <h3 style={{ margin: 0, fontSize: 17 }}>AI Analysis: {symbol}</h3>
            {symbolName && <div style={{ fontSize: 12, color: '#888', marginTop: 2 }}>{symbolName}</div>}
          </div>
          <div style={{ display: 'flex', gap: 8 }}>
            {messages.length > 0 && (
              <button onClick={handleClearHistory} className="btn btn-outline btn-small" style={{ fontSize: 12 }}>
                Clear History
              </button>
            )}
            <button onClick={onClose} style={{ background: 'none', border: 'none', fontSize: 20, cursor: 'pointer', padding: '0 4px', color: '#666' }}>
              &times;
            </button>
          </div>
        </div>

        <div style={{ flex: 1, overflowY: 'auto', padding: '16px 20px' }}>
          {messages.length === 0 && !loading && (
            <div style={{ textAlign: 'center', color: '#999', marginTop: 40, fontSize: 14 }}>
              Starting analysis...
            </div>
          )}
          {messages.map((msg, i) => (
            <div
              key={i}
              style={{
                marginBottom: 16,
                display: 'flex',
                flexDirection: 'column',
                alignItems: msg.role === 'user' ? 'flex-end' : 'flex-start',
              }}
            >
              <div
                style={{
                  maxWidth: '85%',
                  padding: '10px 14px',
                  borderRadius: 10,
                  fontSize: 14,
                  lineHeight: 1.6,
                  background: msg.role === 'user' ? '#e3f2fd' : '#f5f5f5',
                  color: '#333',
                  whiteSpace: 'pre-wrap',
                  wordBreak: 'break-word',
                }}
              >
                {msg.content}
              </div>
            </div>
          ))}
          {loading && (
            <div style={{ display: 'flex', alignItems: 'center', gap: 8, color: '#888', fontSize: 14, marginBottom: 16 }}>
              <span style={{ display: 'inline-block', width: 18, height: 18, border: '2px solid #ccc', borderTopColor: '#1976d2', borderRadius: '50%', animation: 'spin 1s linear infinite' }} />
              Analyzing...
            </div>
          )}
          {error && (
            <div style={{ background: '#ffebee', color: '#c62828', padding: '10px 14px', borderRadius: 8, fontSize: 13, marginBottom: 16 }}>
              {error}
            </div>
          )}
          <div ref={messagesEndRef} />
        </div>

        <form onSubmit={handleSubmit} style={{ padding: '12px 20px', borderTop: '1px solid #e0e0e0', display: 'flex', gap: 8, flexShrink: 0 }}>
          <input
            ref={inputRef}
            type="text"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            placeholder="Ask a follow-up question..."
            className="symbol-input"
            style={{ flex: 1 }}
            disabled={loading}
          />
          <button type="submit" className="btn btn-primary" disabled={loading || !input.trim()}>
            Send
          </button>
        </form>
      </div>
      <style>{`@keyframes spin { to { transform: rotate(360deg) } }`}</style>
    </div>
  )
}
