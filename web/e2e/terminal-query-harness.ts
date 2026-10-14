import { Terminal } from '@xterm/xterm'
import '@xterm/xterm/css/xterm.css'
import { suppressReplayQueries } from '../src/lib/terminal-replay'

export async function runQuery(query: string) {
  const host = document.createElement('div')
  host.style.cssText = 'width:600px;height:300px'
  document.body.append(host)
  const terminal = new Terminal({ windowOptions: { getWinSizePixels: true, getCellSizePixels: true, getWinSizeChars: true } })
  terminal.open(host)
  const replies: string[] = []
  terminal.onData((data) => replies.push(data))
  const handlers = suppressReplayQueries(terminal)
  let replay: string[] = []
  try {
    await new Promise<void>((resolve) => {
      const middle = Math.floor(query.length / 2)
      terminal.write(query.slice(0, middle))
      terminal.write(query.slice(middle), () => {
        replay = replies.slice()
        handlers.forEach((handler) => handler.dispose())
      })
      // Deliberately enqueue live output before replay has parsed.
      terminal.write(query, resolve)
    })
    return { replay, live: replies.slice(replay.length) }
  } finally { handlers.forEach((handler) => handler.dispose()); terminal.dispose(); host.remove() }
}
