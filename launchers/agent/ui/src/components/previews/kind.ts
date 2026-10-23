export type PreviewKind = "markdown" | "image" | "pdf" | "text";
export function previewKind(path: string): PreviewKind {
  const ext = path.split(".").pop()?.toLowerCase();
  if (ext === "md" || ext === "markdown") return "markdown";
  // SVG/HTML are deliberately text, never active documents.
  if (["png", "jpg", "jpeg", "gif", "webp", "avif", "bmp"].includes(ext ?? "")) return "image";
  return ext === "pdf" ? "pdf" : "text";
}
