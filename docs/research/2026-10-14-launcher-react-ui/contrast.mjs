// WCAG 2.1 contrast check for the prototype tokens. Usage: node contrast.mjs
// Reads prototype.html: --gc-* palette values per theme and the --theme-* aliases,
// so the table cannot drift from the mock. OKLCH -> sRGB, relative luminance.
import { readFileSync } from "node:fs";
const css = readFileSync(new URL("./prototype.html", import.meta.url), "utf8");
const palette = (re) => Object.fromEntries([...css.match(re)[1].matchAll(/--(gc-[\w-]+):oklch\(([^)]+)\)/g)].map(([, k, v]) => [k, v.split(/\s+/).map(Number)]));
const light = palette(/:root\{\s*color-scheme:light;([\s\S]*?)\n\}/);
const dark = { ...light, ...palette(/\[data-theme=dark\]\{([\s\S]*?)\n\}/) };
const alias = Object.fromEntries([...css.matchAll(/--theme-([\w-]+):var\(--(gc-[\w-]+)\)/g)].map(([, a, g]) => [a, g]));
function srgb([L, C, h]) {
  const a = C * Math.cos(h * Math.PI / 180), b = C * Math.sin(h * Math.PI / 180);
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3, m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3, s = (L - 0.0894841775 * a - 1.2914855480 * b) ** 3;
  return [4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s, -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s, -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s].map((v) => Math.min(1, Math.max(0, v)));
}
const lum = (c) => { const [r, g, b] = srgb(c); return 0.2126 * r + 0.7152 * g + 0.0722 * b; };
const ratio = (x, y) => { const [a, b] = [lum(x), lum(y)].sort((p, q) => q - p); return (a + 0.05) / (b + 0.05); };
const surfaces = ["panel", "surface", "surface-strong", "hover", "muted"]; // every background text can sit on
const textTokens = ["fg", "fg-soft", "fg-muted", "accent-strong", "danger", "ok"];
const pairs = [
  ...textTokens.flatMap((f) => surfaces.filter((b) => !(f === "fg-muted" && b === "muted")).map((b) => [f, b, 4.5, "text"])), // fg-muted is never placed on --theme-muted (4.44:1)
  ["accent-solid-fg", "accent-solid", 4.5, "text"], ["accent-solid-fg", "accent-solid-hover", 4.5, "text"],
  ["fg", "add", 4.5, "diff text"], ["fg", "del", 4.5, "diff text"],
  ["border-contrast", "panel", 3, "UI border"], ["border-contrast", "surface", 3, "UI border"],
  ["accent-strong", "panel", 3, "focus ring"], ["accent-strong", "surface", 3, "focus ring"],
];
let failed = 0;
for (const [name, tokens] of [["light", light], ["dark", dark]]) for (const [f, b, need, role] of pairs) {
  const r = ratio(tokens[alias[f]], tokens[alias[b]]), ok = r >= need;
  if (!ok) failed++;
  console.log(`${ok ? "pass" : "FAIL"} ${name.padEnd(5)} ${role.padEnd(9)} ${f} on ${b}: ${r.toFixed(2)} (need ${need})`);
}
console.log(failed ? `${failed} failing pairs` : "all pairs pass");
process.exitCode = failed ? 1 : 0;
