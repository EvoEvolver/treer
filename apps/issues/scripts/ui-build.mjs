// Deterministic build of the Issues UI into four flat files: index.html, boot.js, app.js, app.css.
// Shared by build-ui.mjs (writes dist/) and check-ui.mjs (rebuilds and compares with the committed files).
import { build, transform } from "esbuild";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");

export async function buildUi(base = root) {
  const file = (...parts) => readFileSync(join(base, ...parts), "utf8");
  const js = await build({
    entryPoints: [join(base, "ui/src/main.tsx")], outfile: join(base, "dist/app.js"), write: false, bundle: true,
    format: "iife", platform: "browser", target: "es2022", jsx: "automatic", minify: true, legalComments: "inline", logLevel: "silent",
    define: { "process.env.NODE_ENV": '"production"' }, absWorkingDir: base,
  });
  const boot = await transform(file("ui/boot.js"), { minify: true, target: "es2022", loader: "js" });
  const style = await transform(file("ui/src/app.css"), { loader: "css", minify: true });
  return new Map([
    ["index.html", Buffer.from(file("ui/index.html"))],
    ["boot.js", Buffer.from(boot.code)],
    ["app.js", Buffer.from(js.outputFiles[0].contents)],
    ["app.css", Buffer.from(style.code)],
  ]);
}
