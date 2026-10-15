// Deterministic build of the React UI into four short, flat files: ui.html, boot.js, ui.js, ui.css.
// Shared by build-ui.mjs (writes public/) and check-ui.mjs (rebuilds and compares with the committed files).
import { build, transform } from "esbuild";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const LOOKS = ["a", "b", "c"];

// Scope every rule of one look under :root[data-look=X] so the looks can share one stylesheet.
export function scopeCss(css, look) {
  const src = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const prefix = `:root[data-look=${look}]`;
  let pos = 0;
  const splitTop = (selector) => {
    const parts = [];
    let depth = 0, current = "";
    for (const ch of selector) {
      if (ch === "(" || ch === "[") depth++;
      if (ch === ")" || ch === "]") depth--;
      if (ch === "," && depth === 0) { parts.push(current); current = ""; } else current += ch;
    }
    parts.push(current);
    return parts;
  };
  const scopeSelector = (selector) => splitTop(selector).map((part) => {
    const s = part.trim();
    if (s.startsWith(":root")) return prefix + s.slice(5);
    const lead = /^((?:\[data-[\w-]+=[\w-]+\])+)(.*)$/.exec(s);
    if (lead) return `${prefix}${lead[1]}${lead[2]}`;
    return `${prefix} ${s}`;
  }).join(",");
  function block() {
    let out = "";
    for (;;) {
      while (pos < src.length && /\s/.test(src[pos])) pos++;
      if (pos >= src.length) return out;
      if (src[pos] === "}") { pos++; return out; }
      const open = src.indexOf("{", pos), prelude = src.slice(pos, open).trim();
      pos = open + 1;
      if (prelude.startsWith("@media")) out += `${prelude}{${block()}}`;
      else if (prelude.startsWith("@keyframes")) {
        const start = pos;
        let depth = 1;
        while (depth) { if (src[pos] === "{") depth++; if (src[pos] === "}") depth--; pos++; }
        out += `${prelude}{${src.slice(start, pos - 1)}}`;
      } else {
        const close = src.indexOf("}", pos);
        out += `${scopeSelector(prelude)}{${src.slice(pos, close)}}`;
        pos = close + 1;
      }
    }
  }
  return block();
}

export async function buildUi(base = root) {
  const file = (...parts) => readFileSync(join(base, ...parts), "utf8");
  const js = await build({
    entryPoints: [join(base, "ui/src/main.tsx")], outfile: join(base, "public/ui.js"), write: false, bundle: true,
    format: "iife", platform: "browser", target: "es2022", jsx: "automatic", minify: true, legalComments: "inline", logLevel: "silent",
    define: { "process.env.NODE_ENV": '"production"' }, absWorkingDir: base,
  });
  const css = file("ui/src/styles/shared.css") + "\n" + LOOKS.map((look) => scopeCss(file("ui/src/styles/looks", `${look}.css`), look)).join("\n");
  const boot = await transform(file("ui/boot.js"), { minify: true, target: "es2022", loader: "js" });
  const style = await transform(css, { loader: "css", minify: true });
  return new Map([
    ["ui.html", Buffer.from(file("ui/index.html"))],
    ["boot.js", Buffer.from(boot.code)],
    ["ui.js", Buffer.from(js.outputFiles[0].contents)],
    ["ui.css", Buffer.from(style.code)],
  ]);
}
