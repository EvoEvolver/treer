import { useEffect, useRef, useState } from "react";
import { getDocument, GlobalWorkerOptions, type PDFDocumentProxy } from "pdfjs-dist";
import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs";
import { previewBytes } from "../../api";
import { t } from "../../i18n";

// esbuild's file loader returns a local hashed JS asset; this module is only
// imported after PDF selection, so neither engine nor worker loads at startup.
GlobalWorkerOptions.workerSrc = new URL(workerUrl, import.meta.url).href;
export default function PdfPreview({ path }: { path: string }) {
  const [document, setDocument] = useState<PDFDocumentProxy | null>(null);
  const [page, setPage] = useState(1);
  const [zoom, setZoom] = useState(1);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(true);
  const canvas = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const controller = new AbortController();
    let task: ReturnType<typeof getDocument> | undefined;
    void previewBytes(path, controller.signal).then(async bytes => {
      if (controller.signal.aborted) return;
      task = getDocument({ data: bytes, useWasm: false, useWorkerFetch: false, useSystemFonts: true, disableFontFace: true, isImageDecoderSupported: false });
      const pdf = await task.promise;
      if (!controller.signal.aborted) setDocument(pdf);
    }).catch(() => { if (!controller.signal.aborted) { setError(t("files.previewFailed")); setBusy(false); } });
    return () => { controller.abort(); void task?.destroy(); };
  }, [path]);
  useEffect(() => {
    if (!document) return;
    let cancelled = false;
    let render: ReturnType<Awaited<ReturnType<PDFDocumentProxy["getPage"]>>["render"]> | undefined;
    setBusy(true);
    void document.getPage(page).then(async pdfPage => {
      if (cancelled || !canvas.current) return;
      const original = pdfPage.getViewport({ scale: 1 });
      const fit = Math.min(1, Math.max(100, canvas.current.parentElement?.clientWidth ?? original.width) / original.width);
      // Bound the canvas to 4M pixels and 4096px in either dimension.
      const scale = Math.min(fit * zoom, 4096 / original.width, 4096 / original.height, Math.sqrt(4_000_000 / (original.width * original.height)));
      const viewport = pdfPage.getViewport({ scale });
      const node = canvas.current;
      node.width = Math.ceil(viewport.width); node.height = Math.ceil(viewport.height);
      render = pdfPage.render({ canvas: node, viewport });
      await render.promise;
      if (!cancelled) setBusy(false);
      pdfPage.cleanup();
    }).catch(() => { if (!cancelled) { setError(t("files.previewFailed")); setBusy(false); } });
    return () => { cancelled = true; render?.cancel(); };
  }, [document, page, zoom]);
  return <div className="file-pdf">
    <div className="preview-tools" role="group" aria-label={t("files.pdfControls")}>
      <button className="btn" type="button" disabled={!document || page <= 1} onClick={() => setPage(page - 1)}>{t("files.previous")}</button>
      <span aria-live="polite">{t("files.page", { n: page, total: document?.numPages ?? "—" })}</span>
      <button className="btn" type="button" disabled={!document || page >= document.numPages} onClick={() => setPage(page + 1)}>{t("files.next")}</button>
      <button className="btn" type="button" disabled={zoom <= .5} aria-label={t("files.zoomOut")} onClick={() => setZoom(Math.max(.5, zoom - .25))}>−</button>
      <span>{Math.round(zoom * 100)}%</span>
      <button className="btn" type="button" disabled={zoom >= 3} aria-label={t("files.zoomIn")} onClick={() => setZoom(Math.min(3, zoom + .25))}>+</button>
    </div>
    {error && <p role="alert">{error}</p>}
    {busy && !error && <p role="status">{t("files.loading")}</p>}
    <div className="pdf-scroll"><canvas ref={canvas} aria-label={t("files.page", { n: page, total: document?.numPages ?? "—" })} role="img" /></div>
  </div>;
}
