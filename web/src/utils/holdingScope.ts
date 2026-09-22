/**
 * Which half of the portfolio something belongs to.
 *
 * Local and international holdings have a screen each, so anything that
 * navigates to a symbol — a Dashboard heat map tile, a worst-holdings row, a
 * watchlist entry on its way in — has to decide which screen to open.
 */
export type HoldingScope = 'local' | 'international'

/**
 * The screen a symbol belongs to.
 *
 * A symbol that is already held is decided by the server's `is_international`,
 * and nothing else: the API treats a symbol bought entirely in AUD as local
 * however Yahoo denominates it, so the flag cannot be re-derived from the
 * ticker or the quote, and second-guessing it here would split a position off
 * from the screen that lists it.
 *
 * A symbol that is not held yet has no such flag, so its quote currency stands
 * in — good enough to open the right form, and replaced by the real
 * classification the moment the purchase is saved.
 */
export function scopeForSymbol(
  symbol: string,
  isInternationalBySymbol: Record<string, boolean>,
  currency?: string | null,
): HoldingScope {
  const held = isInternationalBySymbol[symbol]
  const isInternational = held ?? (!!currency && currency.trim().toUpperCase() !== 'AUD')
  return isInternational ? 'international' : 'local'
}
