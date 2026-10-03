// Kept out of PriceChart.tsx so that file exports only components: React's
// fast refresh cannot hot-swap a module that also exports plain values.

/**
 * Moving-average overlays, ordered by period so the buttons read left to right.
 * Identified by id rather than period because the period alone no longer says
 * which kind of average it is — 40 is exponential, the rest are simple.
 */
export interface OverlayDef {
  id: string
  label: string
  period: number
  kind: 'sma' | 'ema'
  color: string
  /** EMA gets its own dash so the two kinds are distinguishable at a glance. */
  dash: string
}

/**
 * Overlay colours, validated rather than chosen by eye.
 *
 * Constrained by what the chart already paints: the price line (#2f5ce4), the
 * candles (#4caf50 up, #f44336 down) and the purchase markers own blue, green,
 * red and bright orange, so no overlay may use them. The previous palette
 * matched three of those *exactly* — SMA 150 was the up-candle green and SMA
 * 200 the down-candle red, so in candle mode those lines vanished into the
 * bars — and its brown fell below the chroma floor, reading as grey.
 *
 * Checked with the data-viz validator on a white surface: the full seven pass
 * the lightness band, chroma floor, adjacent-pair CVD separation and contrast;
 * the three in heaviest use (EMA 40, SMA 50, SMA 150) also pass as their own
 * set, since they are typically shown together.
 *
 * EMA 200's green is a compromise, and worth stating plainly. With thirteen
 * chromatic colours already on the chart the space is full: a sweep of 576
 * candidates found nothing separating cleanly from all of them, and every
 * best-scoring option was a blue that collided with the price line in *normal*
 * vision — the same way SMA 150 and SMA 200 once vanished into the candles.
 * This green protects against the always-painted marks instead (ΔE 21 from the
 * up-candle, 26 from SMA 200, which it tracks closely). What it costs is
 * separation from SMA 150 under protanopia, where the two read alike; the
 * differing dash and the legend label are what distinguish them there.
 *
 * A seventh line was the last one this palette could take. Another needs a
 * slot freed first — SMA 100 is the least-used candidate.
 */
export const OVERLAYS: readonly OverlayDef[] = [
  { id: 'sma20',  label: 'SMA 20',  period: 20,  kind: 'sma', color: '#827717', dash: '8 6' },
  { id: 'ema40',  label: 'EMA 40',  period: 40,  kind: 'ema', color: '#0097a7', dash: '3 4' },
  { id: 'sma50',  label: 'SMA 50',  period: 50,  kind: 'sma', color: '#7b1fa2', dash: '8 6' },
  { id: 'sma100', label: 'SMA 100', period: 100, kind: 'sma', color: '#c2185b', dash: '8 6' },
  { id: 'sma150', label: 'SMA 150', period: 150, kind: 'sma', color: '#a15c00', dash: '8 6' },
  { id: 'sma200', label: 'SMA 200', period: 200, kind: 'sma', color: '#3949ab', dash: '8 6' },
  { id: 'ema200', label: 'EMA 200', period: 200, kind: 'ema', color: '#33691e', dash: '3 4' },
]
