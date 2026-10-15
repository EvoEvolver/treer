import {build} from 'esbuild';
import {readFile,writeFile} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
const root=fileURLToPath(new URL('../',import.meta.url));
const result=await build({entryPoints:[root+'ui/app.jsx'],outfile:root+'web/app.js',write:false,bundle:true,format:'iife',platform:'browser',target:'es2022',jsx:'automatic',minify:true,legalComments:'inline',define:{'process.env.NODE_ENV':'"production"'},absWorkingDir:root});
const files=new Map([['app.js',result.outputFiles[0].contents],['index.html',await readFile(root+'ui/index.html')],['style.css',await readFile(root+'ui/style.css')]]);
for(const [name,bytes] of files){
  if(process.argv.includes('--check')){
    if(!Buffer.from(await readFile(root+'web/'+name)).equals(Buffer.from(bytes))) throw new Error('Stale frontend: '+name);
  } else await writeFile(root+'web/'+name,bytes);
}
console.log('Email UI '+(process.argv.includes('--check')?'verified':'built'));
