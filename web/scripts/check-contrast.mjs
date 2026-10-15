// WCAG 2.1 contrast check for the console's HSL design tokens (light :root and .dark).
// Usage: node scripts/check-contrast.mjs [--table]. Same approach as the launcher's directions/contrast.mjs.
//   text pairs need 4.5:1; non-text (focus ring, control edge, status dot) need 3:1.
import { readFileSync } from "node:fs";

const css = readFileSync(new URL("../src/index.css", import.meta.url), "utf8");
const block = (re) => {
  const body = css.match(re)?.[1];
  if (!body) throw new Error(`token block not found: ${re}`);
  return Object.fromEntries([...body.matchAll(/--([\w-]+):\s*([\d.]+)\s+([\d.]+)%\s+([\d.]+)%;/g)].map(([, k, h, s, l]) => [k, [+h, +s / 100, +l / 100]]));
};
const light = block(/:root\s*\{([\s\S]*?)\n  \}/);
const dark = { ...light, ...block(/\.dark\s*\{([\s\S]*?)\n  \}/) };

const rgb = ([h, s, l]) => {
  const a = s * Math.min(l, 1 - l);
  const f = (n) => { const k = (n + h / 30) % 12; return l - a * Math.max(-1, Math.min(k - 3, 9 - k, 1)); };
  return [f(0), f(8), f(4)];
};
const lin = (v) => (v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4);
const lum = (hsl) => { const [r, g, b] = rgb(hsl).map(lin); return 0.2126 * r + 0.7152 * g + 0.0722 * b; };
const ratio = (a, b) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };

const text = [
  ["foreground", "background"], ["card-foreground", "card"], ["popover-foreground", "popover"],
  ["primary-foreground", "primary"], ["secondary-foreground", "secondary"], ["accent-foreground", "accent"],
  ["destructive-foreground", "destructive"], ["sidebar-foreground", "sidebar"],
  ["muted-foreground", "background"], ["muted-foreground", "card"], ["muted-foreground", "muted"],
  ["muted-foreground", "sidebar"], ["muted-foreground", "accent"],
  ["primary", "background"], ["destructive", "background"],
  // Small overview status text needs the full text threshold, including row hover.
  ["status-working-text", "background"], ["status-working-text", "accent"],
  ["status-attention-text", "background"], ["status-attention-text", "accent"],
].map(([f, b]) => [f, b, 4.5, "text"]);
const nonText = [["ring", "background"], ["ring", "card"], ["status-online", "background"], ["status-online", "sidebar"]]
  .map(([f, b]) => [f, b, 3, "non-text"]);
// Known, documented limits (dark theme): the brand blue and the destructive red are also button fills that carry
// white text, so as plain text on the dark page they only reach the 3:1 large-text/icon bar. Input borders stay a
// soft hairline (floor 1.4:1) because every field also has a label and a fill; see docs/quality.md.
const known = { dark: { "primary|background": 3, "destructive|background": 3 } };
const floors = [["input", "background", 1.4, "soft control edge floor"]];

let failed = 0, total = 0;
for (const [name, t] of [["light", light], ["dark", dark]]) {
  for (const [f, b, base, role] of [...text, ...nonText, ...floors]) {
    const need = known[name]?.[`${f}|${b}`] ?? base;
    const r = ratio(t[f], t[b]); total++;
    if (process.argv.includes("--table")) console.log(name, role, f, "on", b, r.toFixed(2));
    if (r < need) { failed++; console.log(`FAIL ${name} ${role} ${f} on ${b}: ${r.toFixed(2)} (need ${need})`); }
  }
}
console.log(failed ? `${failed} of ${total} checks fail` : `all ${total} checks pass`);
process.exitCode = failed ? 1 : 0;
