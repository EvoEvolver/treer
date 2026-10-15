// Generates the interactive static prototypes. Usage: node build.mjs ABSOLUTE_OUTPUT_DIR
// Writes only index.html, phone.html and a/ b/ c/ under that directory.
import { cpSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { fileURLToPath } from "node:url";
const here = fileURLToPath(new URL(".", import.meta.url));
const out = process.argv[2];
if (!isAbsolute(out || "")) throw new Error("Usage: node build.mjs ABSOLUTE_OUTPUT_DIR");
const dirs = {
  a: { name: "A · Quiet Editor", mark: "&#9679;", prompt: "", emptyT: "Start a session", emptyP: "Messages, tool activity and file changes will appear here.",
    intent: "A calm editor-grade workspace: hairline rules, cool neutrals, one ink-blue accent, a left icon rail, and dense tabular detail. Gets out of the way during long runs." },
  b: { name: "B · Warm Paper", mark: "A", prompt: "", emptyT: "What shall we work on?", emptyP: "Describe the change you want. I will read, plan and edit, and show my work as I go.",
    intent: "A readable, unhurried page: cream paper and ink, serif prose, soft rounded forms and an oxblood accent. Conversation reads like writing; machine activity folds into quiet pills." },
  c: { name: "C · Terminal Contrast", mark: "&#9612;", prompt: "&#10095;", emptyT: "agent ready", emptyP: "Type a message below. Enter steers the run; Tab queues it.",
    intent: "High-contrast, monospace and square: glyph gutters, [x] plans, an inverse status line for goal / plan / queue, and a block cursor. Every state is legible at a glance." },
};
const tpl = readFileSync(join(here, "template.html"), "utf8");
mkdirSync(out, { recursive: true });
for (const [d, v] of Object.entries(dirs)) {
  const target = join(out, d); mkdirSync(target, { recursive: true });
  const html = tpl.replaceAll("{{DIR}}", d).replaceAll("{{NAME}}", v.name).replaceAll("{{MARK}}", v.mark).replaceAll("{{PROMPT}}", v.prompt).replaceAll("{{EMPTY_T}}", v.emptyT).replaceAll("{{EMPTY_P}}", v.emptyP);
  writeFileSync(join(target, "index.html"), html);
  cpSync(join(here, `${d}.css`), join(target, "style.css"));
  cpSync(join(here, "shared.css"), join(target, "shared.css"));
  cpSync(join(here, "app.js"), join(target, "app.js"));
}
const card = (d, v) => `
  <article class="card">
    <h2>${v.name}</h2>
    <p class="intent">${v.intent}</p>
    <div class="shots">
      <a class="shot" href="${d}/index.html?theme=light" aria-label="${v.name}, light, open desktop prototype"><iframe src="${d}/index.html?theme=light&amp;pv=0&amp;view=chat" title="${v.name} light preview" loading="lazy" tabindex="-1"></iframe></a>
      <a class="shot" href="${d}/index.html?theme=dark" aria-label="${v.name}, dark, open desktop prototype"><iframe src="${d}/index.html?theme=dark&amp;pv=0&amp;pop=plan" title="${v.name} dark preview" loading="lazy" tabindex="-1"></iframe></a>
    </div>
    <p class="links"><a href="${d}/index.html">Open desktop</a><a href="phone.html?d=${d}">Open 390&nbsp;px</a><a href="${d}/index.html?theme=dark">Dark</a><a href="${d}/index.html?view=empty&amp;theme=light">Empty state</a></p>
  </article>`;
writeFileSync(join(out, "index.html"), `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Agent UI · three directions</title>
<style>
*{box-sizing:border-box}body{margin:0;background:#0f1113;color:#e6e8ea;font:15px/1.55 system-ui,-apple-system,"Segoe UI","PingFang SC","Microsoft YaHei",sans-serif}
main{max-width:1280px;margin:0 auto;padding:40px 24px 64px}h1{margin:0 0 6px;font-size:26px;font-weight:650}.lead{margin:0 0 28px;color:#a4abb2;max-width:70ch}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(340px,1fr));gap:20px}
.card{background:#171a1d;border:1px solid #2b3036;border-radius:14px;padding:20px;display:flex;flex-direction:column;gap:14px}
.card h2{margin:0;font-size:19px}.intent{margin:0;color:#b9c0c7;min-height:6.2em}
.shots{display:grid;grid-template-columns:1fr 1fr;gap:10px}
.shot{position:relative;display:block;aspect-ratio:16/11;overflow:hidden;border:1px solid #2b3036;border-radius:8px;background:#000}
.shot iframe{position:absolute;left:0;top:0;width:1280px;height:880px;border:0;transform:scale(.2);transform-origin:0 0;pointer-events:none}
@media(max-width:1500px){.shot iframe{transform:scale(.205)}}
.links{margin:0;display:flex;flex-wrap:wrap;gap:8px 16px}.links a{color:#9ec1ff}.links a:hover{color:#fff}
a:focus-visible{outline:2px solid #fff;outline-offset:3px}
.note{margin-top:28px;color:#8a9199;font-size:13px}
</style></head><body><main>
<h1>Agent UI — three visual directions</h1>
<p class="lead">Each prototype is interactive: collapse activity, open Files / Tasks, click Goal · Plan · Queue in the status strip, focus the input, filter and arrow through the file tree, switch light/dark and empty/loading/error from “Preview controls”. Thumbnails are live. Use “Open 390&nbsp;px” for the phone layout.</p>
<div class="grid">${Object.entries(dirs).map(([d, v]) => card(d, v)).join("")}</div>
<p class="note">Static mock-ups: no backend, no network, no remote fonts. The shipped UI will be React; the chosen direction becomes its token set.</p>
</main></body></html>`);
writeFileSync(join(out, "phone.html"), `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>390 px view</title>
<style>
*{box-sizing:border-box}body{margin:0;background:#0f1113;color:#dfe3e7;font:13px/1.4 system-ui,sans-serif;min-height:100vh;display:flex;flex-direction:column;align-items:center;gap:12px;padding:16px}
.bar{display:flex;flex-wrap:wrap;gap:6px 14px;justify-content:center;max-width:760px}.bar span{display:inline-flex;gap:4px;align-items:center;color:#9aa3ad}
button,a{border:1px solid #3a4047;background:#181b1e;color:#dfe3e7;border-radius:6px;padding:3px 9px;font:inherit;text-decoration:none;cursor:pointer}button:hover,a:hover{background:#262a30}button[aria-pressed=true]{background:#e8eaed;color:#101214}
.dev{width:398px;height:852px;border:4px solid #2b3036;border-radius:34px;overflow:hidden;background:#000}iframe{width:390px;height:844px;border:0;display:block}
</style></head><body>
<div class="bar">
 <span>Direction <a href="?d=a">A</a><a href="?d=b">B</a><a href="?d=c">C</a><a href="index.html">All</a></span>
 <span>Theme <button data-t="light">Light</button><button data-t="dark">Dark</button></span>
 <span>View <button data-q="view=chat">Chat</button><button data-q="view=empty">Empty</button><button data-q="view=loading">Loading</button><button data-q="view=error">Error</button></span>
 <span>Open <button data-q="panel=files">Files</button><button data-q="panel=tasks">Tasks</button><button data-q="pop=plan">Plan</button><button data-q="pop=goal">Goal</button><button data-q="pop=queue">Queue</button><button data-q="composer=expanded">Composer</button><button data-q="">Reset</button></span>
</div>
<div class="dev"><iframe id="f" title="Prototype at 390 px"></iframe></div>
<script src="phone.js"></script></body></html>`);
writeFileSync(join(out, "phone.js"), `(() => {
  const d = new URLSearchParams(location.search).get("d") || "a", f = document.getElementById("f");
  let theme = "light";
  const load = (q) => { f.src = d + "/index.html?pv=0&theme=" + theme + (q ? "&" + q : ""); };
  f.addEventListener("load", () => f.contentWindow.postMessage({ type: "treer:embed-theme", theme }, "*"));
  document.addEventListener("click", (e) => {
    const b = e.target.closest("button"); if (!b) return;
    if (b.dataset.t) { theme = b.dataset.t; f.contentWindow.postMessage({ type: "treer:embed-theme", theme }, "*"); document.querySelectorAll("[data-t]").forEach((x) => x.setAttribute("aria-pressed", String(x === b))); }
    else if (b.dataset.q !== undefined) load(b.dataset.q);
  });
  document.querySelector("[data-t=light]").setAttribute("aria-pressed", "true"); load("");
})();
`);
console.log("built", Object.keys(dirs).join(","), "->", out);
