import { useRef, useState } from "react";
import { imageUrl, request, uploadImage } from "../api";
import { safeImageUrl } from "../lib/url";
import { t } from "../i18n";
import type { ImageAttachment } from "../types";

export function useImageDrafts(onError: (message: string) => void) {
  const [images, setImages] = useState<ImageAttachment[]>([]);
  const [busy, setBusy] = useState(false);
  const changing = useRef(false);
  const current = useRef(images); current.current = images;
  async function add(files: File[]) {
    if (!files.length || changing.current) return;
    if (files.some(file => !["image/png", "image/jpeg", "image/webp", "image/gif"].includes(file.type))) { onError(t("images.invalid")); return; }
    if (current.current.length + files.length > 8 || files.some(file => !file.size || file.size > 10 * 1024 * 1024) || current.current.reduce((n, image) => n + image.size, 0) + files.reduce((n, file) => n + file.size, 0) > 24 * 1024 * 1024) { onError(t("images.limits")); return; }
    changing.current = true; setBusy(true);
    try {
      for (const file of files) {
        const image = await uploadImage(file);
        current.current = [...current.current, image]; setImages(current.current);
      }
    } catch (error) { onError((error as Error).message); }
    finally { changing.current = false; setBusy(false); }
  }
  async function remove(id: string) {
    if (changing.current) return;
    changing.current = true; setBusy(true);
    try {
      await request(`api/attachments/${id}`, { method: "DELETE" });
      current.current = current.current.filter(image => image.id !== id); setImages(current.current);
    } catch (error) { onError((error as Error).message); }
    finally { changing.current = false; setBusy(false); }
  }
  function sent(ids: string[]) { current.current = current.current.filter(image => !ids.includes(image.id)); setImages(current.current); }
  return { images, busy, add, remove, sent };
}

export function ImageDrafts({ images, busy, disabled, fallback, onRemove }: { images: ImageAttachment[]; busy: boolean; disabled: boolean; fallback: boolean; onRemove(id: string): void }) {
  return <section className="image-drafts" aria-label={t("images.attached")}>
    <div className="image-list">{images.map((image, index) => <figure key={image.id}>
      <img src={safeImageUrl(imageUrl(image.id))} alt={t("images.preview", { number: index + 1 })} />
      <button type="button" disabled={disabled || busy} aria-label={t("images.remove", { number: index + 1 })} onClick={() => onRemove(image.id)}>×</button>
    </figure>)}</div>
    {busy && <p role="status">{t("images.uploading")}</p>}
    {images.length > 0 && <><p>{t("images.privacy")}</p>{fallback && <p role="status">{t("images.fallback")}</p>}</>}
  </section>;
}
