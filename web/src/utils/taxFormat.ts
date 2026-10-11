import type { TaxColumnKind } from '../services/api'

export const taxMoney = new Intl.NumberFormat('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 2 })

/** A tax report cell as shown: the server sends raw numbers and their kind. */
export function formatTaxCell(value: string | number | null, kind: TaxColumnKind): string {
  if (value === null || value === undefined || kind === 'entry') return ''
  if (typeof value === 'string') return value
  switch (kind) {
    case 'money':
      return taxMoney.format(value)
    case 'rate':
      return value.toFixed(4)
    case 'price':
      return value.toLocaleString('en-AU', { minimumFractionDigits: 2, maximumFractionDigits: 4 })
    case 'integer':
      return Math.round(value).toLocaleString('en-AU')
    default:
      return value.toLocaleString('en-AU', { maximumFractionDigits: 4 })
  }
}

/** Numbers line up on the right; text, dates and write-in boxes don't. */
export function isNumericTaxColumn(kind: TaxColumnKind): boolean {
  return kind !== 'text' && kind !== 'date' && kind !== 'entry'
}
