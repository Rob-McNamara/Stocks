import { describe, it, expect } from 'vitest'
import { parseCollapsedCards } from './collapsedCards'

describe('parseCollapsedCards', () => {
  it('reads the stored ids', () => {
    expect(parseCollapsedCards(JSON.stringify(['heatmap-ETFs', 'heatmap-overall']))).toEqual(
      new Set(['heatmap-ETFs', 'heatmap-overall']),
    )
  })

  it('treats an unset key as nothing collapsed', () => {
    expect(parseCollapsedCards(undefined)).toEqual(new Set())
    expect(parseCollapsedCards('')).toEqual(new Set())
  })

  // Every one of these would otherwise throw or produce a Set of junk, and a
  // card that cannot decide whether it is folded renders as a bare heading.
  it('falls back to nothing collapsed on anything it cannot read', () => {
    for (const raw of ['not json', '{"id":1}', '42', 'null', '[']) {
      expect(parseCollapsedCards(raw)).toEqual(new Set())
    }
  })

  it('drops non-string entries rather than storing them as ids', () => {
    expect(parseCollapsedCards(JSON.stringify(['heatmap-ETFs', 7, null, { a: 1 }]))).toEqual(
      new Set(['heatmap-ETFs']),
    )
  })
})
