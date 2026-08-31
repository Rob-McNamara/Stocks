import { describe, it, expect } from 'vitest'
import { squarify, squarifyGrouped, type TreemapInput, type TreemapTile } from './treemap'

const area = (t: TreemapTile) => t.width * t.height

function overlaps(a: TreemapTile, b: TreemapTile): boolean {
  const epsilon = 0.0001
  return (
    a.x + a.width > b.x + epsilon &&
    b.x + b.width > a.x + epsilon &&
    a.y + a.height > b.y + epsilon &&
    b.y + b.height > a.y + epsilon
  )
}

describe('squarify', () => {
  // The whole point of the layout: a position worth twice as much occupies
  // twice the area. If this drifts, the map is decorative rather than readable.
  it('gives each tile an area proportional to its value', () => {
    const items: TreemapInput[] = [
      { key: 'a', value: 60 },
      { key: 'b', value: 30 },
      { key: 'c', value: 10 },
    ]
    const tiles = squarify(items, 400, 200)
    const total = 400 * 200

    for (const tile of tiles) {
      const expected = (tile.value / 100) * total
      expect(area(tile)).toBeCloseTo(expected, 4)
    }
  })

  it('fills the rectangle exactly, without overlaps', () => {
    const items = [12, 9, 7, 5, 4, 3, 2, 1].map((value, i) => ({ key: `s${i}`, value }))
    const tiles = squarify(items, 600, 350)

    expect(tiles).toHaveLength(items.length)
    expect(tiles.reduce((sum, t) => sum + area(t), 0)).toBeCloseTo(600 * 350, 3)

    for (const tile of tiles) {
      expect(tile.x).toBeGreaterThanOrEqual(-0.0001)
      expect(tile.y).toBeGreaterThanOrEqual(-0.0001)
      expect(tile.x + tile.width).toBeLessThanOrEqual(600.0001)
      expect(tile.y + tile.height).toBeLessThanOrEqual(350.0001)
    }
    for (let i = 0; i < tiles.length; i++) {
      for (let j = i + 1; j < tiles.length; j++) {
        expect(overlaps(tiles[i], tiles[j])).toBe(false)
      }
    }
  })

  it('orders tiles largest first regardless of input order', () => {
    const tiles = squarify(
      [
        { key: 'small', value: 1 },
        { key: 'big', value: 100 },
        { key: 'mid', value: 10 },
      ],
      300,
      300,
    )
    expect(tiles.map((t) => t.key)).toEqual(['big', 'mid', 'small'])
  })

  /**
   * The reason for squarifying rather than slicing. A holdings map is always
   * lopsided — a few large positions and a long tail — and simple slicing turns
   * the tail into unreadable threads.
   */
  it('keeps tiles near square even when the values are lopsided', () => {
    const items = [100, 50, 25, 12, 6, 3, 2, 1].map((value, i) => ({ key: `s${i}`, value }))
    const tiles = squarify(items, 800, 500)
    const worst = Math.max(...tiles.map((t) => Math.max(t.width / t.height, t.height / t.width)))
    expect(worst).toBeLessThan(6)
  })

  // A holding with no price has a current value of zero. Laying it out at 0×0
  // would leave an invisible tile that still answers to a click.
  it('drops non-positive values instead of drawing them at zero size', () => {
    const tiles = squarify(
      [
        { key: 'real', value: 10 },
        { key: 'unpriced', value: 0 },
        { key: 'negative', value: -5 },
      ],
      200,
      100,
    )
    expect(tiles.map((t) => t.key)).toEqual(['real'])
    expect(area(tiles[0])).toBeCloseTo(200 * 100, 4)
  })

  it('returns nothing for an empty set or a zero-sized rectangle', () => {
    expect(squarify([], 100, 100)).toEqual([])
    expect(squarify([{ key: 'a', value: 1 }], 0, 100)).toEqual([])
    expect(squarify([{ key: 'a', value: 1 }], 100, 0)).toEqual([])
  })

  // Dimensions come out of area arithmetic, so they land within floating-point
  // distance of the rectangle rather than exactly on it.
  it('gives a single item the whole rectangle', () => {
    const [tile] = squarify([{ key: 'only', value: 42 }], 320, 180)
    expect(tile.key).toBe('only')
    expect(tile.x).toBe(0)
    expect(tile.y).toBe(0)
    expect(tile.width).toBeCloseTo(320, 6)
    expect(tile.height).toBeCloseTo(180, 6)
  })
})

describe('squarifyGrouped', () => {
  const groups = [
    { key: 'Materials', items: [{ key: 'a', value: 40 }, { key: 'b', value: 20 }] },
    { key: 'Financials', items: [{ key: 'c', value: 30 }] },
    { key: 'Utilities', items: [{ key: 'd', value: 10 }] },
  ]

  // A group's frame has to be the sum of its members, or the outer level says
  // something different from the tiles inside it.
  it('sizes each group by the combined value of its items', () => {
    const laid = squarifyGrouped(groups, 400, 250)
    const total = 400 * 250
    const frameArea = (key: string) => {
      const g = laid.find((x) => x.key === key)!
      return g.width * g.height
    }
    expect(frameArea('Materials') / total).toBeCloseTo(0.6, 3)
    expect(frameArea('Financials') / total).toBeCloseTo(0.3, 3)
    expect(frameArea('Utilities') / total).toBeCloseTo(0.1, 3)
  })

  it('places every item inside its own group frame', () => {
    const laid = squarifyGrouped(groups, 600, 400, 16)
    expect(laid.flatMap((g) => g.tiles)).toHaveLength(4)

    for (const group of laid) {
      for (const tile of group.tiles) {
        expect(tile.x).toBeGreaterThanOrEqual(group.x - 0.0001)
        expect(tile.y).toBeGreaterThanOrEqual(group.y - 0.0001)
        expect(tile.x + tile.width).toBeLessThanOrEqual(group.x + group.width + 0.0001)
        expect(tile.y + tile.height).toBeLessThanOrEqual(group.y + group.height + 0.0001)
      }
    }
  })

  it('reserves the header strip at the top of each group', () => {
    const header = 16
    for (const group of squarifyGrouped(groups, 600, 400, header)) {
      for (const tile of group.tiles) {
        expect(tile.y).toBeGreaterThanOrEqual(group.y + header - 0.0001)
      }
    }
  })

  /**
   * A group too short to give up a header strip keeps its whole rectangle. The
   * caller can drop the label; it cannot recover squeezed-out children.
   */
  it('skips the header rather than starving a short group of space', () => {
    const lopsided = [
      { key: 'huge', items: [{ key: 'a', value: 1000 }] },
      { key: 'sliver', items: [{ key: 'b', value: 1 }] },
    ]
    const laid = squarifyGrouped(lopsided, 600, 30, 16)
    const sliver = laid.find((g) => g.key === 'sliver')!
    expect(sliver.height).toBeLessThanOrEqual(16 * 2)
    expect(sliver.tiles[0].y).toBeCloseTo(sliver.y, 4)
    expect(sliver.tiles[0].height).toBeCloseTo(sliver.height, 4)
  })

  it('drops groups whose items are all valueless', () => {
    const laid = squarifyGrouped(
      [
        { key: 'real', items: [{ key: 'a', value: 5 }] },
        { key: 'empty', items: [{ key: 'b', value: 0 }] },
        { key: 'none', items: [] },
      ],
      200,
      200,
    )
    expect(laid.map((g) => g.key)).toEqual(['real'])
  })
})
