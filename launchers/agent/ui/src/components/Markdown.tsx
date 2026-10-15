import DOMPurify from "dompurify";
import { marked } from "marked";
import { memo, useMemo, type KeyboardEvent, type MouseEvent } from "react";
import { safeImageUrl, safeUrl, workspaceFilePath } from "../lib/url";
import { useDialogs } from "./Dialogs";
import { useFileNavigation } from "../state/files";

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

export function renderMarkdown(source: string, cwd = ""): string {
  const renderer = new marked.Renderer();
  renderer.link = function (token) {
    const path = workspaceFilePath(token.href, cwd);
    // Encode each component so literal %, # and ? in file names survive the
    // sanitized href and the click handler's one decode, with no file: navigation.
    return marked.Renderer.prototype.link.call(this, path === undefined ? token : { ...token, href: path.split("/").map(encodeURIComponent).join("/") });
  };
  const html = marked.parse(source, { async: false, renderer });
  return DOMPurify.sanitize(html, { FORBID_ATTR: ["style"], FORBID_TAGS: ["style", "form", "input", "button", "textarea", "select"], SANITIZE_NAMED_PROPS: true, USE_PROFILES: { html: true } });
}

export const Markdown = memo(function Markdown({ text, className }: { text: string; className?: string }) {
  const files = useFileNavigation();
  const html = useMemo(() => renderMarkdown(text, files?.cwd), [text, files?.cwd]);
  const { openImage } = useDialogs();
  // Clicking (or Enter on) a sanitized image opens the viewer; the src was already vetted by safeImageUrl in the hook above.
  const open = (target: EventTarget) => {
    const image = target instanceof HTMLImageElement ? target : null;
    const src = safeImageUrl(image?.getAttribute("src"));
    if (image && src) openImage(src, image.getAttribute("alt") ?? "");
  };
  return (
    <div className={className ? `markdown ${className}` : "markdown"} dangerouslySetInnerHTML={{ __html: html }}
      onClick={(event: MouseEvent) => {
        const link = event.target instanceof Element ? event.target.closest<HTMLAnchorElement>("a[href]") : null;
        const path = workspaceFilePath(link?.getAttribute("href"), files?.cwd ?? "");
        if (link && path !== undefined && files) { event.preventDefault(); files.openFile(path, link); }
        else open(event.target);
      }} onKeyDown={(event: KeyboardEvent) => { if (event.key === "Enter" || event.key === " ") { open(event.target); } }} />
  );
});
