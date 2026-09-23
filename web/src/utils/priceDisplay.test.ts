import { describe, it, expect } from 'vitest'
import { formatPrice, priceParts } from './priceDisplay'

describe('priceParts', () => {
  it('brackets the other currency, whichever leads', () => {
    expect(priceParts(291.5, 190.92, 'USD', true)).toEqual({ main: 'USD 190.92', aside: '(A$291.50)' })
    expect(priceParts(291.5, 190.92, 'USD', false)).toEqual({ main: '$291.50', aside: '(USD 190.92)' })
  })

  it('leaves the code off when the card already names the currency', () => {
    expect(priceParts(291.5, 190.92, null, true)).toEqual({ main: '190.92', aside: '(A$291.50)' })
  })

  it('drops the brackets when there is only one figure', () => {
    expect(priceParts(41.25, null, 'AUD', false)).toEqual({ main: '$41.25', aside: null })
    expect(priceParts(null, 190.92, 'USD', true)).toEqual({ main: 'USD 190.92', aside: null })
    // No FX rate yet: the native price still leads on either screen.
    expect(priceParts(null, 190.92, 'USD', false)).toEqual({ main: 'USD 190.92', aside: null })
    expect(priceParts(null, null, 'USD', true)).toEqual({ main: '—', aside: null })
  })
})

describe('formatPrice', () => {
  // AEG.AX trades at 0.075 and moves a tenth of a cent: at two decimals its
  // price, both its averages and its daily change all collapse together.
  it('keeps a sub-cent price legible', () => {
    expect(formatPrice(0.075)).toBe('0.075')
    expect(formatPrice(0.0826899998939037)).toBe('0.0827')
    expect(formatPrice(0.06969000010212262)).toBe('0.0697')
    expect(formatPrice(0.001)).toBe('0.001')
    expect(formatPrice(0.0045)).toBe('0.0045')
  })

  it('leaves a dollar price at cents', () => {
    expect(formatPrice(123.456)).toBe('123.46')
    expect(formatPrice(1.5)).toBe('1.50')
    expect(formatPrice(0.5)).toBe('0.50')
    expect(formatPrice(0)).toBe('0.00')
  })

  // Padding 0.075 out to 0.0750 would claim a precision the quote never had.
  it('drops trailing zeros without going below cents', () => {
    expect(formatPrice(0.12)).toBe('0.12')
    expect(formatPrice(0.1)).toBe('0.10')
    expect(formatPrice(-0.0032)).toBe('-0.0032')
  })

  // A price that rounds to 0.00 is worse than a long one: it reads as free.
  it('bounds the string without rounding a price to nothing', () => {
    expect(formatPrice(0.000000123)).toBe('0.00000012')
    expect(formatPrice(0.0000005)).toBe('0.0000005')
  })
})
