import { useEffect, useState } from "react";
import { previewBytes } from "../../api";
import { t } from "../../i18n";
import { safeImageUrl } from "../../lib/url";

export default function ImagePreview({ path }: { path: string }) {
  const [url, setUrl] = useState("");
  const [error, setError] = useState("");
  const [zoom, setZoom] = useState(false);
  useEffect(() => {
    const controller = new AbortController();
    let objectUrl: string | undefined;
    void previewBytes(path, controller.signal).then(bytes => {
      if (controller.signal.aborted) return;
      const extension = path.split(".").pop()?.toLowerCase();
      const mime = extension === "jpg" ? "jpeg" : extension === "bmp" ? "bmp" : extension;
      objectUrl = URL.createObjectURL(new Blob([bytes], { type: `image/${mime}` }));
      setUrl(objectUrl);
    }).catch(() => { if (!controller.signal.aborted) setError(t("files.previewFailed")); });
    return () => { controller.abort(); if (objectUrl) URL.revokeObjectURL(objectUrl); };
  }, [path]);
  return <div className="file-image">
    <div className="preview-tools"><button className="btn" type="button" aria-pressed={zoom} onClick={() => setZoom(!zoom)}>{t(zoom ? "files.fit" : "files.actualSize")}</button></div>
    {error ? <p role="alert">{error}</p> : url ? <div className={zoom ? "image-scroll actual" : "image-scroll"}><img src={safeImageUrl(url)} alt={path} onError={() => setError(t("files.previewFailed"))} /></div> : <p role="status">{t("files.loading")}</p>}
  </div>;
}
