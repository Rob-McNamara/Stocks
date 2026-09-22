import { describe, it, expect } from 'vitest'
import { priceParts } from './priceDisplay'

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
