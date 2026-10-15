import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { scanSource } from "../scripts/check-ui-source.mjs";
import { buildUi, root, scopeCss } from "../scripts/ui-build.mjs";

test("the committed UI artifacts equal a fresh deterministic build", async () => {
  const first = await buildUi();
  const second = await buildUi();
  assert.deepEqual([...first.keys()], ["index.html", "boot.js", "ui.js", "ui.css"]);
  for (const [name, bytes] of first) {
    assert.ok(bytes.equals(second.get(name)!), `${name} must be reproducible`);
    assert.ok(bytes.equals(readFileSync(join(root, "public", name))), `public/${name} is stale; run npm run build:ui`);
  }
  const total = [...first.values()].reduce((sum, bytes) => sum + bytes.length, 0);
  assert.ok(total <= 600_000, `UI payload ${total} bytes exceeds the budget`);
});

test("the built page is relative, script-only from self and free of inline code", async () => {
  const html = (await buildUi()).get("index.html")!.toString("utf8");
  assert.match(html, /<base href="\.\/"/);
  assert.doesNotMatch(html, /<script(?![^>]*\ssrc=)/i, "no inline script");
  assert.doesNotMatch(html, /\sstyle=|<style|\son[a-z]+=/i, "no inline style or handlers");
  assert.doesNotMatch(html, /(?:src|href)="(?:\/|https?:)/, "no absolute or remote resource");
});

test("looks are scoped per look and keep media queries, keyframes and theme selectors", () => {
  const scoped = scopeCss(":root{--a:1}\n[data-theme=dark]{--a:2}\n.x,.y:hover{color:red}\n@media (max-width:640px){.x{color:blue}}\n@keyframes k{from{opacity:0}}", "b");
  assert.equal(scoped, ":root[data-look=b]{--a:1}:root[data-look=b][data-theme=dark]{--a:2}:root[data-look=b] .x,:root[data-look=b] .y:hover{color:red}@media (max-width:640px){:root[data-look=b] .x{color:blue}}@keyframes k{from{opacity:0}}");
});

test("the source scanner rejects HTML sinks, eval and unsafe URLs, and accepts the Markdown sink", () => {
  const directory = mkdtempSync(join(tmpdir(), "ui-scan-"));
  mkdirSync(join(directory, "components"));
  writeFileSync(join(directory, "bad.tsx"), [
    "export const a = <div dangerouslySetInnerHTML={{ __html: x }} />;",
    "export const b = <a href={link}>x</a>;",
    "export const c = <a href=\"javascript:alert(1)\">x</a>;",
    "export const d = <a href={safeUrl(link)}>ok</a>;",
    "const vetted = safeUrl(link); export const e = <a href={vetted}>ok</a>; export const f = <Viewer src={link} />;",
    "eval('1'); new Function('1'); document.write('x'); el.innerHTML = 'x'; setTimeout('1', 1);",
    "fetch('/absolute'); fetch('https://example.test'); fetch('api/state');",
    "el['innerHTML'] = x; const k = { ['dangerouslySetInnerHTML']: 1 }; export const g = <div {...props} />;",
    "a.href = link; img.src = link; frame.srcdoc = '<p>'; a.setAttribute('href', link); i.setAttribute('srcdoc', x); window.open(u); location.href = u; location = u; document.createElement('iframe'); import('x'); navigator.sendBeacon('x');",
    "export const h = <iframe src=\"x\" />; export const j = <a target=\"_blank\" href=\"#x\">x</a>; export const k2 = <form action=\"#\" />; new WebSocket('ws://x'); new XMLHttpRequest();",
  ].join("\n"));
  writeFileSync(join(directory, "components/Markdown.tsx"), "export const m = <div dangerouslySetInnerHTML={{ __html: x }} />; el.innerHTML = 'x';");
  const problems = scanSource(directory).join("\n");
  for (const expected of ["dangerouslySetInnerHTML outside", "href must go through safeUrl", "href literal must be relative", "eval is forbidden", "new Function", "document.write", "innerHTML outside", "string-argument timers", "network paths must be relative"]) assert.match(problems, new RegExp(expected), expected);
  for (const expected of ["\\[\"innerHTML\"\\]", "computed dangerouslySetInnerHTML", "spread props", "\\.href assignment", "\\.src assignment", "\\.srcdoc assignment", "setAttribute\\(\"href\"\\)", "setAttribute\\(\"srcdoc\"\\)", "window\\.open", "assigning location", "createElement\\(\"iframe\"\\)", "dynamic import", "sendBeacon", "<iframe> is forbidden", "requires rel", "form action", "new WebSocket may only", "new XMLHttpRequest may only"]) assert.match(problems, new RegExp(expected), expected);
  assert.doesNotMatch(problems, /Markdown\.tsx/);
  assert.doesNotMatch(problems, /bad\.tsx:4/);
  assert.doesNotMatch(problems, /bad\.tsx:5/, "a variable initialised from safeUrl and a component prop are accepted");
  assert.equal(scanSource(join(root, "ui/src")).length, 0);
});
