import { useEffect, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { readSettings, readLoginUrl, endSession } from './api';
type Lang = 'en' | 'zh';
const words = {
 en: {title:'Message endpoints',subtitle:'Workspace channels and relay status',loading:'Loading settings…',refresh:'Refresh',language:'Language',theme:'Theme',dark:'Dark',light:'Light',signIn:'Continue with Treer',loginHint:'Open the private App to sign in with your workspace owner account.',error:'Unable to load settings',retry:'Try again',empty:'No endpoints configured',emptyHint:'Endpoint provisioning and transport settings are still being built. No channel can be activated from this preview.',preview:'Development preview · status only',paused:'Paused',active:'Active',recipient:'Recipient',location:'Expected path',machine:'Machine',reason:'Status reason',revision:'Revision',generation:'Generation',observed:'Current path',unavailable:'Unavailable',diagnostic:'Relay-reported diagnostic; not a Policy or binary verification',worker:'Core workers',pages:'Receive pages',acked:'Acknowledged',failures:'Failures',oldest:'Oldest locally observed unacknowledged item (seconds)',failure:'Last inbox failure',owner:'Signed in as',logout:'Sign out',logoutHint:'Treer’s injected identity remains available while you are signed in to Treer.',details:'Endpoint details',unplaced:'Unplaced or not visible'},
 zh: {title:'消息端点',subtitle:'工作区渠道与中继状态',loading:'正在加载设置…',refresh:'刷新',language:'语言',theme:'主题',dark:'深色',light:'浅色',signIn:'使用 Treer 继续',loginHint:'在独立的私有 App 页面使用工作区 owner 账号登录。',error:'无法加载设置',retry:'重试',empty:'尚未配置端点',emptyHint:'端点配置与渠道设置仍在开发中，此预览不能启用渠道。',preview:'开发预览 · 仅显示状态',paused:'已暂停',active:'已启用',recipient:'接收 Agent',location:'预期路径',machine:'设备',reason:'状态原因',revision:'修订号',generation:'代次',observed:'当前路径',unavailable:'不可用',diagnostic:'中继自行报告的信息，不代表 Policy 或二进制验证',worker:'Core 工作循环',pages:'接收页数',acked:'已确认',failures:'失败次数',oldest:'本地最早观察到的未确认消息（秒）',failure:'最近收件失败',owner:'当前用户',logout:'退出 App 会话',logoutHint:'只要仍登录 Treer，入口注入的身份仍然有效。',details:'端点详情',unplaced:'未放置或不可见'},
} as const;
type View = {binding:{endpoint_id:string;generation:number;recipient:string;expected_path:string;expected_cwd:string;expected_server_id:string};revision:number;paused:boolean;reason:string;observed?:{path?:string|null}|null;diagnostic?:{version:string;state:string}|null};
type Data = {owner_id:string; settings:{endpoints:View[];workers:{receive_pages:number;core_acked:number;failures:number;last_inbox_failure:string|null;oldest_unacked_observed_age_seconds:number|null}}};
function App() {
 const [lang,setLang]=useState<Lang>(navigator.language.startsWith('zh')?'zh':'en');
 const [dark,setDark]=useState(matchMedia('(prefers-color-scheme: dark)').matches);
 const [data,setData]=useState<Data|null>(null), [busy,setBusy]=useState(true),[failure,setFailure]=useState(''),[login,setLogin]=useState(false),[loginUrl,setLoginUrl]=useState<string|null>(null);
 const t=words[lang];
 useEffect(()=>{document.documentElement.lang=lang;document.documentElement.classList.toggle('dark',dark)},[lang,dark]);
 async function load() {
  setBusy(true);setFailure('');setLogin(false);
  try {
   const response=await readSettings();
   const value=response.value;
   if(!response.ok) {
    if(response.status===401 && value?.error?.code==='owner_auth_required') {setData(null);setLogin(true);return;}
    throw new Error(typeof value?.error?.code==='string'?value.error.code:'settings_unavailable');
   }
   if(!Array.isArray(value?.settings?.endpoints)||!value?.settings?.workers) throw new Error('settings_invalid');
   setData(value as Data);
  } catch(error) {setFailure(error instanceof Error?error.message:'settings_unavailable');}
  finally {setBusy(false);}
 }
 useEffect(()=>{void load();void readLoginUrl().then(setLoginUrl).catch(()=>setLoginUrl(null));},[]);
 async function logout() {
  setBusy(true);
  try {await endSession();await load();}
  catch(error){setFailure(error instanceof Error?error.message:'logout_rejected');setBusy(false);}
 }
 return <main><header><div><p className="eyebrow">Treer</p><h1>{t.title}</h1><p>{t.subtitle}</p></div><div className="controls"><label>{t.language}<select value={lang} onChange={e=>setLang(e.target.value as Lang)}><option value="en">English</option><option value="zh">中文</option></select></label><button type="button" onClick={()=>setDark(!dark)}>{dark?t.light:t.dark}</button><button type="button" disabled={busy} onClick={()=>void load()}>{t.refresh}</button></div></header>
 <p className="preview">{t.preview}</p>
 <div role="status" aria-live="polite">{busy?t.loading:''}</div>
 {failure&&<section role="alert" className="error"><h2>{t.error}</h2><code>{failure}</code><button disabled={busy} onClick={()=>void load()}>{t.retry}</button></section>}
 {login&&<section><h2>{t.signIn}</h2><p>{t.loginHint}</p>{loginUrl&&<a className="button" href={loginUrl} target="_blank" rel="noopener noreferrer">{t.signIn}</a>}</section>}
 {data&&<><div className="identity"><span>{t.owner}: <code>{data.owner_id}</code></span><button disabled={busy} onClick={()=>void logout()}>{t.logout}</button><small>{t.logoutHint}</small></div>
 <section><h2>{t.worker}</h2><dl className="metrics"><div><dt>{t.pages}</dt><dd>{data.settings.workers.receive_pages}</dd></div><div><dt>{t.acked}</dt><dd>{data.settings.workers.core_acked}</dd></div><div><dt>{t.failures}</dt><dd>{data.settings.workers.failures}</dd></div><div><dt>{t.oldest}</dt><dd>{data.settings.workers.oldest_unacked_observed_age_seconds??'—'}</dd></div></dl>{data.settings.workers.last_inbox_failure&&<p>{t.failure}: <code>{data.settings.workers.last_inbox_failure}</code></p>}</section>
 {data.settings.endpoints.length===0?<section><h2>{t.empty}</h2><p>{t.emptyHint}</p></section>:<div className="endpoints">{data.settings.endpoints.map(v=><section key={v.binding.endpoint_id}><div className="row"><h2>{v.binding.expected_path}</h2><span className="badge">{v.paused?t.paused:t.active}</span></div><code>{v.binding.endpoint_id}</code><p>{t.reason}: <code>{v.reason}</code></p><dl><dt>{t.recipient}</dt><dd><code>{v.binding.recipient}</code></dd><dt>{t.machine}</dt><dd><code>{v.binding.expected_server_id}</code></dd><dt>{t.observed}</dt><dd>{v.observed?.path??t.unplaced}</dd><dt>{t.revision}</dt><dd>{v.revision}</dd><dt>{t.generation}</dt><dd>{v.binding.generation}</dd></dl>{v.diagnostic&&<details><summary>{t.details}</summary><p>{t.diagnostic}</p><code>{v.diagnostic.version} · {v.diagnostic.state}</code></details>}</section>)}</div>}</>}
 </main>;
}
createRoot(document.getElementById('root')!).render(<App/>);
