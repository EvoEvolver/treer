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
  assert.deepEqual([...first.keys()], ["ui.html", "boot.js", "ui.js", "ui.css"]);
  for (const [name, bytes] of first) {
    assert.ok(bytes.equals(second.get(name)!), `${name} must be reproducible`);
    assert.ok(bytes.equals(readFileSync(join(root, "public", name))), `public/${name} is stale; run npm run build:ui`);
  }
  const total = [...first.values()].reduce((sum, bytes) => sum + bytes.length, 0);
  assert.ok(total <= 600_000, `UI payload ${total} bytes exceeds the budget`);
});

test("the built page is relative, script-only from self and free of inline code", async () => {
  const html = (await buildUi()).get("ui.html")!.toString("utf8");
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
    "eval('1'); new Function('1'); document.write('x'); el.innerHTML = 'x'; setTimeout('1', 1);",
    "fetch('/absolute'); fetch('https://example.test'); fetch('api/state');",
  ].join("\n"));
  writeFileSync(join(directory, "components/Markdown.tsx"), "export const m = <div dangerouslySetInnerHTML={{ __html: x }} />; el.innerHTML = 'x';");
  const problems = scanSource(directory).join("\n");
  for (const expected of ["dangerouslySetInnerHTML outside", "href must go through safeUrl", "href literal must be relative", "eval is forbidden", "new Function", "document.write", "innerHTML outside", "string-argument timers", "network paths must be relative"]) assert.match(problems, new RegExp(expected), expected);
  assert.doesNotMatch(problems, /Markdown\.tsx/);
  assert.doesNotMatch(problems, /bad\.tsx:4/);
  assert.equal(scanSource(join(root, "ui/src")).length, 0);
});
