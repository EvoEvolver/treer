import { useEffect, useRef } from "react";
import { setLook, setThemeMode, useAppearance, type Look, type ThemeMode } from "../appearance";
import { t, type MessageKey } from "../i18n";
import { Icon } from "./Icon";

const looks: Look[] = ["a", "b", "c"];
const modes: ThemeMode[] = ["system", "light", "dark"];

/** Look (3 identities) and theme (system/light/dark) pickers: radio groups in a dialog; Esc and outside clicks close it. */
export function AppearanceMenu({ open, onClose }: { open: boolean; onClose: () => void }) {
  const appearance = useAppearance();
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    ref.current?.querySelector<HTMLElement>('[aria-checked="true"]')?.focus({ preventScroll: true });
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape") onClose(); };
    const onPointer = (event: PointerEvent) => {
      const target = event.target as Element | null;
      if (!ref.current?.contains(target) && !target?.closest("[aria-controls=appearance]")) onClose();
    };
    document.addEventListener("keydown", onKey);
    document.addEventListener("pointerdown", onPointer);
    return () => { document.removeEventListener("keydown", onKey); document.removeEventListener("pointerdown", onPointer); };
  }, [open, onClose]);
  if (!open) return null;
  return (
    <div className="menu" id="appearance" role="dialog" aria-label={t("appearance.title")} ref={ref}>
      <header className="mh"><h2>{t("appearance.title")}</h2><button type="button" className="xbtn" aria-label={t("appearance.close")} onClick={onClose}><Icon name="close" /></button></header>
      <div className="mgroup" role="radiogroup" aria-labelledby="ml-look">
        <p className="mlabel" id="ml-look">{t("appearance.look")}</p>
        {looks.map((look) => (
          <button key={look} type="button" className="mopt" role="radio" aria-checked={appearance.look === look} onClick={() => setLook(look)}>
            <span className={`sw sw-${look}`} aria-hidden="true"><i /></span>
            <span className="mt"><b>{t(`look.${look}.name` as MessageKey)}</b><small>{t(`look.${look}.note` as MessageKey)}</small></span>
          </button>
        ))}
      </div>
      <div className="mgroup mtheme" role="radiogroup" aria-labelledby="ml-theme">
        <p className="mlabel" id="ml-theme">{t("appearance.theme")}</p>
        <div className="mrow">
          {modes.map((mode) => <button key={mode} type="button" className="mchip" role="radio" aria-checked={appearance.mode === mode} onClick={() => setThemeMode(mode)}>{t(`theme.${mode}` as MessageKey)}</button>)}
        </div>
      </div>
    </div>
  );
}
