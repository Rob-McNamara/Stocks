interface PricePoint {
  close: number | null
}

export function calculateSMA(data: PricePoint[], period: number): Array<number | null> {
  const sma: Array<number | null> = Array(data.length).fill(null)

  for (let i = 0; i < data.length; i += 1) {
    if (i + 1 < period) {
      continue
    }

    let sum = 0
    let valid = true
    for (let j = i + 1 - period; j <= i; j += 1) {
      const value = data[j].close
      if (value === null) {
        valid = false
        break
      }
      sum += value
    }

    if (valid) {
      sma[i] = sum / period
    }
  }

  return sma
}

export function getLatestSMA(smaArray: Array<number | null>): number | null {
  // Find the last non-null SMA value
  for (let i = smaArray.length - 1; i >= 0; i -= 1) {
    if (smaArray[i] !== null) {
      return smaArray[i]
    }
  }
  return null
}

export function crossoverStats(
  history: { close: number | null; volume: number | null }[],
  smaArray: Array<number | null>,
  todayVolume?: number | null,
): { days: number; volumePct: number | null } {
  for (let i = smaArray.length - 1; i >= 0; i--) {
    const sma = smaArray[i]
    const close = history[i].close
    if (sma === null || close === null) continue
    if (close < sma) {
      const crossoverIdx = i + 1
      const days = smaArray.length - 1 - i
      // crossoverIdx may equal history.length when the crossover is today (live price
      // crossed above but last stored close was still below) — use todayVolume as fallback
      const crossoverVol = crossoverIdx < history.length
        ? history[crossoverIdx].volume
        : (todayVolume ?? null)
      const prev20 = history.slice(Math.max(0, crossoverIdx - 20), crossoverIdx).filter((p) => p.volume !== null && p.volume! > 0)
      let volumePct: number | null = null
      if (crossoverVol && crossoverVol > 0 && prev20.length > 0) {
        const avgVol = prev20.reduce((s, p) => s + p.volume!, 0) / prev20.length
        if (avgVol > 0) volumePct = ((crossoverVol - avgVol) / avgVol) * 100
      }
      return { days, volumePct }
    }
  }
  return { days: smaArray.length - 1, volumePct: null }
}
