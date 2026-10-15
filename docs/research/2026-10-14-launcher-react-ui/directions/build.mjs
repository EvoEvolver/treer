// Generates the interactive static prototypes. Usage: node build.mjs ABSOLUTE_OUTPUT_DIR
// Writes ui.css, boot.js, app.js, index.html, phone.html, phone.js and the folders a/ b/ c/ (look locked) and all/ (live look switch).
import { mkdirSync, readFileSync, writeFileSync, copyFileSync } from "node:fs";
import { gzipSync } from "node:zlib";
import { isAbsolute, join } from "node:path";
import { fileURLToPath } from "node:url";
const here = fileURLToPath(new URL(".", import.meta.url));
const out = process.argv[2];
if (!isAbsolute(out || "")) throw new Error("Usage: node build.mjs ABSOLUTE_OUTPUT_DIR");
const looks = {
  a: { name: "A · Quiet Editor", intent: "A calm editor-grade workspace: hairline rules, cool neutrals, one ink-blue accent, a left icon rail, dense tabular detail. Gets out of the way during long runs." },
  b: { name: "B · Warm Paper", intent: "A readable, unhurried page: warm grey and white like today's launcher, serif prose, soft rounded forms and the launcher's deep green. Conversation reads like writing; machine activity folds into quiet pills." },
  c: { name: "C · Terminal Contrast", intent: "High-contrast, monospace and square: glyph gutters, [x] plans, an inverse status line for goal / plan / queue, a block cursor. Every state is legible at a glance." },
};

// Scope every rule of one look under :root[data-look=X] so the three looks can live in one file.
function scope(css, look) {
  const src = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const prefix = `:root[data-look=${look}]`;
  let pos = 0;
  const splitTop = (sel) => { const parts = []; let depth = 0, cur = ""; for (const ch of sel) { if (ch === "(" || ch === "[") depth++; if (ch === ")" || ch === "]") depth--; if (ch === "," && depth === 0) { parts.push(cur); cur = ""; } else cur += ch; } parts.push(cur); return parts; };
  const scopeSel = (sel) => splitTop(sel).map((s) => {
    s = s.trim();
    if (s.startsWith(":root")) return prefix + s.slice(5);
    const lead = /^((?:\[data-[\w-]+=[\w-]+\])+)(.*)$/.exec(s);
    if (lead) return `${prefix}${lead[1]}${lead[2]}`;
    return `${prefix} ${s}`;
  }).join(",");
  function block() {
    let res = "";
    for (;;) {
      while (pos < src.length && /\s/.test(src[pos])) pos++;
      if (pos >= src.length) return res;
      if (src[pos] === "}") { pos++; return res; }
      const open = src.indexOf("{", pos), prelude = src.slice(pos, open).trim();
      pos = open + 1;
      if (prelude.startsWith("@media")) res += `${prelude}{${block()}}`;
      else if (prelude.startsWith("@keyframes")) { let depth = 1, start = pos; while (depth) { if (src[pos] === "{") depth++; if (src[pos] === "}") depth--; pos++; } res += `${prelude}{${src.slice(start, pos - 1)}}`; }
      else { const close = src.indexOf("}", pos); res += `${scopeSel(prelude)}{${src.slice(pos, close)}}`; pos = close + 1; }
    }
  }
  return block();
}

mkdirSync(out, { recursive: true });
const css = readFileSync(join(here, "shared.css"), "utf8") + "\n" + Object.keys(looks).map((l) => `/* look ${l} */\n${scope(readFileSync(join(here, `${l}.css`), "utf8"), l)}`).join("\n");
writeFileSync(join(out, "ui.css"), css);
for (const f of ["boot.js", "app.js"]) copyFileSync(join(here, f), join(out, f));

const tpl = readFileSync(join(here, "template.html"), "utf8");
const lookCtl = '<span class="pv-g" role="group" aria-label="Look">Look <button data-look-set="a">A</button><button data-look-set="b">B</button><button data-look-set="c">C</button></span>\n    <span class="pv-g" role="group" aria-label="Borders">Borders <button data-borders-set="soft" aria-pressed="true">Soft</button><button data-borders-set="classic" aria-pressed="false">Classic</button></span>';
const pages = { a: { look: "a", lock: true }, b: { look: "b", lock: true }, c: { look: "c", lock: true }, all: { look: "a", lock: false } };
for (const [d, p] of Object.entries(pages)) {
  mkdirSync(join(out, d), { recursive: true });
  const name = p.lock ? looks[p.look].name : "All looks · live switch";
  writeFileSync(join(out, d, "index.html"), tpl.replaceAll("{{LOOK}}", p.look).replaceAll("{{LOCK}}", p.lock ? " data-look-lock" : "").replaceAll("{{ROOT}}", "../").replaceAll("{{NAME}}", name).replaceAll("{{LOOKCTL}}", p.lock ? "" : lookCtl));
}

