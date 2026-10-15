import type { RefObject } from "react"
import { ArrowDown, ArrowLeft, ArrowRight, ArrowUp, CornerDownLeft, Delete, Keyboard } from "lucide-react"
import { MobileTerminalKey } from "@/components/control-plane-ui"
import type { TerminalPaneHandle } from "@/components/terminal-pane"

export function MobileTerminalKeys({ terminalRef, ctrlArmed, onToggleCtrl }: { terminalRef: RefObject<TerminalPaneHandle | null>; ctrlArmed: boolean; onToggleCtrl: () => void }) {
  return <div className="border-t border-zinc-800 bg-[#191d20] px-2 pt-2 pb-[max(0.5rem,env(safe-area-inset-bottom))]">
              <div className="grid grid-cols-6 gap-1.5">
                <MobileTerminalKey label="Escape" onClick={() => terminalRef.current?.send("\x1b")}>Esc</MobileTerminalKey>
                <MobileTerminalKey label="Tab" onClick={() => terminalRef.current?.send("\t")}>Tab</MobileTerminalKey>
                <MobileTerminalKey label="Control modifier for next key" active={ctrlArmed} onClick={onToggleCtrl}>Ctrl</MobileTerminalKey>
                <MobileTerminalKey label="Open keyboard" onClick={() => terminalRef.current?.focus()}><Keyboard className="size-4" /></MobileTerminalKey>
                <MobileTerminalKey label="Backspace" onClick={() => terminalRef.current?.send("\x7f")}><Delete className="size-4" /></MobileTerminalKey>
                <MobileTerminalKey label="Enter" onClick={() => terminalRef.current?.send("\r")}><CornerDownLeft className="size-4" /></MobileTerminalKey>
              </div>
              <div className="mt-1.5 grid grid-cols-8 gap-1.5">
                <MobileTerminalKey label="Control C" onClick={() => terminalRef.current?.send("\x03")}>^C</MobileTerminalKey>
                <MobileTerminalKey label="Control D" onClick={() => terminalRef.current?.send("\x04")}>^D</MobileTerminalKey>
                <MobileTerminalKey label="Control Z" onClick={() => terminalRef.current?.send("\x1a")}>^Z</MobileTerminalKey>
                <MobileTerminalKey label="Control L" onClick={() => terminalRef.current?.send("\x0c")}>^L</MobileTerminalKey>
                <MobileTerminalKey label="Left arrow" onClick={() => terminalRef.current?.send("\x1b[D")}><ArrowLeft className="size-4" /></MobileTerminalKey>
                <MobileTerminalKey label="Up arrow" onClick={() => terminalRef.current?.send("\x1b[A")}><ArrowUp className="size-4" /></MobileTerminalKey>
                <MobileTerminalKey label="Down arrow" onClick={() => terminalRef.current?.send("\x1b[B")}><ArrowDown className="size-4" /></MobileTerminalKey>
                <MobileTerminalKey label="Right arrow" onClick={() => terminalRef.current?.send("\x1b[C")}><ArrowRight className="size-4" /></MobileTerminalKey>
              </div>
            </div>
}
