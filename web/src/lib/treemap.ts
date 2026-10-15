export interface Rectangle { x: number; y: number; width: number; height: number }

// Squarified, equal-leaf-weight layout. Input order remains stable for keyboard
// navigation; the algorithm only chooses how many adjacent cells share a strip.
export function treemap(weights: number[], bounds: Rectangle): Rectangle[] {
  if (!weights.length || bounds.width <= 0 || bounds.height <= 0) return []
  const total = weights.reduce((sum, value) => sum + Math.max(1, value), 0)
  const areas = weights.map((value) => Math.max(1, value) / total * bounds.width * bounds.height)
  const cells: Rectangle[] = []
  let remaining = { ...bounds }
  function worst(row: number[], side: number) {
    const sum = row.reduce((a, b) => a + b, 0)
    return Math.max(side * side * Math.max(...row) / (sum * sum), sum * sum / (side * side * Math.min(...row)))
  }
  let index = 0
  while (index < areas.length) {
    const row = [areas[index++]]
    const side = Math.min(remaining.width, remaining.height)
    while (index < areas.length && worst([...row, areas[index]], side) <= worst(row, side)) row.push(areas[index++])
    const vertical = remaining.width >= remaining.height
    const strip = row.reduce((a, b) => a + b, 0) / side
    let offset = 0
    for (const area of row) {
      const length = area / strip
      cells.push(vertical
        ? { x: remaining.x, y: remaining.y + offset, width: strip, height: length }
        : { x: remaining.x + offset, y: remaining.y, width: length, height: strip })
      offset += length
    }
    remaining = vertical
      ? { ...remaining, x: remaining.x + strip, width: Math.max(0, remaining.width - strip) }
      : { ...remaining, y: remaining.y + strip, height: Math.max(0, remaining.height - strip) }
  }
  return cells
}
