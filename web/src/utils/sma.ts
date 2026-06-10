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
