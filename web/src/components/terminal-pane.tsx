import { forwardRef, useEffect, useImperativeHandle, useRef } from "react"
import { Terminal } from "@xterm/xterm"
import { FitAddon } from "@xterm/addon-fit"
import "@xterm/xterm/css/xterm.css"
import { websocketUrl } from "@/lib/api"

type TerminalStatus = "not attached" | "connecting" | "live" | "reconnecting" | "closed" | "error"

interface TerminalPaneProps {
  workspaceId: string | null
  agentId: string | null
  active: boolean
  onStatusChange: (status: TerminalStatus) => void
  transformInput?: (data: string) => string
}

export interface TerminalPaneHandle {
  focus: () => void
  send: (data: string) => void
}

interface StreamCursor {
  stream_epoch: string
  revision: number
}

interface TerminalControlMessage {
  type: string
  session_id?: string
  stream_epoch?: string
  revision?: number
  gap?: boolean
  replay_chunks?: number
  reason?: string
  error?: { message?: string }
}

const MAX_PENDING_INPUT_BYTES = 32_768
const TERMINAL_FLOW_WINDOW_BYTES = 256 * 1024
const MAX_PENDING_OUTPUT_BYTES = TERMINAL_FLOW_WINDOW_BYTES * 2
const MAX_PENDING_OUTPUT_WRITES = 1024

interface PendingTerminalWrite {
  bytes: number
  cursor: StreamCursor | null
  waitsForCursor: boolean
  refreshAfterWrite: boolean
  parsed: boolean
}

function shouldResetTerminal(cursor: StreamCursor | null, message: TerminalControlMessage) {
  if (message.gap || !message.stream_epoch) return true
  return cursor?.stream_epoch !== message.stream_epoch
}

