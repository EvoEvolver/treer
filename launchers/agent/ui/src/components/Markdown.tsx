import DOMPurify from "dompurify";
import { marked } from "marked";
import { memo, useMemo } from "react";
import { safeImageUrl } from "../lib/url";

// THE ONLY HTML sink in the UI. Source text is parsed by marked, then sanitized by DOMPurify before it reaches the DOM.
// Styles are forbidden (CSP), links always open safely, and images may only be same-origin, data: raster or blob:.
marked.use({ breaks: true, gfm: true });
DOMPurify.addHook("afterSanitizeAttributes", (node) => {
  if (node.tagName === "A" && node.hasAttribute("href")) {
    node.setAttribute("target", "_blank");
    node.setAttribute("rel", "noopener noreferrer");
  }
  if (node.tagName === "IMG") {
    const safe = safeImageUrl(node.getAttribute("src"));
    if (safe) node.setAttribute("src", safe);
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
  return DOMPurify.sanitize(html, { FORBID_ATTR: ["style"], FORBID_TAGS: ["style"], USE_PROFILES: { html: true } });
}

export const Markdown = memo(function Markdown({ text, className }: { text: string; className?: string }) {
  const html = useMemo(() => renderMarkdown(text), [text]);
  return <div className={className ? `markdown ${className}` : "markdown"} dangerouslySetInnerHTML={{ __html: html }} />;
});
