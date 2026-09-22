import { describe, it, expect } from 'vitest'
import { scopeForSymbol } from './holdingScope'

describe('scopeForSymbol', () => {
  const held = { 'BHP.AX': false, EXPD: true }

  it('sends a held symbol to the screen its server flag names', () => {
    expect(scopeForSymbol('BHP.AX', held)).toBe('local')
    expect(scopeForSymbol('EXPD', held)).toBe('international')
  })

  // The API calls a symbol bought entirely in AUD local whatever currency
  // Yahoo reports, and that ruling has to survive the trip to the screen —
  // otherwise the position is charted on one screen and listed on the other.
  it('trusts the flag over the quote currency for a held symbol', () => {
    expect(scopeForSymbol('BHP.AX', held, 'USD')).toBe('local')
    expect(scopeForSymbol('EXPD', held, 'AUD')).toBe('international')
  })

  it('falls back to the quote currency for a symbol not held yet', () => {
    expect(scopeForSymbol('SOL.AX', {}, 'AUD')).toBe('local')
    expect(scopeForSymbol('MSFT', {}, 'USD')).toBe('international')
    expect(scopeForSymbol('MSFT', {}, 'usd')).toBe('international')
    expect(scopeForSymbol('SOL.AX', {}, ' AUD ')).toBe('local')
  })

  // Nothing known either way: the local screen is the safe landing, since it is
  // where most entries belong and the form lets the currency be set anyway.
  it('lands on the local screen when nothing is known', () => {
    expect(scopeForSymbol('NEW.AX', {})).toBe('local')
    expect(scopeForSymbol('NEW.AX', {}, null)).toBe('local')
    expect(scopeForSymbol('NEW.AX', {}, '')).toBe('local')
  })
})
