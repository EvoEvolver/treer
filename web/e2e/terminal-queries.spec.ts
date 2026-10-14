import { test, expect } from '@playwright/test'

const cases: [string, string, RegExp[]][] = [
  ['DA1', '\x1b[c', [/^\x1b\[\?1;2c$/]],
  ['DA2', '\x1b[>c', [/^\x1b\[>0;276;0c$/]],
  ['DSR status', '\x1b[5n', [/^\x1b\[0n$/]],
  ['CPR', '\x1b[6n', [/^\x1b\[\d+;\d+R$/]],
  ['private CPR', '\x1b[?6n', [/^\x1b\[\?\d+;\d+R$/]],
  ...[15, 25, 26, 53].map((code): [string, string, RegExp[]] => [`private DSR ${code} (5.5.0 no-op)`, `\x1b[?${code}n`, []]),
  ['DECRQM ANSI', '\x1b[4$p', [/^\x1b\[4;2\$y$/]],
  ['DECRQM private', '\x1b[?25$p', [/^\x1b\[\?25;1\$y$/]],
  ['DECRQM unknown mode', '\x1b[999$p', [/^\x1b\[999;0\$y$/]],
  ...['m', 'r', ' q', '"q', '"p', 'unsupported'].map((data): [string, string, RegExp[]] => [
    `DECRQSS ${data}`, `\x1bP$q${data}\x1b\\`, [data === 'unsupported' ? /^\x1bP0\$r\x1b\\$/ : /^\x1bP1\$r.*\x1b\\$/],
  ]),
  ['OSC palette', '\x1b]4;1;?\x07', [/^\x1b\]4;1;rgb:.*\x1b\\$/]],
  ...[10, 11, 12].map((id): [string, string, RegExp[]] => [
    `OSC special color ${id}`, `\x1b]${id};?\x1b\\`, [new RegExp(`^\\x1b\\]${id};rgb:.*\\x1b\\\\$`)],
  ]),
  ['OSC stacked special colors', '\x1b]10;?;?;?\x07', [/^\x1b\]10;rgb:/, /^\x1b\]11;rgb:/, /^\x1b\]12;rgb:/]],
  ['window pixels', '\x1b[14t', [/^\x1b\[4;\d+;\d+t$/]],
  ['cell pixels', '\x1b[16t', [/^\x1b\[6;\d+;\d+t$/]],
  ['window characters', '\x1b[18t', [/^\x1b\[8;\d+;\d+t$/]],
]

for (const [name, query, expected] of cases) {
  test(`xterm query inventory: ${name} is silent in replay and unchanged live`, async ({ page }) => {
    await page.route('**/terminal-query-test', (route) => route.fulfill({ contentType: 'text/html', body: '<!doctype html><html><body></body></html>' }))
    await page.goto('/terminal-query-test')
    const result = await page.evaluate(async (query) => {
      // Vite compiles this browser-only harness with the actual installed xterm.
      const harness = await import(/* @vite-ignore */ '/e2e/terminal-query-harness.ts')
      return harness.runQuery(query)
    }, query)
    expect(result.replay).toEqual([])
    expect(result.live).toHaveLength(expected.length)
    expected.forEach((pattern, index) => expect(result.live[index]).toMatch(pattern))
  })
}
