import type { RefObject } from "react"
import { ArrowDown, ArrowLeft, ArrowRight, ArrowUp, CornerDownLeft, Delete, Keyboard } from "lucide-react"
import { MobileTerminalKey } from "@/components/control-plane-ui"
import type { TerminalPaneHandle } from "@/components/terminal-pane"
import { t } from "@/i18n"

export function MobileTerminalKeys({ terminalRef, ctrlArmed, onToggleCtrl }: { terminalRef: RefObject<TerminalPaneHandle | null>; ctrlArmed: boolean; onToggleCtrl: () => void }) {
  return <div className="border-t border-zinc-800 bg-[#191d20] px-2 pt-2 pb-[max(0.5rem,env(safe-area-inset-bottom))]">
              <div className="grid grid-cols-6 gap-1.5">
                <MobileTerminalKey label={t("keys.escape")} onClick={() => terminalRef.current?.send("\x1b")}>{t("keys.esc")}</MobileTerminalKey>
                <MobileTerminalKey label={t("keys.tab")} onClick={() => terminalRef.current?.send("\t")}>{t("keys.tab")}</MobileTerminalKey>
                <MobileTerminalKey label={t("keys.control_modifier_for_next_key")} active={ctrlArmed} onClick={onToggleCtrl}>{t("keys.ctrl")}</MobileTerminalKey>
                <MobileTerminalKey label={t("keys.open_keyboard")} onClick={() => terminalRef.current?.focus()}><Keyboard className="size-4" /></MobileTerminalKey>
                <MobileTerminalKey label={t("keys.backspace")} onClick={() => terminalRef.current?.send("\x7f")}><Delete className="size-4" /></MobileTerminalKey>
                <MobileTerminalKey label={t("keys.enter")} onClick={() => terminalRef.current?.send("\r")}><CornerDownLeft className="size-4" /></MobileTerminalKey>
              </div>
              <div className="mt-1.5 grid grid-cols-8 gap-1.5">
                <MobileTerminalKey label={t("keys.control_c")} onClick={() => terminalRef.current?.send("\x03")}>{t("keys.c")}</MobileTerminalKey>
                <MobileTerminalKey label={t("keys.control_d")} onClick={() => terminalRef.current?.send("\x04")}>{t("keys.d")}</MobileTerminalKey>
                <MobileTerminalKey label={t("keys.control_z")} onClick={() => terminalRef.current?.send("\x1a")}>{t("keys.z")}</MobileTerminalKey>
                <MobileTerminalKey label={t("keys.control_l")} onClick={() => terminalRef.current?.send("\x0c")}>{t("keys.l")}</MobileTerminalKey>
                <MobileTerminalKey label={t("keys.left_arrow")} onClick={() => terminalRef.current?.send("\x1b[D")}><ArrowLeft className="size-4" /></MobileTerminalKey>
                <MobileTerminalKey label={t("keys.up_arrow")} onClick={() => terminalRef.current?.send("\x1b[A")}><ArrowUp className="size-4" /></MobileTerminalKey>
                <MobileTerminalKey label={t("keys.down_arrow")} onClick={() => terminalRef.current?.send("\x1b[B")}><ArrowDown className="size-4" /></MobileTerminalKey>
                <MobileTerminalKey label={t("keys.right_arrow")} onClick={() => terminalRef.current?.send("\x1b[C")}><ArrowRight className="size-4" /></MobileTerminalKey>
              </div>
            </div>
}
