import { useEffect, useRef } from "react";
import { setThemeMode, useAppearance, type ThemeMode } from "../appearance";
import { t, type MessageKey } from "../i18n";
import { exportName, toMarkdown } from "../lib/export";
import { useStore } from "../state/store";
import { Icon } from "./Icon";

const modes: ThemeMode[] = ["system", "light", "dark"];

/** Model and reasoning effort for the current thread. Also shown from the composer's model chip. */
export function ModelFields() {
  const { state, actions } = useStore();
  const thread = state.snapshot?.thread;
  const models = state.snapshot?.modelOptions ?? [];
  const selected = models.find((option) => option.model === thread?.model);
  const efforts = selected?.supportedReasoningEfforts ?? [];
  const busy = state.settingsBusy || !thread;
  if (!thread || models.length === 0) return <p className="muted">{t("model.unavailable")}</p>;
  return (
    <>
      <div className="field"><label htmlFor="model-select">{t("model.model")}</label>
        <select id="model-select" value={thread.model ?? ""} disabled={busy} onChange={(event) => void actions.settings({ model: event.target.value })}>
          {/* A harness may sit on its own default, which is not one of the choices. */}
          {!models.some((option) => option.model === thread.model) && <option value="">{t(thread.modelSelectionStatus === "unknown" ? "model.unknown" : "model.default")}</option>}
          {models.map((option) => <option key={option.id} value={option.model}>{option.displayName || option.model}</option>)}
        </select>
      </div>
      {efforts.length > 0 && (
        <div className="field"><label htmlFor="effort-select">{t("model.effort")}</label>
          <select id="effort-select" value={thread.effortSelectionStatus === "unknown" ? "__unknown" : thread.reasoningEffort ?? ""} disabled={busy} onChange={(event) => void actions.settings({ reasoningEffort: event.target.value || null })}>
            {thread.effortSelectionStatus === "unknown" && <option value="__unknown" disabled>{t("model.effortUnknown")}</option>}
            <option value="">{t("model.default")}</option>
            {efforts.map((option) => <option key={option.reasoningEffort} value={option.reasoningEffort}>{option.reasoningEffort}</option>)}
          </select>
        </div>
      )}
    </>
  );
}

/** Model, theme and export in one place, anchored under the header (a bottom sheet on phones). */
export function Settings({ open, onClose }: { open: boolean; onClose: (returnFocus: boolean) => void }) {
  const appearance = useAppearance();
  const { state, dispatch } = useStore();
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    ref.current?.querySelector<HTMLElement>("select:not(:disabled), input:checked")?.focus({ preventScroll: true });
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape" && !document.querySelector("dialog[open]")) { event.preventDefault(); onClose(true); } };
    const onPointer = (event: PointerEvent) => {
      const target = event.target as Element | null;
      if (!ref.current?.contains(target) && !target?.closest("[aria-controls=settings]")) onClose(false);
    };
    document.addEventListener("keydown", onKey);
    document.addEventListener("pointerdown", onPointer);
    return () => { document.removeEventListener("keydown", onKey); document.removeEventListener("pointerdown", onPointer); };
  }, [open, onClose]);
  if (!open) return null;
  /** Client-side only: the conversation already loaded in the page becomes a downloaded .md file. */
  function exportMarkdown() {
    const snapshot = state.snapshot;
    if (!snapshot || !(snapshot.thread?.turns.length)) { dispatch({ type: "notice", message: t("export.empty") }); return; }
    const url = URL.createObjectURL(new Blob([toMarkdown(snapshot)], { type: "text/markdown;charset=utf-8" }));
    const link = document.createElement("a");
    link.href = url;
    link.download = exportName(snapshot);
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
    onClose(true);
  }
  return (
    <div className="sheet settings" id="settings" role="dialog" aria-labelledby="settings-title" ref={ref}>
      <header className="sheet-h"><h2 id="settings-title">{t("settings.title")}</h2><button type="button" className="ib" aria-label={t("settings.close")} onClick={() => onClose(true)}><Icon name="close" /></button></header>
      <section aria-labelledby="settings-model"><h3 id="settings-model">{t("model.title")}</h3><ModelFields /></section>
      <section>
        <h3 id="settings-theme">{t("appearance.theme")}</h3>
        <div className="segmented" role="radiogroup" aria-labelledby="settings-theme">
          {modes.map((mode) => <label key={mode}><input type="radio" name="theme-mode" value={mode} checked={appearance.mode === mode} onChange={() => setThemeMode(mode)} /><span>{t(`theme.${mode}` as MessageKey)}</span></label>)}
        </div>
      </section>
      <section aria-labelledby="settings-export">
        <h3 id="settings-export">{t("export.title")}</h3>
        <button type="button" className="btn" onClick={exportMarkdown}><Icon name="download" />{t("export.markdown")}</button>
      </section>
    </div>
  );
}
