import DOMPurify from "dompurify";
import { marked } from "marked";
import { memo, useMemo, type KeyboardEvent, type MouseEvent } from "react";
import { safeImageUrl, safeUrl } from "../lib/url";
import { useDialogs } from "./Dialogs";

// THE ONLY HTML sink in the UI. Source text is parsed by marked, then sanitized by DOMPurify before it reaches the DOM.
// Styles are forbidden (CSP), links always open safely, and images may only be same-origin, data: raster or blob:.
marked.use({ breaks: true, gfm: true });
DOMPurify.addHook("afterSanitizeAttributes", (node) => {
  if (node.tagName === "A" && node.hasAttribute("href")) {
    // DOMPurify's own URI list also allows ftp:, tel:, sms: and more; links here are http(s), mailto or relative only.
    const href = safeUrl(node.getAttribute("href"));
    if (href) {
      node.setAttribute("href", href);
      node.setAttribute("target", "_blank");
      node.setAttribute("rel", "noopener noreferrer");
    } else node.removeAttribute("href");
  }
  if (node.tagName === "IMG") {
    const safe = safeImageUrl(node.getAttribute("src"));
    if (safe) {
      node.setAttribute("src", safe);
      node.setAttribute("tabindex", "0");
      node.setAttribute("role", "button");
    }
    else {
      // A blocked image becomes its alt text, never a request and never a broken-image icon.
      const label = document.createElement("span");
      label.className = "blocked-image";
      label.textContent = `[${node.getAttribute("alt") || "image"}]`;
      node.replaceWith(label);
    }
  }
});

export function renderMarkdown(source: string): string {
  const html = marked.parse(source, { async: false });
  return DOMPurify.sanitize(html, { FORBID_ATTR: ["style"], FORBID_TAGS: ["style", "form", "input", "button", "textarea", "select"], SANITIZE_NAMED_PROPS: true, USE_PROFILES: { html: true } });
}

export const Markdown = memo(function Markdown({ text, className }: { text: string; className?: string }) {
  const html = useMemo(() => renderMarkdown(text), [text]);
  const { openImage } = useDialogs();
  // Clicking (or Enter on) a sanitized image opens the viewer; the src was already vetted by safeImageUrl in the hook above.
  const open = (target: EventTarget) => {
    const image = target instanceof HTMLImageElement ? target : null;
    const src = safeImageUrl(image?.getAttribute("src"));
    if (image && src) openImage(src, image.getAttribute("alt") ?? "");
  };
  return (
    <div className={className ? `markdown ${className}` : "markdown"} dangerouslySetInnerHTML={{ __html: html }}
      onClick={(event: MouseEvent) => open(event.target)} onKeyDown={(event: KeyboardEvent) => { if (event.key === "Enter" || event.key === " ") { open(event.target); } }} />
  );
});
