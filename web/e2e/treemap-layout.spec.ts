import { test, expect } from '@playwright/test'
import { squarify } from '../src/lib/treemap-layout'

test('squarified layout fills bounds without overlap and preserves exact workload weights', () => {
  for (const count of [5, 40, 300]) for (const [width, height] of [[1100, 620], [350, 540], [340, 280]]) {
    const items = Array.from({ length: count }, (_, i) => ({ path: `/group-${i}`, count: i % 7 + 1 }))
    const rects = squarify(items, (item) => item.count, { x: 0, y: 0, width, height })
    expect(rects.map(({ item }) => item)).toEqual(items)
    const sum = items.reduce((s, item) => s + item.count, 0)
    let area = 0
    for (const { item, rect } of rects) {
      expect(rect.x).toBeGreaterThanOrEqual(0)
      expect(rect.y).toBeGreaterThanOrEqual(0)
      expect(rect.x + rect.width).toBeLessThanOrEqual(width + 0.000001)
      expect(rect.y + rect.height).toBeLessThanOrEqual(height + 0.000001)
      expect(rect.width * rect.height).toBeCloseTo(item.count / sum * width * height, 5)
      area += rect.width * rect.height
    }
    expect(area).toBeCloseTo(width * height, 5)
    let maxIntersection = 0
    for (let i = 0; i < rects.length; i++) for (let j = i + 1; j < rects.length; j++) {
      const a = rects[i].rect, b = rects[j].rect
      const intersection = Math.max(0, Math.min(a.x + a.width, b.x + b.width) - Math.max(a.x, b.x))
        * Math.max(0, Math.min(a.y + a.height, b.y + b.height) - Math.max(a.y, b.y))
      maxIntersection = Math.max(maxIntersection, intersection)
    }
    expect(maxIntersection).toBeLessThan(0.000001)
  }
})

test('zero-area and empty folders do not allocate invalid rectangles', () => {
  expect(squarify([0, 0], (n) => n, { x: 0, y: 0, width: 300, height: 200 })).toEqual([])
  expect(squarify([1], (n) => n, { x: 0, y: 0, width: 0, height: 200 })).toEqual([])
  expect(squarify([0, 3], (n) => n, { x: 10, y: 20, width: 300, height: 200 })).toEqual([{ item: 3, rect: { x: 10, y: 20, width: 300, height: 200 } }])
})
