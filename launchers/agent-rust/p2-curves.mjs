// Offline numeric evidence plotting; never contacts Agents or reads Codex home.
import {readFile,writeFile} from 'node:fs/promises';
const [input,output]=process.argv.slice(2);
if(!input||!output)throw Error('usage: node p2-curves.mjs evidence.jsonl chart.svg');
const rows=(await readFile(input,'utf8')).trim().split('\n').map(JSON.parse);
const samples=rows.filter(r=>r.type==='sample');
const boundary=samples.find(r=>r.agents.some(a=>a.turnCount>24))?.at;
const clean=samples.filter(r=>!boundary||r.at<boundary);
if(clean.length<2)throw Error('not enough clean samples');
const first=Date.parse(clean[0].at),duration=Date.parse(clean.at(-1).at)-first;
const cpu=s=>s.split(':').reduce((n,v)=>n*60+Number(v),0);
const cpuRole=(a,role)=>a.processes.filter(p=>p.role===role).reduce((n,p)=>n+cpu(p.cpu),0);
const panels=[['Launcher RSS (MiB)','launcher',false],['Codex RSS (MiB)','provider',false],['Railway RSS (MiB)','mcp',false],['Launcher CPU delta (seconds)','launcher',true]];
const svg=['<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="1080" viewBox="0 0 1000 1080"><rect width="1000" height="1080" fill="white"/><g font-family="sans-serif" font-size="13" fill="#111">','<text x="65" y="25" font-size="20">#158 matched samples only — 2026-10-17 UTC</text>','<text x="65" y="48">Node: blue · Rust: orange · independent role scales · intervention suffix excluded</text>'];
panels.forEach(([title,role,isCpu],k)=>{
 const top=90+k*240,w=850,h=170,left=80;
 const series=[0,1].map(i=>clean.map(s=>isCpu?cpuRole(s.agents[i],role)-cpuRole(clean[0].agents[i],role):s.agents[i].rssKiB[role]/1024));
 const max=Math.max(...series.flat())*1.06;
 svg.push(`<text x="${left}" y="${top-13}" font-size="16">${title}</text>`);
 for(let j=0;j<=4;j++){const y=top+h-j*h/4;svg.push(`<path d="M${left},${y}H${left+w}" stroke="#ddd"/><text x="${left-8}" y="${y+4}" text-anchor="end">${(max*j/4).toFixed(1)}</text>`);}
 for(let j=0;j<=4;j++){const x=left+j*w/4;svg.push(`<text x="${x}" y="${top+h+20}" text-anchor="middle">${new Date(first+duration*j/4).toISOString().slice(11,19)}</text>`);}
 series.forEach((values,i)=>{const points=values.map((v,j)=>`${(left+(Date.parse(clean[j].at)-first)/duration*w).toFixed(2)},${(top+h-v/max*h).toFixed(2)}`).join(' ');svg.push(`<polyline points="${points}" fill="none" stroke="${i?'#bb5500':'#1768aa'}" stroke-width="1.5"/>`);});
});
svg.push(`<text x="65" y="1060">${clean.length} samples; ${clean[0].at} to ${clean.at(-1).at}; RSS is process RSS, not private memory.</text></g></svg>`);
await writeFile(output,svg.join('\n'),{flag:'wx',mode:0o600});
console.log(JSON.stringify({samples:clean.length,from:clean[0].at,to:clean.at(-1).at,excludedFrom:boundary,output}));
