/**
 * Squarified treemap layout.
 *
 * Ported from Bruls, Huizing & van Wijk (2000). Slicing a rectangle strictly in
 * one direction is far simpler, but it produces slivers as soon as the values
 * are unevenly weighted — and a holdings map always is, with a handful of large
 * positions and a long tail. Squarifying keeps each tile near a square, which
 * is what makes areas comparable by eye.
 */

export interface TreemapInput {
  /** Identifies the tile to the caller; this module never interprets it. */
  key: string
  /** Relative area. Zero and negative values are dropped, not drawn at 0×0. */
  value: number
}

export interface TreemapTile {
  key: string
  value: number
  x: number
  y: number
  width: number
  height: number
}

export interface TreemapGroup {
  /** Identifies the group; also what the caller labels it with. */
  key: string
  items: TreemapInput[]
}

export interface TreemapGroupLayout {
  key: string
  /** Combined value of the group's items — its share of the whole. */
  value: number
  /** The group's outer rectangle, header strip included. */
  x: number
  y: number
  width: number
  height: number
  /** Children, laid out in the rectangle below the header. */
  tiles: TreemapTile[]
}

/** Worst aspect ratio in a row of tiles laid along `side`. Lower is squarer. */
function worstRatio(row: number[], rowSum: number, side: number): number {
  if (rowSum <= 0 || side <= 0) return Infinity
  const scale = (side * side) / (rowSum * rowSum)
  const max = Math.max(...row)
  const min = Math.min(...row)
  return Math.max((scale * max) / 1, 1 / (scale * min))
}

/**
 * Lay `items` out inside a rectangle, largest first.
 *
 * Tiles fill the rectangle exactly: the areas are proportional to `value`, so
 * the caller can read size as weight without a scale. Items with a non-positive
 * value are omitted — a position worth nothing has no area to occupy, and
 * drawing it at zero size would leave an invisible click target.
 */
export function squarify(
  items: TreemapInput[],
  width: number,
  height: number,
): TreemapTile[] {
  const usable = items.filter((item) => item.value > 0)
  if (usable.length === 0 || width <= 0 || height <= 0) return []

  const sorted = [...usable].sort((a, b) => b.value - a.value)
  const total = sorted.reduce((sum, item) => sum + item.value, 0)
  // Work in pixel area from here on, so a row's height falls straight out of
  // its area divided by the side it runs along.
  const scale = (width * height) / total
  const scaled = sorted.map((item) => ({ ...item, area: item.value * scale }))

  const tiles: TreemapTile[] = []
  let x = 0
  let y = 0
  let freeWidth = width
  let freeHeight = height
  let index = 0

  while (index < scaled.length) {
    const side = Math.min(freeWidth, freeHeight)
    const row: number[] = []
    let rowSum = 0

    // Grow the row while doing so makes its worst tile squarer.
    while (index + row.length < scaled.length) {
      const next = scaled[index + row.length].area
      const currentWorst = row.length === 0 ? Infinity : worstRatio(row, rowSum, side)
      const nextWorst = worstRatio([...row, next], rowSum + next, side)
      if (nextWorst > currentWorst) break
      row.push(next)
      rowSum += next
    }

    // The row runs along the shorter side and is `thickness` deep, so the
    // remaining free rectangle stays as square as possible for the next row.
    const alongWidth = freeWidth >= freeHeight
    const thickness = rowSum / side
    let offset = 0
    row.forEach((area, i) => {
      const item = scaled[index + i]
      const length = area / thickness
      tiles.push(
        alongWidth
          ? { key: item.key, value: item.value, x, y: y + offset, width: thickness, height: length }
          : { key: item.key, value: item.value, x: x + offset, y, width: length, height: thickness },
      )
      offset += length
    })

    index += row.length
    if (alongWidth) {
      x += thickness
      freeWidth -= thickness
    } else {
      y += thickness
      freeHeight -= thickness
    }
    // Floating-point drift can leave a sliver of free space with items still to
    // place; without this the loop would spin placing zero-area tiles.
    if (freeWidth <= 0.0001 || freeHeight <= 0.0001) break
  }

  return tiles
}

/**
 * Two-level treemap: groups laid out against each other, then each group's
 * items laid out inside it.
 *
 * `headerHeight` reserves a strip at the top of every group for its label. A
 * group too short to give up that strip keeps its full rectangle instead — the
 * children are worth more than a label the caller can drop.
 */
export function squarifyGrouped(
  groups: TreemapGroup[],
  width: number,
  height: number,
  headerHeight = 0,
): TreemapGroupLayout[] {
  const totals = groups
    .map((group) => ({
      group,
      value: group.items.reduce((sum, item) => sum + Math.max(item.value, 0), 0),
    }))
    .filter((entry) => entry.value > 0)

  const byKey = new Map(totals.map((entry) => [entry.group.key, entry]))

  return squarify(
    totals.map((entry) => ({ key: entry.group.key, value: entry.value })),
    width,
    height,
  ).map((frame) => {
    const entry = byKey.get(frame.key)!
    const header = frame.height > headerHeight * 2 ? headerHeight : 0
    const tiles = squarify(entry.group.items, frame.width, frame.height - header).map((tile) => ({
      ...tile,
      x: tile.x + frame.x,
      y: tile.y + frame.y + header,
    }))
    return {
      key: frame.key,
      value: entry.value,
      x: frame.x,
      y: frame.y,
      width: frame.width,
      height: frame.height,
      tiles,
    }
  })
}
