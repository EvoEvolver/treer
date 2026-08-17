import { useEffect, useRef } from "react"
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
}

export function TerminalPane({ workspaceId, agentId, active, onStatusChange }: TerminalPaneProps) {
  const hostRef = useRef<HTMLDivElement>(null)

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
    const terminal = new Terminal({
      cursorBlink: true,
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

    const sendResize = () => {
      if (socket?.readyState === WebSocket.OPEN) {
        socket.send(JSON.stringify({ type: "resize", cols: terminal.cols, rows: terminal.rows }))
      }
    }

    const connect = () => {
      if (disposed) return
      window.clearTimeout(reconnectTimer)
      onStatusChange("connecting")
      socket = new WebSocket(websocketUrl(`/api/workspaces/${encodeURIComponent(workspaceId)}/agents/${encodeURIComponent(agentId)}/terminal?cols=${terminal.cols}&rows=${terminal.rows}`))
      socket.binaryType = "arraybuffer"
      const currentSocket = socket
      currentSocket.onmessage = (event) => {
        if (disposed || socket !== currentSocket) return
        if (event.data instanceof ArrayBuffer) {
          terminal.write(new Uint8Array(event.data))
          return
        }
        const message = JSON.parse(event.data) as { type: string; reason?: string; error?: { message?: string } }
        if (message.type === "ready") {
          terminal.reset()
          onStatusChange("live")
          terminal.focus()
        } else if (message.type === "closed") {
          onStatusChange(message.reason === "agent server disconnected" ? "reconnecting" : "closed")
          if (message.reason && message.reason !== "agent server disconnected") terminal.writeln(`\r\n\x1b[31m[treer] ${message.reason}\x1b[0m`)
        } else if (message.type === "error") {
          onStatusChange("error")
          terminal.writeln(`\r\n\x1b[31m[treer] ${message.error?.message ?? "terminal error"}\x1b[0m`)
        }
      }
      currentSocket.onerror = () => { if (!disposed && socket === currentSocket) onStatusChange("error") }
      currentSocket.onclose = () => {
        if (disposed || socket !== currentSocket) return
        if (!active) {
          onStatusChange("closed")
          return
        }
        onStatusChange("reconnecting")
        reconnectTimer = window.setTimeout(connect, 700)
      }
    }

    const input = terminal.onData((data) => {
      if (socket?.readyState === WebSocket.OPEN) socket.send(new TextEncoder().encode(data))
    })
    const observer = new ResizeObserver(() => {
      window.clearTimeout(resizeTimer)
      resizeTimer = window.setTimeout(() => {
        if (disposed) return
        fit.fit()
        sendResize()
      }, 60)
    })
    observer.observe(host)
    requestAnimationFrame(() => {
      if (disposed) return
      fit.fit()
      connect()
    })

    return () => {
      disposed = true
      window.clearTimeout(reconnectTimer)
      window.clearTimeout(resizeTimer)
      observer.disconnect()
      input.dispose()
      socket?.close()
      terminal.dispose()
    }
  }, [workspaceId, agentId, active, onStatusChange])

  if (!workspaceId) return <div className="grid h-full place-items-center text-xs text-zinc-500">No workspace selected</div>
  if (!agentId) return <div className="grid h-full place-items-center text-xs text-zinc-500">Select an agent to attach</div>
  return <div ref={hostRef} className="h-full min-h-0 w-full overflow-hidden p-3" />
}
