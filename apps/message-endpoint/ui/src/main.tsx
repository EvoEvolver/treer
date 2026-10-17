import { useEffect, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { readSettings, readLoginUrl, endSession, readEditUrl, saveConfiguration } from './api';
type Lang = 'en' | 'zh';
const words = {
 en: {title:'Message endpoints',subtitle:'Workspace channels and relay status',loading:'Loading settings…',refresh:'Refresh',language:'Language',theme:'Theme',dark:'Dark',light:'Light',signIn:'Continue with Treer',loginHint:'Open the private App to sign in with your workspace owner account.',error:'Unable to load settings',retry:'Try again',empty:'No endpoints configured',emptyHint:'Create a paused binding to an existing endpoint Agent. This preview cannot activate a channel.',preview:'Development preview · channels cannot be activated yet',paused:'Paused',active:'Active',recipient:'Recipient',location:'Expected path',machine:'Machine',reason:'Status reason',revision:'Revision',generation:'Generation',observed:'Current path',unavailable:'Unavailable',diagnostic:'Relay-reported diagnostic; not a Policy or binary verification',worker:'Core workers',pages:'Receive pages',acked:'Acknowledged',failures:'Failures',oldest:'Oldest locally observed unacknowledged item (seconds)',failure:'Last inbox failure',owner:'Signed in as',logout:'Sign out',logoutHint:'Treer’s injected identity remains available while you are signed in to Treer.',details:'Endpoint details',unplaced:'Unplaced or not visible'},
 zh: {title:'消息端点',subtitle:'工作区渠道与中继状态',loading:'正在加载设置…',refresh:'刷新',language:'语言',theme:'主题',dark:'深色',light:'浅色',signIn:'使用 Treer 继续',loginHint:'在独立的私有 App 页面使用工作区 owner 账号登录。',error:'无法加载设置',retry:'重试',empty:'尚未配置端点',emptyHint:'为已创建的端点 Agent 添加暂停的绑定，此预览不能启用渠道。',preview:'开发预览 · 尚不能启用渠道',paused:'已暂停',active:'已启用',recipient:'接收 Agent',location:'预期路径',machine:'设备',reason:'状态原因',revision:'修订号',generation:'代次',observed:'当前路径',unavailable:'不可用',diagnostic:'中继自行报告的信息，不代表 Policy 或二进制验证',worker:'Core 工作循环',pages:'接收页数',acked:'已确认',failures:'失败次数',oldest:'本地最早观察到的未确认消息（秒）',failure:'最近收件失败',owner:'当前用户',logout:'退出 App 会话',logoutHint:'只要仍登录 Treer，入口注入的身份仍然有效。',details:'端点详情',unplaced:'未放置或不可见'},
} as const;
type View = {binding:{endpoint_id:string;generation:number;recipient:string;expected_path:string;expected_server_id:string};revision:number;paused:boolean;reason:string;observed?:{path?:string|null}|null;diagnostic?:{version:string;state:string}|null};
type Data = {owner_id:string; settings:{endpoints:View[];audit_full:boolean;emergency_pause_bindings:number;workers:{receive_pages:number;core_acked:number;failures:number;last_inbox_failure:string|null;oldest_unacked_observed_age_seconds:number|null}}};
function App() {
 const [lang,setLang]=useState<Lang>(navigator.language.startsWith('zh')?'zh':'en');
 const [dark,setDark]=useState(matchMedia('(prefers-color-scheme: dark)').matches);
 const [data,setData]=useState<Data|null>(null), [busy,setBusy]=useState(true),[failure,setFailure]=useState(''),[login,setLogin]=useState(false),[loginUrl,setLoginUrl]=useState<string|null>(null);
 const [editUrl,setEditUrl]=useState<string|null>(null),[saving,setSaving]=useState(false),[saved,setSaved]=useState(false);
 const editable=editUrl===location.origin+'/'&&location.pathname==='/';
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
 useEffect(()=>{void load();void readLoginUrl().then(setLoginUrl).catch(()=>setLoginUrl(null));void readEditUrl().then(setEditUrl).catch(()=>setEditUrl(null));},[]);
 async function logout() {
  setBusy(true);
  try {await endSession();await load();}
  catch(error){setFailure(error instanceof Error?error.message:'logout_rejected');setBusy(false);}
 }
 async function save(command:unknown) {
  if(!editable||saving)return;
  setSaving(true);setSaved(false);setFailure('');
  try {await saveConfiguration(command);setSaved(true);await load();}
  catch(error){setFailure(error instanceof Error?error.message:'configuration_unavailable');}
  finally {setSaving(false);}
 }
 return <main><header><div><p className="eyebrow">Treer</p><h1>{t.title}</h1><p>{t.subtitle}</p></div><div className="controls"><label>{t.language}<select value={lang} onChange={e=>setLang(e.target.value as Lang)}><option value="en">English</option><option value="zh">中文</option></select></label><button type="button" onClick={()=>setDark(!dark)}>{dark?t.light:t.dark}</button><button type="button" disabled={busy} onClick={()=>void load()}>{t.refresh}</button></div></header>
 <p className="preview">{t.preview}</p>
 <div role="status" aria-live="polite">{busy?t.loading:saved?(lang==='zh'?'已保存，端点保持暂停。':'Saved. The endpoint remains paused.'):''}</div>
 {failure&&<section role="alert" className="error"><h2>{t.error}</h2><code>{failure}</code><button disabled={busy} onClick={()=>void load()}>{t.retry}</button></section>}
 {login&&<section><h2>{t.signIn}</h2><p>{t.loginHint}</p>{loginUrl&&<a className="button" href={loginUrl} target="_blank" rel="noopener noreferrer">{t.signIn}</a>}</section>}
 {data&&<><div className="identity"><span>{t.owner}: <code>{data.owner_id}</code></span><button disabled={busy} onClick={()=>void logout()}>{t.logout}</button><small>{t.logoutHint}</small></div>
 {editable?<CreateBinding lang={lang} disabled={saving||busy} save={save}/>:<section><h2>{lang==='zh'?'在独立 App 页面编辑':'Edit in the dedicated App'}</h2><p>{lang==='zh'?'嵌入视图仅供查看，修改需要独立 App 来源的 owner 验证。':'This embedded or direct view is read-only. Changes require owner verification on the dedicated App origin.'}</p>{editUrl&&<a className="button" href={editUrl} target="_blank" rel="noopener noreferrer">{lang==='zh'?'打开设置':'Open settings'}</a>}</section>}
 {data.settings.audit_full&&<section role="alert"><h2>{lang==='zh'?'审计已满':'Audit capacity reached'}</h2><p>{lang==='zh'?'创建、绑定和恢复已锁定；安全暂停仍可用，并以每个绑定一条记录保存最新操作者和计数。':'Create, bind and resume are locked. Safety pause remains available, with a bounded latest-actor record and counter per binding.'}</p><p>{lang==='zh'?'使用紧急暂停记录的绑定数':'Bindings with emergency pause records'}: {data.settings.emergency_pause_bindings}</p></section>}
 <section><h2>{t.worker}</h2><dl className="metrics"><div><dt>{t.pages}</dt><dd>{data.settings.workers.receive_pages}</dd></div><div><dt>{t.acked}</dt><dd>{data.settings.workers.core_acked}</dd></div><div><dt>{t.failures}</dt><dd>{data.settings.workers.failures}</dd></div><div><dt>{t.oldest}</dt><dd>{data.settings.workers.oldest_unacked_observed_age_seconds??'—'}</dd></div></dl>{data.settings.workers.last_inbox_failure&&<p>{t.failure}: <code>{data.settings.workers.last_inbox_failure}</code></p>}</section>
 {data.settings.endpoints.length===0?<section><h2>{t.empty}</h2><p>{t.emptyHint}</p></section>:<div className="endpoints">{data.settings.endpoints.map(v=><section key={v.binding.endpoint_id}><div className="row"><h2>{v.binding.expected_path}</h2><span className="badge">{v.paused?t.paused:t.active}</span></div><code>{v.binding.endpoint_id}</code><p>{t.reason}: <code>{v.reason}</code></p><dl><dt>{t.recipient}</dt><dd><code>{v.binding.recipient}</code></dd><dt>{t.machine}</dt><dd><code>{v.binding.expected_server_id}</code></dd><dt>{t.observed}</dt><dd>{v.observed?.path??t.unplaced}</dd><dt>{t.revision}</dt><dd>{v.revision}</dd><dt>{t.generation}</dt><dd>{v.binding.generation}</dd></dl>{v.diagnostic&&<details><summary>{t.details}</summary><p>{t.diagnostic}</p><code>{v.diagnostic.version} · {v.diagnostic.state}</code></details>}{editable&&<BindingActions lang={lang} view={v} disabled={saving||busy} save={save}/>}</section>)}</div>}</>}
 </main>;
}
type FormProps={lang:Lang;disabled:boolean;save:(command:unknown)=>Promise<void>};
function CreateBinding({lang,disabled,save}:FormProps) {
 const zh=lang==='zh', [confirmed,setConfirmed]=useState(false),[preview,setPreview]=useState<Record<string,unknown>|null>(null),[checking,setChecking]=useState(false),[failure,setFailure]=useState('');
 return <section><h2>{zh?'添加暂停的端点':'Add a paused endpoint'}</h2><p>{zh?'先创建普通 command Agent，再绑定它的稳定 ID。路径与设备由 Core 查询，不会创建或启动 Agent。':'Create an ordinary command Agent first, then bind its stable ID. Core supplies its path and machine. This does not create or start an Agent.'}</p>
 <form onChange={()=>{setPreview(null);setConfirmed(false);}} onSubmit={async e=>{e.preventDefault();const f=new FormData(e.currentTarget);setChecking(true);setFailure('');setConfirmed(false);try{const result=await saveConfiguration({action:'preview_create',endpoint_id:f.get('endpoint_id'),recipient:f.get('recipient'),generation:Number(f.get('generation'))});if(result.status!=='preview'||!result.preview)throw new Error('preview_unavailable');setPreview(result.preview);}catch(error){setFailure(error instanceof Error?error.message:'preview_unavailable');}finally{setChecking(false);}}}>
 <label>{zh?'端点 Agent ID':'Endpoint Agent ID'}<input required name="endpoint_id" placeholder="ag_…" maxLength={128} pattern="ag_[A-Za-z0-9_-]+" disabled={disabled}/></label>
 <label>{zh?'接收 Agent ID':'Recipient Agent ID'}<input required name="recipient" placeholder="ag_…" maxLength={128} pattern="ag_[A-Za-z0-9_-]+" disabled={disabled}/></label>
 <label>{zh?'代次（与私有启动配置一致）':'Generation (match the private launch profile)'}<input required name="generation" type="number" min="1" max="9007199254740991" step="1" defaultValue="1" disabled={disabled}/></label>
 <button disabled={disabled||checking}>{zh?'从 Core 预览绑定':'Preview binding from Core'}</button>
 {failure&&<p role="alert"><code>{failure}</code></p>}
 {preview&&<div className="pin-preview"><h3>{zh?'确认这些当前对象':'Confirm these current objects'}</h3><dl><dt>{zh?'端点':'Endpoint'}</dt><dd>{String(preview.endpoint_id)} · {String(preview.expected_path)}</dd><dt>{zh?'设备':'Machine'}</dt><dd>{String(preview.expected_server_id)}</dd><dt>{zh?'工作目录 SHA-256':'Working directory SHA-256'}</dt><dd><code>{String(preview.expected_cwd_sha256)}</code></dd><dt>{zh?'接收者':'Recipient'}</dt><dd>{String(preview.recipient)} · {preview.recipient_path?String(preview.recipient_path):'—'}</dd><dt>{zh?'代次':'Generation'}</dt><dd>{String(preview.generation)}</dd></dl><label className="confirm"><input type="checkbox" checked={confirmed} onChange={e=>{e.stopPropagation();setConfirmed(e.target.checked);}} disabled={disabled}/>{zh?'确认以上路径、设备与目录摘要。新端点保持暂停。':'Confirm the displayed path, machine and directory hash. The new endpoint stays paused.'}</label>
 <button type="button" disabled={disabled||!confirmed} onClick={()=>{void save({action:'create',endpoint_id:preview.endpoint_id,recipient:preview.recipient,generation:preview.generation,expected:preview,confirm:confirmed});setConfirmed(false);setPreview(null);}}>{zh?'创建暂停绑定':'Create paused binding'}</button></div>}</form></section>;
}
function BindingActions({lang,disabled,save,view}:FormProps&{view:View}) {
 const zh=lang==='zh',[confirmed,setConfirmed]=useState(false),[invalid,setInvalid]=useState(false);
 return <details><summary>{zh?'配置此端点':'Configure endpoint'}</summary><p>{zh?'目标在此代次内不可更换。这里只保存目标，不验证 bot，不启用渠道。':'The target is immutable within this generation. Saving does not verify a bot or activate a channel.'}</p>
 <form onSubmit={e=>{e.preventDefault();const f=new FormData(e.currentTarget),ids=String(f.get('senders')).split(',').map(x=>Number(x.trim())),chat=Number(f.get('chat')),topic=String(f.get('topic')??'').trim();
 if(ids.length<1||ids.length>32||ids.some(n=>!Number.isSafeInteger(n)||n<=0)||new Set(ids).size!==ids.length||!Number.isSafeInteger(chat)||chat===0||(topic!==''&&(!Number.isSafeInteger(Number(topic))||Number(topic)<=0))){setInvalid(true);return;}
 setInvalid(false);void save({action:'bind_telegram',endpoint_id:view.binding.endpoint_id,revision:view.revision,confirm:confirmed,target:{account_id:f.get('account'),chat_id:chat,topic_id:topic===''?null:Number(topic),allowed_senders:ids}});setConfirmed(false);}}>
 <label>{zh?'Telegram 账号配置 ID（不含 token）':'Telegram account configuration ID (no token)'}<input name="account" required maxLength={128} pattern="[A-Za-z0-9_-]+" disabled={disabled}/></label>
 <label>{zh?'Chat ID':'Chat ID'}<input name="chat" required inputMode="numeric" maxLength={20} disabled={disabled}/></label>
 <label>{zh?'Topic ID（可选）':'Topic ID (optional)'}<input name="topic" inputMode="numeric" maxLength={16} disabled={disabled}/></label>
 <label>{zh?'允许的人类 Telegram ID（逗号分隔）':'Allowed human Telegram IDs (comma separated)'}<input name="senders" required maxLength={600} disabled={disabled}/></label>
 {invalid&&<p role="alert">{zh?'请输入有效且不重复的整数 ID。':'Enter valid, distinct integer IDs.'}</p>}
 <label className="confirm"><input type="checkbox" checked={confirmed} onChange={e=>setConfirmed(e.target.checked)} disabled={disabled}/>{zh?'确认这些精确的目标和允许的发送者。':'Confirm the exact target and allowed senders.'}</label>
 <button disabled={disabled||!confirmed||!view.paused}>{zh?'绑定 Telegram 目标':'Bind Telegram target'}</button>
 <button type="button" disabled={disabled||!confirmed} onClick={()=>{void save({action:'pause',endpoint_id:view.binding.endpoint_id,revision:view.revision,confirm:confirmed});setConfirmed(false);}}>{zh?'暂停端点':'Pause endpoint'}</button>
 </form></details>;
}
createRoot(document.getElementById('root')!).render(<App/>);
