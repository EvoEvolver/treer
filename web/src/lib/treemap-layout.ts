export type MapRect = { x: number; y: number; width: number; height: number }

// Squarified strips, in caller-supplied path order. No weight sorting: renaming
// moves a block predictably; a status update never changes its position.
export function squarify<T>(items: T[], weight: (item: T) => number, bounds: MapRect): { item: T; rect: MapRect }[] {
  const total = items.reduce((sum, item) => sum + Math.max(0, weight(item)), 0)
  if (!total || bounds.width <= 0 || bounds.height <= 0) return []
  const pending = items.filter((item) => weight(item) > 0).map((item) => ({ item, area: weight(item) / total * bounds.width * bounds.height }))
  const result: { item: T; rect: MapRect }[] = []
  let box = { ...bounds }
  function worst(row: typeof pending, side: number) {
    const sum = row.reduce((s, entry) => s + entry.area, 0)
    const values = row.map((entry) => entry.area)
    return Math.max(side ** 2 * Math.max(...values) / sum ** 2, sum ** 2 / (side ** 2 * Math.min(...values)))
  }
  let index = 0
  while (index < pending.length) {
    const side = Math.min(box.width, box.height)
    const row = [pending[index++]]
    while (index < pending.length && worst([...row, pending[index]], side) <= worst(row, side)) row.push(pending[index++])
    const sum = row.reduce((s, entry) => s + entry.area, 0)
    const vertical = box.width >= box.height
    const thickness = Math.min(vertical ? box.width : box.height, sum / side)
    let offset = 0
    for (let i = 0; i < row.length; i++) {
      const entry = row[i]
      const length = i === row.length - 1 ? side - offset : entry.area / thickness
      result.push({ item: entry.item, rect: vertical
        ? { x: box.x, y: box.y + offset, width: thickness, height: length }
        : { x: box.x + offset, y: box.y, width: length, height: thickness } })
      offset += length
    }
    box = vertical ? { ...box, x: box.x + thickness, width: Math.max(0, box.width - thickness) }
      : { ...box, y: box.y + thickness, height: Math.max(0, box.height - thickness) }
  }
  return result
}
