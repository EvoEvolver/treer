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
  reason?: string
  error?: { message?: string }
}

const MAX_PENDING_INPUT_BYTES = 32_768

function shouldResetTerminal(cursor: StreamCursor | null, message: TerminalControlMessage) {
  if (message.gap || !message.stream_epoch) return true
  return cursor?.stream_epoch !== message.stream_epoch
}

function terminalUrl(workspaceId: string, agentId: string, cols: number, rows: number, cursor: StreamCursor | null) {
  const params = new URLSearchParams({ cols: String(cols), rows: String(rows) })
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
    let reconnectAttempt = 0
    let reconnectAllowed = true
    let lastHostWidth = 0
    let lastHostHeight = 0
    let cursor: StreamCursor | null = null
    const pendingInput: string[] = []
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
          terminal.write(new Uint8Array(event.data))
          return
        }
        const message = JSON.parse(event.data) as TerminalControlMessage
        if (message.type === "ready") {
          reconnectAttempt = 0
          if (shouldResetTerminal(cursor, message)) terminal.reset()
          if (message.stream_epoch && message.revision != null) {
            cursor = { stream_epoch: message.stream_epoch, revision: message.revision }
          }
          onStatusChange("live")
          terminal.focus()
          flushPendingInput()
        } else if (message.type === "cursor") {
          if (message.stream_epoch && message.revision != null) {
            cursor = { stream_epoch: message.stream_epoch, revision: message.revision }
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
        const delay = Math.min(1_000 * 2 ** reconnectAttempt, 10_000)
        reconnectAttempt += 1
        reconnectTimer = window.setTimeout(() => connect(false), delay)
      }
    }

    const input = terminal.onData((data) => {
      send(transformInputRef.current?.(data) ?? data)
    })
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
      observer.disconnect()
      input.dispose()
      socket?.close()
      terminal.dispose()
      focusRef.current = () => undefined
      sendRef.current = () => undefined
    }
  }, [workspaceId, agentId, active, onStatusChange])

  if (!workspaceId) return <div className="grid h-full place-items-center text-xs text-zinc-500">No workspace selected</div>
  if (!agentId) return <div className="grid h-full place-items-center text-xs text-zinc-500">Select an agent to attach</div>
  return <div className="h-full min-h-0 min-w-0 w-full max-w-full p-3"><div ref={hostRef} className="h-full min-h-0 min-w-0 w-full max-w-full overflow-hidden" /></div>
})
