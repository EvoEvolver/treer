// Deterministic local ESM build: core UI + independently requested rich viewers
// and content-addressed PDF worker. No CDN, modulepreload or worker at startup.
// Shared by build-ui.mjs (writes public/) and check-ui.mjs (rebuilds and compares with the committed files).
import { build, transform } from "esbuild";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
import { createHash } from "node:crypto";

export const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");

export async function buildUi(base = root) {
  const file = (...parts) => readFileSync(join(base, ...parts), "utf8");
  const require = createRequire(join(base, "package.json"));
  const worker = readFileSync(require.resolve("pdfjs-dist/build/pdf.worker.min.mjs"));
  const workerName = `pdf-worker-${createHash("sha256").update(worker).digest("hex").slice(0, 16)}.js`;
  const js = await build({
    entryPoints: { ui: join(base, "ui/src/main.tsx") }, outdir: join(base, "public"), write: false, bundle: true,
    format: "esm", splitting: true, chunkNames: "[name]-[hash]", platform: "browser", target: "es2022", jsx: "automatic", minify: true, legalComments: "inline", logLevel: "silent",
    // Keep dependency identities relative to this launcher, including when a
    // worktree reuses node_modules through a symlink. Real dependency locations
    // otherwise enter esbuild's chunk hashes and change the committed closure.
    preserveSymlinks: true,
    define: { "process.env.NODE_ENV": '"production"' }, absWorkingDir: base,
    plugins: [{ name: "local-pdf-worker", setup(build) {
      build.onLoad({ filter: /pdf\.worker\.min\.mjs$/ }, () => ({ contents: `export default ${JSON.stringify(`./${workerName}`)}`, loader: "js" }));
    } }],
  });
  const css = file("ui/src/styles/app.css");
  const boot = await transform(file("ui/boot.js"), { minify: true, target: "es2022", loader: "js" });
  const style = await transform(css, { loader: "css", minify: true });
  const files = new Map([
    ["index.html", Buffer.from(file("ui/index.html"))],
    ["boot.js", Buffer.from(boot.code)],
    ...js.outputFiles.map(output => [output.path.slice(join(base, "public").length + 1), Buffer.from(output.contents)]),
    ["ui.css", Buffer.from(style.code)],
    [workerName, worker],
  ]);
  files.set("ui-assets.json", Buffer.from(JSON.stringify({ schema: "treer.ui-assets/v1", files: [...files].map(([path, bytes]) => ({ path, bytes: bytes.length, sha256: createHash("sha256").update(bytes).digest("hex") })) }, null, 2) + "\n"));
  return files;
}
