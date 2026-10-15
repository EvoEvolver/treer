// WCAG 2.1 contrast check for the prototype tokens. Usage: node contrast.mjs
// Values are read from prototype.html so the table cannot drift from the mock.
import { readFileSync } from "node:fs";
const css = readFileSync(new URL("./prototype.html", import.meta.url), "utf8");
const block = (re) => Object.fromEntries([...css.match(re)[1].matchAll(/--(g-[\w-]+):oklch\(([^)]+)\)/g)].map(([, k, v]) => [k, v.split(/\s+/).map(Number)]));
const light = block(/:root\{\s*(--g-0[\s\S]*?)color-scheme:light/);
const dark = { ...light, ...block(/\[data-theme=dark\]\{([\s\S]*?)color-scheme:dark/) };
function srgb([L, C, h]) {
  const a = C * Math.cos(h * Math.PI / 180), b = C * Math.sin(h * Math.PI / 180);
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3, m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3, s = (L - 0.0894841775 * a - 1.2914855480 * b) ** 3;
  return [4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s, -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s, -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s].map((v) => Math.min(1, Math.max(0, v)));
}
const lum = (c) => { const [r, g, b] = srgb(c); return 0.2126 * r + 0.7152 * g + 0.0722 * b; };
const ratio = (x, y) => { const [a, b] = [lum(x), lum(y)].sort((p, q) => q - p); return (a + 0.05) / (b + 0.05); };
// [foreground, background, required, role]
const text = [["g-fg", "g-fg-soft", "g-fg-muted", "g-amber-text", "g-ok", "g-bad"].flatMap((f) => ["g-0", "g-1", "g-2", "g-3"].map((b) => [f, b, 4.5, "text"]))].flat();
const pairs = [
  ...text,
  ["g-amber-fg", "g-amber", 4.5, "text"],
  ["g-fg", "g-add", 4.5, "diff text"], ["g-fg", "g-del", 4.5, "diff text"],
  ["g-line-ui", "g-0", 3, "UI border"], ["g-line-ui", "g-1", 3, "UI border"], ["g-line-ui", "g-2", 3, "UI border"],
  ["g-amber-text", "g-1", 3, "focus ring"], ["g-amber-text", "g-2", 3, "UI icon"],["g-amber-text", "g-3", 3, "UI icon"],
];
let failed = 0;
for (const [name, tokens] of [["light", light], ["dark", dark]]) for (const [f, b, need, role] of pairs) {
  const r = ratio(tokens[f], tokens[b]), ok = r >= need;
  if (!ok) failed++;
  console.log(`${ok ? "pass" : "FAIL"} ${name.padEnd(5)} ${role.padEnd(9)} ${f} on ${b}: ${r.toFixed(2)} (need ${need})`);
}
console.log(failed ? `${failed} failing pairs` : "all pairs pass");
process.exitCode = failed ? 1 : 0;
