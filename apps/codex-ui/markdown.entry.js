import DOMPurify from "dompurify";
import { marked } from "marked";

marked.use({
  breaks: true,
  gfm: true,
});

export function renderMarkdown(element, source) {
  const html = marked.parse(source ?? "", { async: false });
  element.innerHTML = DOMPurify.sanitize(html, {
    FORBID_ATTR: ["style"],
    FORBID_TAGS: ["style"],
    USE_PROFILES: { html: true },
  });
  for (const link of element.querySelectorAll("a[href]")) {
    link.target = "_blank";
    link.rel = "noreferrer noopener";
  }
}
