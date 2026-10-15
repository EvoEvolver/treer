// WCAG 2.1 contrast check for the three direction token sets. Usage: node contrast.mjs
// Reads the hex tokens from a.css / b.css / c.css (light = :root, dark = [data-theme=dark]).
import { readFileSync } from "node:fs";
const lin = (v) => { v /= 255; return v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; };
const lum = (h) => { const n = parseInt(h.slice(1), 16); return 0.2126 * lin(n >> 16) + 0.7152 * lin((n >> 8) & 255) + 0.0722 * lin(n & 255); };
const ratio = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };
const tokens = (css, re) => Object.fromEntries([...css.match(re)[1].matchAll(/--([\w-]+):(#[0-9a-fA-F]{6})/g)].map(([, k, v]) => [k, v]));
const surfaces = ["bg", "panel", "sunken"];
const pairs = [
  ...["fg", "fg2", "fg3", "accent", "danger", "warn", "ok"].flatMap((f) => surfaces.map((b) => [f, b, 4.5, "text"])),
  ["accent-fg", "accent", 4.5, "text"], ["fg", "accent-soft", 4.5, "text"], ["accent", "accent-soft", 4.5, "text"], ["fg3", "accent-soft", 4.5, "text"],
  ["fg", "add", 4.5, "diff text"], ["fg", "del", 4.5, "diff text"], ["ok", "add", 4.5, "diff gutter"], ["danger", "del", 4.5, "diff gutter"],
  ["edge", "panel", 3, "input edge"], ["edge", "bg", 3, "input edge"],
  ["accent", "panel", 3, "focus ring"], ["accent", "bg", 3, "focus ring"],
];
let failed = 0, total = 0;
for (const d of ["a", "b", "c"]) {
  const css = readFileSync(new URL(`./${d}.css`, import.meta.url), "utf8");
  const light = tokens(css, /:root\{\s*color-scheme:light;([\s\S]*?)\n\}/);
  const dark = { ...light, ...tokens(css, /\[data-theme=dark\]\{([\s\S]*?)\n\}/) };
  const extra = d === "c" ? [["bg", "fg", 4.5, "status line"], ["accent-fg", "accent", 4.5, "status line"]] : [];
  for (const [theme, t] of [["light", light], ["dark", dark]]) for (const [f, b, need, role] of [...pairs, ...extra]) {
    const r = ratio(t[f], t[b]); total++;
    if (r < need) { failed++; console.log(`FAIL ${d} ${theme} ${role} ${f} on ${b}: ${r.toFixed(2)} (need ${need})`); }
  }
}
console.log(failed ? `${failed} of ${total} checks fail` : `all ${total} checks pass`);
process.exitCode = failed ? 1 : 0;