const card = (d, v) => `
  <article class="card">
    <h2>${v.name}</h2>
    <p class="intent">${v.intent}</p>
    <div class="shots">
      <a class="shot" href="${d}/index.html?theme=light" aria-label="${v.name}, light, open desktop prototype"><iframe src="${d}/index.html?theme=light&amp;pv=0" title="${v.name} light preview" loading="lazy" tabindex="-1"></iframe></a>
      <a class="shot" href="${d}/index.html?theme=dark" aria-label="${v.name}, dark, open desktop prototype"><iframe src="${d}/index.html?theme=dark&amp;pv=0&amp;pop=plan" title="${v.name} dark preview" loading="lazy" tabindex="-1"></iframe></a>
    </div>
    <p class="links"><a href="${d}/index.html">Open desktop</a><a href="phone.html?d=${d}">Open 390&nbsp;px</a><a href="${d}/index.html?theme=dark">Dark</a><a href="${d}/index.html?view=empty">Empty state</a></p>
  </article>`;
writeFileSync(join(out, "index.html"), `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Agent UI · three looks</title>
<style>
*{box-sizing:border-box}body{margin:0;background:#0f1113;color:#e6e8ea;font:15px/1.55 system-ui,-apple-system,"Segoe UI","PingFang SC","Microsoft YaHei",sans-serif}
main{max-width:1280px;margin:0 auto;padding:40px 24px 64px}h1{margin:0 0 6px;font-size:26px;font-weight:650}.lead{margin:0 0 28px;color:#a4abb2;max-width:72ch}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(340px,1fr));gap:20px}
.card{background:#171a1d;border:1px solid #2b3036;border-radius:14px;padding:20px;display:flex;flex-direction:column;gap:14px}
.card h2{margin:0;font-size:19px}.intent{margin:0;color:#b9c0c7;min-height:6.2em}
.shots{display:grid;grid-template-columns:1fr 1fr;gap:10px}
.shot{position:relative;display:block;aspect-ratio:16/11;overflow:hidden;border:1px solid #2b3036;border-radius:8px;background:#000}
.shot iframe{position:absolute;left:0;top:0;width:1280px;height:880px;border:0;transform:scale(.2);transform-origin:0 0;pointer-events:none}
.links{margin:0;display:flex;flex-wrap:wrap;gap:8px 16px}.links a{color:#9ec1ff}.links a:hover{color:#fff}
a:focus-visible{outline:2px solid #fff;outline-offset:3px}
.hero{display:flex;flex-wrap:wrap;gap:16px 28px;align-items:center;justify-content:space-between;background:#1d2227;border:1px solid #3a4250;border-radius:14px;padding:20px 24px;margin-bottom:20px}
.hero h2{margin:0 0 4px;font-size:19px}.hero p{margin:0;color:#b9c0c7;max-width:62ch}
.go{display:inline-block;background:#e8eaed;color:#101214;border-radius:8px;padding:10px 18px;font-weight:600;text-decoration:none}.go:hover{background:#fff}
.note{margin-top:28px;color:#8a9199;font-size:13px}
</style></head><body><main>
<h1>Agent UI — three looks</h1>
<p class="lead">All three stay: they become user-selectable looks (appearance), each with light and dark. Each prototype is interactive: collapse activity, open Files / Tasks, click Goal · Plan · Queue, focus the input, filter and arrow through the file tree. The Appearance button switches look and theme.</p>
<section class="hero"><div><h2>Compare live: all looks in one page</h2><p>Switch look (A / B / C) and light / dark in the same interface, with state and scroll kept. Try it at 390&nbsp;px too.</p></div><p class="links" style="margin:0"><a class="go" href="all/index.html">Open the live switcher</a><a href="phone.html?d=all">390&nbsp;px</a></p></section>
<div class="grid">${Object.entries(looks).map(([d, v]) => card(d, v)).join("")}</div>
<p class="links" style="margin-top:20px"><a href="borders-compare.html">Compare borders: classic vs soft (A and B)</a></p>
<p class="note">Static mock-ups: no backend, no network, no remote fonts. Look is remembered in localStorage (treer-launcher-look) or set with ?look=a|b|c.</p>
</main></body></html>`);
writeFileSync(join(out, "phone.html"), `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>390 px view</title>
<style>
*{box-sizing:border-box}body{margin:0;background:#0f1113;color:#dfe3e7;font:13px/1.4 system-ui,sans-serif;min-height:100vh;display:flex;flex-direction:column;align-items:center;gap:12px;padding:16px}
.bar{display:flex;flex-wrap:wrap;gap:6px 14px;justify-content:center;max-width:820px}.bar span{display:inline-flex;gap:4px;align-items:center;color:#9aa3ad}
button,a{border:1px solid #3a4047;background:#181b1e;color:#dfe3e7;border-radius:6px;padding:3px 9px;font:inherit;text-decoration:none;cursor:pointer}button:hover,a:hover{background:#262a30}button[aria-pressed=true]{background:#e8eaed;color:#101214}
.dev{width:398px;height:852px;border:4px solid #2b3036;border-radius:34px;overflow:hidden;background:#000}iframe{width:390px;height:844px;border:0;display:block}
</style></head><body>
<div class="bar">
 <span>Page <a href="?d=a">A</a><a href="?d=b">B</a><a href="?d=c">C</a><a href="?d=all">Live switch</a><a href="index.html">Home</a></span>
 <span id="lk">Look <button data-l="a">A</button><button data-l="b">B</button><button data-l="c">C</button></span>
 <span>Theme <button data-t="light">Light</button><button data-t="dark">Dark</button></span>
 <span>View <button data-q="view=chat">Chat</button><button data-q="view=empty">Empty</button><button data-q="view=loading">Loading</button><button data-q="view=error">Error</button></span>
 <span>Open <button data-q="panel=files">Files</button><button data-q="panel=tasks">Tasks</button><button data-q="pop=plan">Plan</button><button data-q="pop=goal">Goal</button><button data-q="pop=queue">Queue</button><button data-q="composer=expanded">Composer</button><button data-q="menu=1">Appearance</button><button data-q="">Reset</button></span>
</div>
<div class="dev"><iframe id="f" title="Prototype at 390 px"></iframe></div>
<script src="phone.js"></script></body></html>`);
writeFileSync(join(out, "borders-compare.html"), `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Borders: classic vs soft</title>
<style>*{box-sizing:border-box}body{margin:0;background:#0f1113;color:#e6e8ea;font:15px/1.5 system-ui,sans-serif}main{max-width:1320px;margin:0 auto;padding:32px 20px 64px}h1{margin:0 0 6px;font-size:24px}p{color:#a4abb2;max-width:80ch}
figure{margin:28px 0 0}figcaption{margin-bottom:8px;font-weight:600}img{display:block;width:100%;height:auto;border:1px solid #2b3036;border-radius:8px}a{color:#9ec1ff}</style></head><body><main>
<h1>Borders: classic (left) vs soft (right)</h1>
<p>Same screen, centre of the window. Soft: decorative dividers and card edges are hairlines; controls are identified by fill, label and the focus ring; popovers and the composer get a faint shadow instead of a dark outline. Classic is the previous outline strength. Try it live: <a href="all/index.html?pv=1">live switcher</a> &rarr; Borders &rarr; Soft / Classic. C keeps its terminal outlines by design.</p>
<figure><figcaption>A · Quiet Editor · light</figcaption><img src="borders-a-light.png" alt="A light, classic left, soft right"></figure>
<figure><figcaption>A · Quiet Editor · dark</figcaption><img src="borders-a-dark.png" alt="A dark, classic left, soft right"></figure>
<figure><figcaption>B · Warm Paper · light</figcaption><img src="borders-b-light.png" alt="B light, classic left, soft right"></figure>
<figure><figcaption>B · Warm Paper · dark</figcaption><img src="borders-b-dark.png" alt="B dark, classic left, soft right"></figure>
</main></body></html>`);
writeFileSync(join(out, "phone.js"), `(() => {
  const params = new URLSearchParams(location.search), d = ["a", "b", "c", "all"].includes(params.get("d")) ? params.get("d") : "a", f = document.getElementById("f");
  let theme = "light", look = d === "all" ? "a" : d;
  if (d !== "all") document.getElementById("lk").hidden = true;
  const load = (q) => { f.src = d + "/index.html?pv=0&theme=" + theme + (d === "all" ? "&look=" + look : "") + (q ? "&" + q : ""); };
  f.addEventListener("load", () => f.contentWindow.postMessage({ type: "treer:embed-theme", theme }, "*"));
  document.addEventListener("click", (e) => {
    const b = e.target.closest("button"); if (!b) return;
    if (b.dataset.t) { theme = b.dataset.t; f.contentWindow.postMessage({ type: "treer:embed-theme", theme }, "*"); document.querySelectorAll("[data-t]").forEach((x) => x.setAttribute("aria-pressed", String(x === b))); }
    else if (b.dataset.l) { look = b.dataset.l; load(""); document.querySelectorAll("[data-l]").forEach((x) => x.setAttribute("aria-pressed", String(x === b))); }
    else if (b.dataset.q !== undefined) load(b.dataset.q);
  });
  document.querySelector("[data-t=light]").setAttribute("aria-pressed", "true"); document.querySelector("[data-l=" + look + "]")?.setAttribute("aria-pressed", "true"); load("");
})();
`);
const size = (f) => readFileSync(join(out, f));
console.log("ui.css", size("ui.css").length, "bytes,", gzipSync(size("ui.css")).length, "gzip; boot.js", size("boot.js").length, "app.js", size("app.js").length);
