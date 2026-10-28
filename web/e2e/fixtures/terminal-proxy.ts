import type { Page, WebSocketRoute } from '@playwright/test'
import xtermHeadless from '@xterm/headless'

// A byte-faithful model of the Console terminal path behind the browser:
// Host output ring (revisions, 512 KiB bound, cursor/gap replay), Proxy
// attach (ready + 32 KiB replay chunks), the bounded per-browser queue that
// detaches a session when full, binary+cursor pairs and the ack flow window.
// Constants mirror crates/treer-agent-runtime and crates/treer-proxy.
const HOST_OUTPUT_LIMIT = 512 * 1024
const REPLAY_CHUNK = 32 * 1024
const FLOW_WINDOW = 256 * 1024
const PROXY_QUEUE_CAPACITY = 1024
const { Terminal: HeadlessTerminal } = xtermHeadless

export function rng(seed: number) {
  return () => {
    seed = (seed + 0x6d2b79f5) | 0
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed)
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

/** Realistic agent CLI output: logs, OSC titles, progress bars, ink-style and
 * full-screen redraws, CJK/emoji and long wrapped lines. */
export function workload(seed: number, rounds: number): Buffer {
  const random = rng(seed)
  const pick = <T,>(values: T[]) => values[Math.floor(random() * values.length)]
  const out: string[] = []
  const words = ['compiling', '编译', 'テスト', 'running', '✅ done', '🚀 deploy', '⚠️ warn', 'crate', '模块加载', '한국어']
  for (let round = 0; round < rounds; round++) {
    for (let i = 0; i < 30; i++) {
      const text = Array.from({ length: 2 + Math.floor(random() * 10) }, () => pick(words)).join(' ')
      out.push(`\x1b[2m${String(round).padStart(3, '0')}:${String(i).padStart(2, '0')}\x1b[0m \x1b[3${1 + (i % 7)}m${pick(['INFO', 'WARN', 'DEBUG'])}\x1b[0m ${text} \x1b[38;5;${16 + Math.floor(random() * 200)}m${'='.repeat(Math.floor(random() * 150))}\x1b[0m\r\n`)
      if (i % 5 === 0) out.push(`\x1b]0;✳ Working ${round}.${i} — 中文标题\x07`)
    }
    for (let p = 0; p <= 100; p += 4) {
      out.push(`\r\x1b[K\x1b[36m下载 [${'█'.repeat(p / 5)}${'░'.repeat(20 - p / 5)}] ${p}%\x1b[0m \x1b[38;2;${p};120;200m${pick(words)}\x1b[0m`)
    }
    out.push('\r\n')
    // ink (Claude Code) style: redraw the live region by erasing upwards.
    let height = 0
    for (let frame = 0; frame < 12; frame++) {
      out.push('\x1b[2K\x1b[1A'.repeat(height) + '\x1b[2K\r')
      const lines = 2 + Math.floor(random() * 4)
      out.push(Array.from({ length: lines }, (_, line) => `\x1b[1m${pick(['⠋', '⠙', '⠹', '⠸'])}\x1b[22m ${pick(words)} ${frame}/${line} ${'·'.repeat(Math.floor(random() * 40))}`).join('\r\n'))
      height = lines - 1
    }
    out.push('\r\n')
    // Full-screen TUI frames in the alternate screen with scroll regions.
    out.push('\x1b[?1049h\x1b[?25l')
    for (let frame = 0; frame < 4; frame++) {
      out.push('\x1b[H\x1b[2J\x1b[2;18r')
      for (let row = 1; row <= 20; row++) out.push(`\x1b[${row};1H\x1b[4${row % 8}m\x1b[30m ${row} ${pick(words)} ${frame} \x1b[0m│${'─'.repeat(Math.floor(random() * 30))}\x1b[K`)
      out.push('\x1b[r')
    }
    out.push('\x1b[?25h\x1b[?1049l')
    out.push(`${'x'.repeat(200 + Math.floor(random() * 300))}|end of long line ${round}\r\n`)
  }
  return Buffer.from(out.join(''), 'utf8')
}

/** Split at random byte offsets: mid UTF-8 and mid escape sequence on purpose. */
export function split(bytes: Buffer, seed: number, maxChunk = 2048): Buffer[] {
  const random = rng(seed)
  const chunks: Buffer[] = []
  for (let offset = 0; offset < bytes.length;) {
    const size = random() < 0.6 ? 1 + Math.floor(random() * 48) : 1 + Math.floor(random() * maxChunk)
    chunks.push(bytes.subarray(offset, offset + size))
    offset += size
  }
  return chunks
}

/** The screen and scrollback an xterm with the same geometry shows for `bytes`. */
export async function expectedBuffer(bytes: Buffer, cols: number, rows: number) {
  const terminal = new HeadlessTerminal({ cols, rows, scrollback: 10_000, allowProposedApi: true })
  await new Promise<void>((resolve) => terminal.write(bytes, resolve))
  const buffer = terminal.buffer.active
  const lines: string[] = []
  for (let y = 0; y < buffer.length; y++) lines.push(buffer.getLine(y)!.translateToString(true).trimEnd())
  terminal.dispose()
  return { lines, screen: lines.slice(-rows) }
}

interface Session {
  socket: WebSocketRoute
  queue: Array<Buffer | string>
  inFlight: number
  lastRevision: number
  detached: boolean
  closed: boolean
}

export class TerminalBackend {
  readonly epoch = 'stream_e2e'
  private chunks: Array<{ revision: number; data: Buffer }> = []
  private bytes = 0
  private nextRevision = 1
  private sessions = new Set<Session>()
  connections: URL[] = []
  detaches = 0
  windowStalls = 0
  /** Most binary frames the browser held unacknowledged at once. */
  maxUnackedFrames = 0
  private unackedFrames = 0
  browserCloses: number[] = []
  inputs: string[] = []
  resizes: Array<{ cols: number; rows: number }> = []
  /** Every byte the Host produced, in order. */
  produced: Buffer[] = []

  /** Treat acknowledgements as arriving this late after each attach, as from
   * a browser still busy parsing the replay (a slow phone). */
  attachStallMs = 0

  constructor(readonly queueCapacity = PROXY_QUEUE_CAPACITY) {}

  get lastRevision() { return this.nextRevision - 1 }
  get stats() {
    return { maxUnackedFrames: this.maxUnackedFrames, windowStalls: this.windowStalls, detaches: this.detaches, browserCloses: this.browserCloses, connections: this.connections.length, lastRevision: this.lastRevision, sessions: [...this.sessions].map(({ queue, inFlight, lastRevision, detached }) => ({ queue: queue.length, inFlight, lastRevision, detached })) }
  }
  get idle() {
    const sessions = [...this.sessions]
    return sessions.length === 1 && sessions.every((session) => !session.detached && session.queue.length === 0 && session.inFlight === 0 && session.lastRevision === this.lastRevision)
  }

  async route(page: Page) {
    await page.routeWebSocket(/\/terminal(?:\?.*)?$/, (socket) => this.attach(socket))
  }

  push(data: Buffer) {
    const revision = this.nextRevision++
    this.produced.push(data)
    this.chunks.push({ revision, data })
    this.bytes += data.length
    while (this.bytes > HOST_OUTPUT_LIMIT) this.bytes -= this.chunks.shift()!.data.length
    for (const session of this.sessions) {
      if (revision <= session.lastRevision) continue
      // handle_cluster_session_delivery: binary then cursor, try_send each.
      if (!this.enqueue(session, data)) continue
      if (!this.enqueue(session, JSON.stringify({ type: 'cursor', stream_epoch: this.epoch, revision }))) continue
      session.lastRevision = revision
      this.pump(session)
    }
  }

  private enqueue(session: Session, frame: Buffer | string) {
    if (session.detached) return false
    if (session.queue.length >= this.queueCapacity) {
      session.detached = true
      this.detaches += 1
      this.sessions.delete(session)
      this.pump(session)
      return false
    }
    session.queue.push(frame)
    return true
  }

  private pump(session: Session) {
    while (!session.closed && session.queue.length > 0 && session.inFlight < FLOW_WINDOW) {
      const frame = session.queue.shift()!
      if (typeof frame === 'string') session.socket.send(frame)
      else {
        session.inFlight += frame.length
        this.maxUnackedFrames = Math.max(this.maxUnackedFrames, ++this.unackedFrames)
        session.socket.send(frame)
      }
    }
    // The Proxy drops the browser socket once a detached queue is drained.
    if (session.detached && !session.closed && session.queue.length === 0) {
      session.closed = true
      session.socket.close()
    }
  }

  private attach(socket: WebSocketRoute) {
    const url = new URL(socket.url())
    this.connections.push(url)
    const session: Session = { socket, queue: [], inFlight: 0, lastRevision: 0, detached: false, closed: false }
    const stalledUntil = Date.now() + this.attachStallMs
    socket.onMessage((message) => {
      if (typeof message !== 'string') return void this.inputs.push(Buffer.from(message).toString())
      const parsed = JSON.parse(message)
      if (parsed.type === 'ack') {
        if (parsed.bytes > session.inFlight) throw new Error(`ack ${parsed.bytes} exceeds in-flight ${session.inFlight}`)
        const apply = () => {
          if (session.inFlight >= FLOW_WINDOW) this.windowStalls += 1
          this.unackedFrames = 0
          session.inFlight -= parsed.bytes
          this.pump(session)
        }
        const wait = stalledUntil - Date.now()
        if (wait > 0) setTimeout(apply, wait)
        else apply()
      } else if (parsed.type === 'resize') this.resizes.push({ cols: parsed.cols, rows: parsed.rows })
    })
    socket.onClose((code, reason) => {
      // A custom handler disables Playwright's close forwarding; finish the
      // handshake like the Proxy does.
      if (!session.closed) void socket.close({ code, reason })
      session.closed = true
      if (!session.detached) this.browserCloses.push(code ?? 0)
      this.sessions.delete(session)
    })
    // HostRuntime replay + Proxy terminal_ready.
    const epoch = url.searchParams.get('stream_epoch')
    const since = Number(url.searchParams.get('since_revision') ?? 0)
    const first = this.chunks[0]?.revision ?? this.nextRevision
    const matching = epoch === this.epoch ? since : 0
    const gap = epoch != null && (epoch !== this.epoch || matching + 1 < first)
    const replay = Buffer.concat(this.chunks.filter((chunk) => chunk.revision > matching).map((chunk) => chunk.data))
    const replayChunks = Math.ceil(replay.length / REPLAY_CHUNK)
    this.sessions.add(session)
    this.enqueue(session, JSON.stringify({ type: 'ready', session_id: `term_${this.connections.length}`, stream_epoch: this.epoch, revision: this.lastRevision, gap, replay_chunks: replayChunks }))
    for (let offset = 0; offset < replay.length; offset += REPLAY_CHUNK) this.enqueue(session, replay.subarray(offset, offset + REPLAY_CHUNK))
    session.lastRevision = this.lastRevision
    this.pump(session)
  }

  /** Mid-session resync (agent-server lag path): a gap Ready and a full replay. */
  resync() {
    for (const session of this.sessions) {
      const replay = Buffer.concat(this.chunks.map((chunk) => chunk.data))
      this.enqueue(session, JSON.stringify({ type: 'ready', session_id: 'term_resync', stream_epoch: this.epoch, revision: this.lastRevision, gap: true, replay_chunks: Math.ceil(replay.length / REPLAY_CHUNK) }))
      for (let offset = 0; offset < replay.length; offset += REPLAY_CHUNK) this.enqueue(session, replay.subarray(offset, offset + REPLAY_CHUNK))
      session.lastRevision = this.lastRevision
      this.pump(session)
    }
  }

  /** Feed chunks at a steady rate, as a busy PTY would. */
  async stream(chunks: Buffer[], perTick = 24, tickMs = 2) {
    for (let index = 0; index < chunks.length; index += perTick) {
      for (const chunk of chunks.slice(index, index + perTick)) this.push(chunk)
      await new Promise((resolve) => setTimeout(resolve, tickMs))
    }
  }
}

/** Read `total` buffer lines the Console xterm shows by paging its viewport. */
export async function browserBuffer(page: Page, scope: string, total: number) {
  return page.evaluate(async ({ scope, total }) => {
    const root = document.querySelector(scope)!
    const viewport = root.querySelector<HTMLElement>('.xterm-viewport')!
    const rowsElement = root.querySelector<HTMLElement>('.xterm-rows')!
    const rows = rowsElement.children.length
    const rowHeight = (rowsElement.children[0] as HTMLElement).getBoundingClientRect().height
    const frame = () => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)))
    const read = () => [...rowsElement.children].map((row) => (row.textContent ?? '').replace(/ /g, ' ').trimEnd())
    const lines: string[] = new Array(total).fill('')
    const bottom = Math.max(0, total - rows)
    for (let top = 0; ; top = Math.min(bottom, top + rows)) {
      viewport.scrollTop = top === bottom ? viewport.scrollHeight : top * rowHeight
      viewport.dispatchEvent(new Event('scroll'))
      await frame()
      read().forEach((line, index) => { lines[top + index] = line })
      if (top === bottom) break
    }
    return { lines, screen: read(), rows }
  }, { scope, total })
}
