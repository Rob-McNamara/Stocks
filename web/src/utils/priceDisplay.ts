/**
 * A price and its counterpart in the other currency, as the two are shown.
 *
 * On the International screen a holding is read in the currency it trades in,
 * so the native price leads and the AUD conversion follows in brackets; the
 * Local screen keeps AUD leading. The bracketed AUD carries an `A$` rather
 * than the bare `$` used elsewhere, because beside a US$ figure a lone `$`
 * says nothing.
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
  const audText = aud != null ? `$${aud.toFixed(2)}` : null
  const nativeText = native != null && currency ? `${currency} ${native.toFixed(2)}` : null
  if (nativeFirst && nativeText) {
    return { main: nativeText, aside: aud != null ? `(A$${aud.toFixed(2)})` : null }
  }
  if (!audText) return { main: nativeText ?? '—', aside: null }
  return { main: audText, aside: nativeText && !nativeFirst ? `(${nativeText})` : null }
}
