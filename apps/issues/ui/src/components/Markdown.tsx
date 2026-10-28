import { Fragment, type ReactNode } from "react";

// A small, safe Markdown subset for issue descriptions and comments. Author text is untrusted: it is only ever turned
// into React elements (text nodes are escaped by React), never into HTML. Supported: paragraphs, # headings, - / 1.
// lists, > quotes, ``` fenced code, `code`, **bold**, *italic*, [text](http(s) URL) links and #N issue references.
// Anything else is shown literally.

const SAFE_PROTOCOLS = new Set(["http:", "https:"]);

function safeHref(raw: string): string | null {
  try {
    const url = new URL(raw);
    return SAFE_PROTOCOLS.has(url.protocol) && !url.username && !url.password ? url.href : null;
  } catch { return null; }
}

const INLINE = /(`[^`\n]+`)|(\*\*[^*\n]+\*\*)|(\*[^*\n]+\*)|(\[[^\]\n]{1,300}\]\([^)\s]{1,2000}\))|((?<![\w&#])#\d{1,9}\b)/g;

function inline(text: string, key = ""): ReactNode[] {
  const out: ReactNode[] = [];
  let last = 0, n = 0;
  for (const match of text.matchAll(INLINE)) {
    const at = match.index ?? 0, token = match[0], k = `${key}.${n++}`;
    if (at > last) out.push(text.slice(last, at));
    if (match[1]) out.push(<code key={k}>{token.slice(1, -1)}</code>);
    else if (match[2]) out.push(<strong key={k}>{inline(token.slice(2, -2), k)}</strong>);
    else if (match[3]) out.push(<em key={k}>{inline(token.slice(1, -1), k)}</em>);
    else if (match[4]) {
      const split = token.indexOf("](");
      const label = token.slice(1, split), href = safeHref(token.slice(split + 2, -1));
      out.push(href ? <a key={k} href={href} target="_blank" rel="noopener noreferrer nofollow">{inline(label, k)}</a> : token);
    } else out.push(<a key={k} href={"#issue=" + token.slice(1)}>{token}</a>);
    last = at + token.length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

function withBreaks(lines: string[], key: string): ReactNode[] {
  return lines.flatMap((line, i) => i ? [<br key={`${key}b${i}`} />, ...inline(line, `${key}l${i}`)] : inline(line, `${key}l${i}`));
}

export function Markdown({ text, className = "markdown" }: { text: string; className?: string }) {
  const lines = text.replace(/\r\n?/g, "\n").split("\n"), count = lines.length;
  const at = (n: number) => lines[n] ?? "";
  const bullet = /^\s*([-*+]|\d{1,9}[.)])\s+/;
  const blocks: ReactNode[] = [];
  let i = 0;
  while (i < count) {
    const line = at(i), key = `k${i}`;
    if (!line.trim()) { i++; continue; }
    const fence = /^\s*(```|~~~)/.exec(line)?.[1];
    if (fence) {
      const body: string[] = [];
      for (i++; i < count && !at(i).trimStart().startsWith(fence); i++) body.push(at(i));
      i++;
      blocks.push(<pre key={key}><code>{body.join("\n")}</code></pre>);
      continue;
    }
    const heading = /^(#{1,6})\s+(.*)$/.exec(line);
    if (heading) {
      const level = Math.min(6, (heading[1] ?? "#").length + 2);    // inside an issue, # is a section, not a page title
      const Tag = `h${level}` as "h3";
      blocks.push(<Tag key={key}>{inline(heading[2] ?? "", key)}</Tag>);
      i++;
      continue;
    }
    if (/^\s*>/.test(line)) {
      const body: string[] = [];
      for (; i < count && /^\s*>/.test(at(i)); i++) body.push(at(i).replace(/^\s*>\s?/, ""));
      blocks.push(<blockquote key={key}>{withBreaks(body, key)}</blockquote>);
      continue;
    }
    if (bullet.test(line)) {
      const ordered = /^\s*\d/.test(line), items: string[][] = [];
      for (; i < count && at(i).trim(); i++) {
        const last = items[items.length - 1];
        if (bullet.test(at(i)) || !last) items.push([at(i).replace(bullet, "")]);
        else last.push(at(i).trim());
      }
      const List = ordered ? "ol" : "ul";
      blocks.push(<List key={key}>{items.map((item, n) => <li key={n}>{withBreaks(item, `${key}i${n}`)}</li>)}</List>);
      continue;
    }
    const body: string[] = [];
    for (; i < count && at(i).trim() && !/^\s*(```|~~~|#{1,6}\s|>)/.test(at(i)) && !(body.length && bullet.test(at(i))); i++) body.push(at(i));
    if (!body.length) { body.push(line); i++; }
    blocks.push(<p key={key}>{withBreaks(body, key)}</p>);
  }
  return <div className={className}>{blocks.map((b, n) => <Fragment key={n}>{b}</Fragment>)}</div>;
}

/** The first line of a text, for a collapsed comment. */
export function firstLine(text: string, limit = 140): string {
  const line = (text.split("\n").find((l) => l.trim()) ?? "").replace(/^\s*(#{1,6}\s+|[-*+>]\s+|```\S*)/, "").trim();
  return line.length > limit ? line.slice(0, limit - 1).trimEnd() + "…" : line;
}
