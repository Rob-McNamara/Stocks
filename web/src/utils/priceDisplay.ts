/**
 * A price with as many decimals as it needs, and no more.
 *
 * Two decimals suit a dollar stock, but AEG.AX trades at 0.075 and moves a
 * tenth of a cent a day: rounded to cents its price, its 50-day average and
 * its 150-day average all read $0.07, and a day's move reads +0.00. Below a
 * dollar the figure keeps two significant decimals beyond its leading zeros,
 * and trailing zeros are dropped so nothing gains false precision — 0.075
 * stays 0.075 rather than becoming 0.0750. The eight-decimal ceiling is there
 * to bound the string, not to round a price away: at six, anything under about
 * 0.000005 would have read as a flat 0.00.
 *
 * Money totals are not prices and stay at two decimals: a portfolio worth
 * $1500.00 is not worth $1500.0025.
 */
export function formatPrice(value: number): string {
  const abs = Math.abs(value)
  if (!isFinite(value) || abs === 0 || abs >= 1) return value.toFixed(2)
  const decimals = Math.min(8, Math.max(2, Math.ceil(-Math.log10(abs)) + 2))
  return value.toFixed(decimals).replace(/(\.\d\d\d*?)0+$/, '$1')
}

/**
 * A price and its counterpart in the other currency, as the two are shown.
 *
 * On the International screen a holding is read in the currency it trades in,
 * so the native price leads and the AUD conversion follows in brackets; the
 * Local screen keeps AUD leading. The bracketed AUD carries an `A$` rather
 * than the bare `$` used elsewhere, because beside a foreign figure a lone `$`
 * says nothing.
 *
 * `currency` labels the native figure, and is left out where the surrounding
 * card already names it — the holding's own currency tag — so the code is not
 * repeated on every line.
 *
 * Either side may be missing — a symbol with no FX rate yet has no AUD figure,
 * and a close-only quote has no native one — so whichever exists leads and the
 * brackets are dropped.
 */
export function priceParts(
  aud: number | null | undefined,
  native: number | null | undefined,
  currency: string | null | undefined,
  nativeFirst: boolean,
): { main: string; aside: string | null } {
  const audText = aud != null ? `$${formatPrice(aud)}` : null
  const nativeText = native != null ? (currency ? `${currency} ${formatPrice(native)}` : formatPrice(native)) : null
  if (nativeFirst && nativeText) {
    return { main: nativeText, aside: aud != null ? `(A$${formatPrice(aud)})` : null }
  }
  if (!audText) return { main: nativeText ?? '—', aside: null }
  return { main: audText, aside: nativeText && !nativeFirst ? `(${nativeText})` : null }
}