function terminalUrl(workspaceId: string, agentId: string, cols: number, rows: number, cursor: StreamCursor | null) {
  const params = new URLSearchParams({ cols: String(cols), rows: String(rows), flow_control: "true" })
  if (cursor) {
    params.set("stream_epoch", cursor.stream_epoch)
    params.set("since_revision", String(cursor.revision))
  }
  return websocketUrl(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents/${encodeURIComponent(agentId)}/terminal?${params}`)
}

export const TerminalPane = forwardRef<TerminalPaneHandle, TerminalPaneProps>(function TerminalPane({ workspaceId, agentId, active, onStatusChange, transformInput }, ref) {
  const hostRef = useRef<HTMLDivElement>(null)
  const focusRef = useRef<() => void>(() => undefined)
  const sendRef = useRef<(data: string) => void>(() => undefined)
  const transformInputRef = useRef(transformInput)
  transformInputRef.current = transformInput

  useImperativeHandle(ref, () => ({
    focus: () => focusRef.current(),
    send: (data) => sendRef.current(data),
  }), [])

  useEffect(() => {
    const host = hostRef.current
    if (!host || !workspaceId || !agentId) {
      onStatusChange("not attached")
      return
    }

    let disposed = false
    let socket: WebSocket | null = null
    let reconnectTimer: number | undefined
    let resizeTimer: number | undefined
    let refreshFrame = 0
    let reconnectAttempt = 0
    let reconnectAllowed = true
    let lastHostWidth = 0
    let lastHostHeight = 0
    let cursor: StreamCursor | null = null
    let replayChunksRemaining = 0
    let replayCursor: StreamCursor | null = null
    let pendingOutputBytes = 0
    let reconnectAfterWrites = false
    let resetOnReady = false
    let flowControlActive = false
    const pendingInput: string[] = []
    const commitQueue: PendingTerminalWrite[] = []
    const pendingAcks = new Map<WebSocket, number>()
    let pendingInputBytes = 0
    const terminal = new Terminal({
      cursorBlink: false,
      cursorStyle: "block",
      fontFamily: "SFMono-Regular, Menlo, Monaco, Consolas, monospace",
      fontSize: 13,
      lineHeight: 1.16,
      scrollback: 10_000,
      allowTransparency: false,
      theme: {
        background: "#0f1215",
        foreground: "#d8dcdf",
        cursor: "#f2f3f3",
        selectionBackground: "#3b4750",
        black: "#171b1f",
        red: "#ff7b72",
        green: "#75c69b",
        yellow: "#d9b65c",
        blue: "#72a7e8",
        magenta: "#b79ae8",
        cyan: "#65bec7",
        white: "#d8dcdf",
        brightBlack: "#687178",
      },
    })
    const fit = new FitAddon()
    terminal.loadAddon(fit)
    terminal.open(host)
    focusRef.current = () => terminal.focus()

    // Windows browsers (Chrome/Edge) auto-copy selected text to the clipboard;
    // xterm would forward that as Ctrl+C → SIGINT. If a selection exists when
    // Ctrl+C is pressed, swallow the key so the agent's foreground job is not
    // interrupted. (Selected text is already on the OS clipboard.)
    terminal.attachCustomKeyEventHandler((event) => {
      if (event.type !== "keydown") return true
      if (!event.ctrlKey || event.altKey || event.metaKey) return true
      if (event.key.toLowerCase() !== "c") return true
      const selection = window.getSelection()?.toString()
      if (!selection) return true
      return false
    })

    const flushPendingInput = () => {
      if (socket?.readyState !== WebSocket.OPEN) return
      for (const data of pendingInput) socket.send(new TextEncoder().encode(data))
      pendingInput.length = 0
      pendingInputBytes = 0
    }

    const send = (data: string) => {
      if (socket?.readyState === WebSocket.OPEN) {
        socket.send(new TextEncoder().encode(data))
        return
      }
      if (pendingInputBytes + data.length > MAX_PENDING_INPUT_BYTES) return
      pendingInput.push(data)
      pendingInputBytes += data.length
    }
    sendRef.current = send

    const sendResize = () => {
      if (socket?.readyState === WebSocket.OPEN) {
        socket.send(JSON.stringify({ type: "resize", cols: terminal.cols, rows: terminal.rows }))
      }
    }

    const acknowledgeWrite = (target: WebSocket, bytes: number) => {
      if (target.readyState !== WebSocket.OPEN) return
      target.send(JSON.stringify({ type: "ack", bytes }))
    }

    const flushAcknowledgements = () => {
      for (const [target, bytes] of pendingAcks) acknowledgeWrite(target, bytes)
      pendingAcks.clear()
    }

    const fitIfHostChanged = () => {
      const { width, height } = host.getBoundingClientRect()
      const widthChanged = Math.abs(width - lastHostWidth) >= 0.5
      const heightDelta = Math.abs(height - lastHostHeight)
      // On mobile (coarse pointer), the browser URL bar showing/hiding changes
      // the viewport height (dvh) without changing width. Refitting on those
      // deltas is pure noise for the PTY, so only apply small height-only
      // changes on precise pointers; keep large ones (e.g. on-screen keyboard).
      const coarsePointer = window.matchMedia("(pointer: coarse)").matches
      const heightNoiseThreshold = coarsePointer ? 96 : 0
      if (!widthChanged && (heightDelta < 0.5 || heightDelta <= heightNoiseThreshold)) return
      lastHostWidth = width
      lastHostHeight = height
      fit.fit()
      sendResize()
    }

    const finalizeWrites = () => {
      while (commitQueue.length > 0) {
        const next = commitQueue[0]
        if (!next.parsed || (next.waitsForCursor && !next.cursor)) break
        if (next.cursor) cursor = next.cursor
        commitQueue.shift()
      }
    }

    const maybeReconnect = () => {
      if (disposed || !reconnectAfterWrites || commitQueue.length > 0) return
      reconnectAfterWrites = false
      const delay = Math.min(1_000 * 2 ** reconnectAttempt, 10_000)
      reconnectAttempt += 1
      reconnectTimer = window.setTimeout(() => connect(false), delay)
    }

    const queueOutput = (data: Uint8Array, currentSocket: WebSocket) => {
      let outputCursor: StreamCursor | null = null
      let waitsForCursor = true
      let refreshAfterWrite = false
      if (replayChunksRemaining > 0) {
        replayChunksRemaining -= 1
        waitsForCursor = false
        if (replayChunksRemaining === 0) {
          outputCursor = replayCursor
          replayCursor = null
          refreshAfterWrite = true
        }
      }
      const pending: PendingTerminalWrite = {
        bytes: data.byteLength,
        cursor: outputCursor,
        waitsForCursor,
        refreshAfterWrite,
        parsed: false,
      }
      pendingOutputBytes += data.byteLength
      commitQueue.push(pending)
      if (pendingOutputBytes > MAX_PENDING_OUTPUT_BYTES || commitQueue.length > MAX_PENDING_OUTPUT_WRITES) {
        resetOnReady = true
        currentSocket.close(1013, "terminal output backlog")
      }
      const flowControlled = flowControlActive
      // xterm batches queued writes into time slices. Feeding it every frame keeps
      // that batching intact; serializing callbacks here makes small-frame TUIs stall.
      terminal.write(data, () => {
        pending.parsed = true
        pendingOutputBytes = Math.max(0, pendingOutputBytes - pending.bytes)
        if (disposed) return
        if (flowControlled) {
          pendingAcks.set(currentSocket, (pendingAcks.get(currentSocket) ?? 0) + pending.bytes)
        }
        finalizeWrites()
        if (pending.refreshAfterWrite) {
          window.cancelAnimationFrame(refreshFrame)
          refreshFrame = window.requestAnimationFrame(() => {
            refreshFrame = 0
            if (!disposed) terminal.refresh(0, terminal.rows - 1)
          })
        }
        maybeReconnect()
      })
    }

    const connect = (initial = false) => {
      if (disposed) return
      window.clearTimeout(reconnectTimer)
      reconnectAllowed = true
      if (initial) onStatusChange("connecting")
      socket = new WebSocket(terminalUrl(workspaceId, agentId, terminal.cols, terminal.rows, cursor))
      socket.binaryType = "arraybuffer"
      const currentSocket = socket
      currentSocket.onmessage = (event) => {
        if (disposed || socket !== currentSocket) return
        if (event.data instanceof ArrayBuffer) {
          queueOutput(new Uint8Array(event.data), currentSocket)
          return
        }
        const message = JSON.parse(event.data) as TerminalControlMessage
        if (message.type === "ready") {
          reconnectAttempt = 0
          flowControlActive = message.replay_chunks != null
          if (resetOnReady || shouldResetTerminal(cursor, message)) terminal.reset()
          resetOnReady = false
          if (message.stream_epoch && message.revision != null) {
            replayCursor = { stream_epoch: message.stream_epoch, revision: message.revision }
            replayChunksRemaining = message.replay_chunks ?? 1
            if (replayChunksRemaining === 0) {
              cursor = replayCursor
              replayCursor = null
            }
          }
          onStatusChange("live")
          terminal.focus()
          flushPendingInput()
        } else if (message.type === "cursor") {
          if (message.stream_epoch && message.revision != null) {
            const pending = commitQueue.find((write) => write.waitsForCursor && !write.cursor)
            if (pending) {
              pending.cursor = { stream_epoch: message.stream_epoch, revision: message.revision }
              finalizeWrites()
            } else {
              cursor = { stream_epoch: message.stream_epoch, revision: message.revision }
            }
          }
        } else if (message.type === "closed") {
          reconnectAllowed = message.reason === "agent server disconnected"
          onStatusChange(reconnectAllowed ? "reconnecting" : "closed")
          if (message.reason && !reconnectAllowed) terminal.writeln(`\r\n\x1b[31m[treer] ${message.reason}\x1b[0m`)
        } else if (message.type === "error") {
          reconnectAllowed = false
          onStatusChange("error")
          terminal.writeln(`\r\n\x1b[31m[treer] ${message.error?.message ?? "terminal error"}\x1b[0m`)
        }
      }
      currentSocket.onerror = () => {}
      currentSocket.onclose = () => {
        if (disposed || socket !== currentSocket) return
        if (!active) {
          onStatusChange("closed")
          return
        }
        if (!reconnectAllowed) return
        onStatusChange("reconnecting")
        for (const pending of commitQueue) {
          if (pending.waitsForCursor && !pending.cursor) {
            pending.waitsForCursor = false
            resetOnReady = true
          }
        }
        finalizeWrites()
        reconnectAfterWrites = true
        maybeReconnect()
      }
    }

    const input = terminal.onData((data) => {
      send(transformInputRef.current?.(data) ?? data)
    })
    const parsed = terminal.onWriteParsed(flushAcknowledgements)
    const observer = new ResizeObserver(() => {
      window.clearTimeout(resizeTimer)
      resizeTimer = window.setTimeout(() => {
        if (disposed) return
        fitIfHostChanged()
      }, 60)
    })
    observer.observe(host)
    requestAnimationFrame(() => {
      if (disposed) return
      fitIfHostChanged()
      connect(true)
    })

    return () => {
      disposed = true
      window.clearTimeout(reconnectTimer)
      window.clearTimeout(resizeTimer)
      window.cancelAnimationFrame(refreshFrame)
      observer.disconnect()
      input.dispose()
      parsed.dispose()
      socket?.close()
      commitQueue.length = 0
      pendingAcks.clear()
      terminal.dispose()
      focusRef.current = () => undefined
      sendRef.current = () => undefined
    }
  }, [workspaceId, agentId, active, onStatusChange])

  if (!workspaceId) return <div className="grid h-full place-items-center text-xs text-zinc-500">No workspace selected</div>
  if (!agentId) return <div className="grid h-full place-items-center text-xs text-zinc-500">Select an agent to attach</div>
  return <div className="h-full min-h-0 min-w-0 w-full max-w-full p-3"><div ref={hostRef} className="h-full min-h-0 min-w-0 w-full max-w-full overflow-hidden" /></div>
})
