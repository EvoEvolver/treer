import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { t } from "../i18n";
import { safeImageUrl } from "../lib/url";
import { Icon } from "./Icon";

interface Dialogs { openText(title: string, text: string): void; openImage(src: string, alt: string): void }
const DialogsContext = createContext<Dialogs | null>(null);
export function useDialogs(): Dialogs {
  const value = useContext(DialogsContext);
  if (!value) throw new Error("DialogsProvider is missing");
  return value;
}

/** A native modal dialog supplies the focus trap, Esc and the backdrop; this wrapper only syncs it with `open`. */
function Modal({ open, onClose, label, className, children }: { open: boolean; onClose: () => void; label: string; className: string; children: ReactNode }) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    if (open && !element.open) element.showModal();
    else if (!open && element.open) element.close();
  }, [open]);
  return <dialog ref={ref} className={className} aria-label={label} onClose={onClose} onCancel={onClose}>{open ? children : null}</dialog>;
}

function TextView({ title, text, onClose }: { title: string; text: string; onClose: () => void }) {
  const [copied, setCopied] = useState(false);
  async function copy() {
    try { await navigator.clipboard.writeText(text); setCopied(true); setTimeout(() => setCopied(false), 1500); } catch { /* clipboard unavailable */ }
  }
  return (
    <>
      <header className="dialog-h"><h2>{title}</h2><span className="dialog-tools"><button type="button" className="btn" onClick={() => void copy()}>{copied ? t("dialog.copied") : t("dialog.copy")}</button><button type="button" className="ib" aria-label={t("dialog.close")} onClick={onClose}><Icon name="close" /></button></span></header>
      <pre className="dialog-text" tabIndex={0}>{text}</pre>
    </>
  );
}

function ImageView({ src, alt, onClose }: { src: string; alt: string; onClose: () => void }) {
  const [zoom, setZoom] = useState<"fit" | number>("fit");
  const [natural, setNatural] = useState(0);
  const image = useRef<HTMLImageElement>(null);
  const safe = safeImageUrl(src);
  const scale = typeof zoom === "number" ? zoom : natural && image.current ? image.current.clientWidth / natural : 1;
  const step = (factor: number) => setZoom(Math.min(8, Math.max(0.1, (typeof zoom === "number" ? zoom : scale || 1) * factor)));
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "+" || event.key === "=") step(1.25);
      else if (event.key === "-") step(0.8);
      else if (event.key === "0") setZoom("fit");
      else if (event.key === "1") setZoom(1);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  });
  return (
    <>
      <header className="dialog-h"><h2>{alt || t("image.viewer")}</h2>
        <span className="dialog-tools">
          <button type="button" className="btn" onClick={() => step(0.8)} aria-label={t("image.zoomOut")}>−</button>
          <button type="button" className="btn" onClick={() => setZoom("fit")}>{t("image.fit")}</button>
          <button type="button" className="btn" onClick={() => setZoom(1)}>{t("image.actual")}</button>
          <button type="button" className="btn" onClick={() => step(1.25)} aria-label={t("image.zoomIn")}>+</button>
          <button type="button" className="ib" aria-label={t("dialog.close")} onClick={onClose}><Icon name="close" /></button>
        </span>
      </header>
      <div className="image-stage" tabIndex={0}>
        {/* Only same-origin, data: raster and blob: URLs reach this element; SVG is only ever shown through <img>. */}
        {safe && <img ref={image} className={zoom === "fit" ? "fit" : "zoomed"} src={safe} alt={alt} onLoad={(event) => setNatural(event.currentTarget.naturalWidth)} width={typeof zoom === "number" && natural ? Math.round(natural * zoom) : undefined} />}
      </div>
    </>
  );
}

export function DialogsProvider({ children }: { children: ReactNode }) {
  const [text, setText] = useState<{ title: string; text: string } | null>(null);
  const [image, setImage] = useState<{ src: string; alt: string } | null>(null);
  const openText = useCallback((title: string, body: string) => setText({ title, text: body }), []);
  const openImage = useCallback((src: string, alt: string) => setImage({ src, alt }), []);
  const value = useMemo(() => ({ openText, openImage }), [openText, openImage]);
  return (
    <DialogsContext.Provider value={value}>
      {children}
      <Modal open={text !== null} onClose={() => setText(null)} label={text?.title ?? t("dialog.text")} className="text-dialog">{text && <TextView title={text.title} text={text.text} onClose={() => setText(null)} />}</Modal>
      <Modal open={image !== null} onClose={() => setImage(null)} label={image?.alt || t("image.viewer")} className="image-dialog">{image && <ImageView src={image.src} alt={image.alt} onClose={() => setImage(null)} />}</Modal>
    </DialogsContext.Provider>
  );
}
