import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import test from "node:test";

const source = readFileSync(new URL("../ui/boot.js", import.meta.url), "utf8");
function boot(framed: boolean, search = "") {
  const listeners = new Map<string, (event: unknown) => void>();
  const root = { dataset: {} as Record<string, string> };
  const parent = {};
  const context: Record<string, unknown> = {
    URL, URLSearchParams, document: { documentElement: root }, location: { origin: "https://proxy.test", search },
    localStorage: { getItem: () => null, setItem: () => {} }, parent,
    matchMedia: () => ({ matches: false, addEventListener: () => {} }),
    addEventListener: (type: string, listener: (event: unknown) => void) => listeners.set(type, listener),
  };
  context.window = context; context.top = framed ? parent : context;
  runInNewContext(source, context);
  return { root, listeners, parent };
}
test("standalone boot has defaults and never registers embed handlers", () => {
  const { root, listeners } = boot(false, "?embed_origin=https://console.test");
  assert.equal(root.dataset.theme, "light"); assert.equal(root.dataset.look, undefined);
  assert.equal(listeners.has("message"), false);
});
test("framed boot accepts only the immediate parent at the configured exact origin", () => {
  const { root, listeners, parent } = boot(true, "?embed_origin=https://console.test");
  const message = listeners.get("message")!;
  const data = { type: "treer:embed-theme", theme: "dark", look: "c" };
  message({ source: {}, origin: "https://console.test", data });
  message({ source: parent, origin: "https://foreign.test", data });
  message({ source: parent, origin: "null", data });
  assert.equal(root.dataset.theme, "light");
  message({ source: parent, origin: "https://console.test", data });
  assert.equal(root.dataset.theme, "dark"); assert.equal(root.dataset.look, undefined, "a Console look hint is accepted but ignored");
});
test("invalid configured origins fail closed, omitted origin allows same-origin frames", () => {
  for (const origin of ["", "null", "https://user:pass@console.test", "javascript:alert(1)", "https://console.test/path", "https://*.test"]) {
    assert.equal(boot(true, `?embed_origin=${encodeURIComponent(origin)}`).listeners.has("message"), false);
  }
  const { root, listeners, parent } = boot(true);
  listeners.get("message")!({ source: parent, origin: "https://proxy.test", data: { type: "treer:embed-theme", theme: "dark" } });
  assert.equal(root.dataset.theme, "dark");
});
