export interface Bar {
  date: string
  open: number | null
  high: number | null
  low: number | null
  close: number | null
  volume: number | null
}

/**
 * Monday of the week containing `date` (YYYY-MM-DD), as YYYY-MM-DD.
 * Computed in UTC so the result never shifts with the viewer's timezone.
 */
function weekStart(date: string): string {
  const [y, m, d] = date.split('-').map(Number)
  const t = Date.UTC(y, m - 1, d)
  const day = new Date(t).getUTCDay() // 0 = Sunday
  const sinceMonday = (day + 6) % 7
  return new Date(t - sinceMonday * 86400000).toISOString().slice(0, 10)
}

/**
 * Collapse daily bars into weekly ones: first open, highest high, lowest low,
 * last close, summed volume.
 *
 * Each weekly bar is dated by its *last* trading day rather than the Monday, so
 * the most recent bar carries today's date — the chart header and x-axis then
 * read the same in both intervals, and a partial current week is labelled by
 * how far it has got.
 *
 * Input must be in ascending date order; output preserves that. Null fields are
 * skipped rather than treated as zero, so bars predating OHLC ingest aggregate
 * to a null open/high/low and the chart falls back to a line, exactly as the
 * daily view does.
 */
export function toWeeklyBars(bars: Bar[]): Bar[] {
  const out: Bar[] = []
  let key: string | null = null

  for (const bar of bars) {
    const week = weekStart(bar.date)
    if (week !== key) {
      key = week
      out.push({ ...bar })
      continue
    }

    const current = out[out.length - 1]
    current.date = bar.date
    // Open is the week's first, so it is only taken when still unset
    if (current.open === null) current.open = bar.open
    if (bar.high !== null) current.high = current.high === null ? bar.high : Math.max(current.high, bar.high)
    if (bar.low !== null) current.low = current.low === null ? bar.low : Math.min(current.low, bar.low)
    if (bar.close !== null) current.close = bar.close
    if (bar.volume !== null) current.volume = (current.volume ?? 0) + bar.volume
  }

  return out
}
