import { build, transform } from 'esbuild';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
export const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
export async function buildUi() {
  const js = await build({entryPoints:[join(root,'ui/src/main.tsx')],outfile:join(root,'dist/app.js'),write:false,bundle:true,format:'iife',platform:'browser',target:'es2022',jsx:'automatic',minify:true,legalComments:'inline',logLevel:'silent',define:{'process.env.NODE_ENV':'"production"'},absWorkingDir:root});
  const css = await transform(readFileSync(join(root,'ui/src/app.css'),'utf8'),{loader:'css',minify:true});
  return new Map([['index.html',readFileSync(join(root,'ui/index.html'))],['app.js',Buffer.from(js.outputFiles[0].contents)],['app.css',Buffer.from(css.code)]]);
}
