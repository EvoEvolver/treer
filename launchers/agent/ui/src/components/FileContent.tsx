import { useEffect, useState, type ComponentType } from "react";
import { t } from "../i18n";
import type { FilePreview } from "../types";
import { previewKind } from "./previews/kind";

// Literal local imports only. No rich viewer is requested merely by mounting
// FilesPanel, listing a directory or selecting text/source view.
const loaders = {
  markdown: () => import("./previews/MarkdownPreview"),
  image: () => import("./previews/ImagePreview"),
  pdf: () => import("./previews/PdfPreview"),
};
export function FileContent({ file }: { file: FilePreview }) {
  const kind = previewKind(file.path);
  const [source, setSource] = useState(false);
  const [Viewer, setViewer] = useState<ComponentType<{ path: string; content: string }> | null>(null);
  const [error, setError] = useState(false);
  const rich = kind !== "text" && !source;
  // Rich binary previews have a strict in-browser streaming budget, even when
  // source/list metadata is stale. Text/Markdown keep the server's 64KiB cap.
  const tooLarge = (kind === "pdf" || kind === "image") && file.size > 20 * 1024 * 1024;
  useEffect(() => {
    if (!rich || tooLarge) return;
    let cancelled = false;
    void loaders[kind as keyof typeof loaders]().then(module => {
      if (!cancelled) setViewer(() => module.default);
    }).catch(() => { if (!cancelled) setError(true); });
    return () => { cancelled = true; };
  }, [kind, rich, tooLarge]);
  return <>
    {kind === "markdown" && <div className="preview-tools" role="group" aria-label={t("files.preview")}>
      <button className="btn" type="button" aria-pressed={!source} onClick={() => setSource(false)}>{t("files.rendered")}</button>
      <button className="btn" type="button" aria-pressed={source} onClick={() => setSource(true)}>{t("files.source")}</button>
    </div>}
    {!rich ? <pre tabIndex={0}>{file.content}</pre> : tooLarge ? <p role="alert">{t("files.previewLimit")}</p> : error ? <p role="alert">{t("files.previewFailed")}</p> : Viewer ? <Viewer path={file.path} content={file.content} /> : <p role="status">{t("files.loading")}</p>}
  </>;
}
