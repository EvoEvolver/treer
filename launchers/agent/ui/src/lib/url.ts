// The only places a data-driven URL may reach href/src (enforced by scripts/check-ui-source.mjs).
const SAFE_LINK = /^(?:https?:|mailto:)/i;
const SAFE_IMAGE = /^(?:data:image\/(?:png|jpeg|gif|webp);base64,[A-Za-z0-9+/=]+$|blob:)/i;
const HAS_SCHEME = /^[a-z][a-z0-9+.-]*:/i;

/** Resolve a transcript file reference for the existing cwd-bound preview API.
 * Outside paths stay absolute so the server rejects them; never strip a root
 * from a different directory. This is presentation, not an access check.
 */
export function workspaceFilePath(value: string | null | undefined, cwd: string): string | undefined {
  let path = value?.trim();
  if (!path || /^[#?]/.test(path) || path.startsWith("//") || /[\u0000-\u001f\u007f]/.test(path)) return undefined;
  if (/^file:/i.test(path)) {
    try {
      const url = new URL(path);
      if (url.hostname && url.hostname !== "localhost") return undefined;
      path = url.pathname.replace(/^\/([a-z]:\/)/i, "$1");
    } catch { return undefined; }
  } else {
    if (HAS_SCHEME.test(path) && !/^[a-z]:[\\/]/i.test(path)) return undefined;
    path = path.split(/[?#]/, 1)[0]?.replace(/:\d+(?::\d+)?$/, "") ?? "";
  }
  try { path = decodeURIComponent(path); } catch { return undefined; }
  if (!path || /[\u0000-\u001f\u007f]/.test(path) || path.startsWith("//")) return undefined;
  // Backslashes are accepted only as Windows file separators, never URL hosts.
  if (path.includes("\\") && !/^[a-z]:[\\/]/i.test(path)) return undefined;
  path = path.replaceAll("\\", "/");
  if (HAS_SCHEME.test(path) && !/^[a-z]:\//i.test(path)) return undefined;
  const root = cwd.replaceAll("\\", "/").replace(/^\/\/\?\//, "").replace(/\/+$/, "");
  const windows = /^[a-z]:\//i.test(root);
  const comparedPath = windows ? path.toLowerCase() : path;
  const comparedRoot = windows ? root.toLowerCase() : root;
  if (root && comparedPath.startsWith(comparedRoot + "/")) path = path.slice(root.length + 1);
  return path.replace(/^(?:\.\/)+/, "");
}

/** http(s) and mailto links, or a plain relative reference; anything else (javascript:, data:, file:, //host) is dropped. */
export function safeUrl(value: string | null | undefined): string | undefined {
  const url = value?.trim();
  // A backslash can be read as a slash ("/\\host"), so it never counts as a relative reference.
  if (!url || url.startsWith("//") || /[\u0000-\u001f\\]/.test(url)) return undefined;
  if (SAFE_LINK.test(url)) return url;
  return HAS_SCHEME.test(url) ? undefined : url;
}

/** Same-origin relative images, data: raster images and blob: URLs. SVG is never inlined or embedded from data:. */
export function safeImageUrl(value: string | null | undefined): string | undefined {
  const url = value?.trim();
  if (!url || /[\u0000-\u001f\\]/.test(url)) return undefined;
  if (SAFE_IMAGE.test(url)) return url;
  return HAS_SCHEME.test(url) || url.startsWith("//") ? undefined : url;
}
