import { forwardRef } from "react";
import { t } from "../i18n";
import { toggleTheme } from "../appearance";
import { useStore } from "../state/store";
import type { MessageKey } from "../i18n";
import { Icon } from "./Icon";

export type MenuTrigger = (element: HTMLButtonElement | null) => void;

/** The look button. Rendered in the header and, for the rail look, in the rail; CSS shows the one that fits. */
export const LookButton = forwardRef<HTMLButtonElement, { className: string; open: boolean; onToggle: () => void }>(function LookButton({ className, open, onToggle }, ref) {
  return (
    <button ref={ref} type="button" className={className} aria-haspopup="dialog" aria-expanded={open} aria-controls="appearance" aria-label={t("appearance.open")} title={t("appearance.open")} onClick={onToggle}>
      <Icon name="look" />
    </button>
  );
});

export function useStatus(): { state: "starting" | "working" | "blocked" | "idle" | "reconnecting"; label: string } {
  const { state } = useStore();
  const runtime = state.snapshot?.runtime.state ?? "starting";
  const key: "starting" | "working" | "blocked" | "idle" | "reconnecting" = state.connected ? runtime : "reconnecting";
  return { state: key, label: t(`status.${key}` as MessageKey) };
}

export function Header({ menuOpen, onToggleMenu, lookRef }: { menuOpen: boolean; onToggleMenu: () => void; lookRef: React.Ref<HTMLButtonElement> }) {
  const { state } = useStore();
  const status = useStatus();
  const name = state.snapshot?.harness.name || t("app.name");
  const title = state.snapshot?.thread?.title || name;
  return (
    <header className="top">
      <div className="brand">
        <span className="mark" aria-hidden="true"><span className="m-a">●</span><span className="m-b">{name.charAt(0).toUpperCase()}</span><span className="m-c">▌</span></span>
        <div className="titles">
          <h1>{title}</h1>
          <p className="cwd">{state.snapshot?.thread?.cwd || t("header.cwdConnecting")}</p>
        </div>
      </div>
      <div className="actions">
        <span className={`status status-${status.state}`} role="status"><i className="dot" /><span className="status-t">{status.label}</span></span>
        <LookButton ref={lookRef} className="tbtn" open={menuOpen} onToggle={onToggleMenu} />
        <button type="button" className="tbtn" aria-label={t("appearance.toggleTheme")} title={t("appearance.toggleTheme")} onClick={toggleTheme}><Icon name="theme" /></button>
      </div>
    </header>
  );
}

export function Rail({ menuOpen, onToggleMenu }: { menuOpen: boolean; onToggleMenu: () => void }) {
  return (
    <nav className="rail" aria-label={t("tabbar.label")}>
      <span className="rail-sp" />
      <LookButton className="rbtn" open={menuOpen} onToggle={onToggleMenu} />
      <button type="button" className="rbtn" aria-label={t("appearance.toggleTheme")} title={t("appearance.toggleTheme")} onClick={toggleTheme}><Icon name="theme" /></button>
    </nav>
  );
}

export function Tabbar({ menuOpen, onToggleMenu }: { menuOpen: boolean; onToggleMenu: () => void }) {
  return (
    <nav className="tabbar" aria-label={t("tabbar.label")}>
      <button type="button" aria-haspopup="dialog" aria-expanded={menuOpen} aria-controls="appearance" onClick={onToggleMenu}><Icon name="look" /><span>{t("tabbar.look")}</span></button>
      <button type="button" onClick={toggleTheme}><Icon name="theme" /><span>{t("tabbar.theme")}</span></button>
    </nav>
  );
}
