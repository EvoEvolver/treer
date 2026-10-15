// The only places a data-driven URL may reach href/src (enforced by scripts/check-ui-source.mjs).
const SAFE_LINK = /^(?:https?:|mailto:)/i;
const SAFE_IMAGE = /^(?:data:image\/(?:png|jpeg|gif|webp);base64,[A-Za-z0-9+/=]+$|blob:)/i;
const HAS_SCHEME = /^[a-z][a-z0-9+.-]*:/i;

/** http(s) and mailto links, or a plain relative reference; anything else (javascript:, data:, file:, //host) is dropped. */
export function safeUrl(value: string | null | undefined): string | undefined {
  const url = value?.trim();
  if (!url || url.startsWith("//") || /[\u0000-\u001f]/.test(url)) return undefined;
  if (SAFE_LINK.test(url)) return url;
  return HAS_SCHEME.test(url) ? undefined : url;
}

/** Same-origin relative images, data: raster images and blob: URLs. SVG is never inlined or embedded from data:. */
export function safeImageUrl(value: string | null | undefined): string | undefined {
  const url = value?.trim();
  if (!url || /[\u0000-\u001f]/.test(url)) return undefined;
  if (SAFE_IMAGE.test(url)) return url;
  return HAS_SCHEME.test(url) || url.startsWith("//") ? undefined : url;
}
