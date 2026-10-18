import {useEffect,useState} from 'react';
import {readEditUrl,readEmailThread,submitEmailReply,endMemberSession} from './api';
type Message={source_id:string;body:string;created_at:number;member_id:string|null};
type Thread={id:string;endpoint_id:string;recipient_agent_id:string;messages:Message[]};
export function EmailThread({id}:{id:string}) {
 const [zh,setZh]=useState(navigator.language.startsWith('zh')),[dark,setDark]=useState(matchMedia('(prefers-color-scheme: dark)').matches);
 const [thread,setThread]=useState<Thread|null>(null),[busy,setBusy]=useState(false),[failure,setFailure]=useState(''),[login,setLogin]=useState(false),[root,setRoot]=useState<string|null>(null),[text,setText]=useState(''),[queued,setQueued]=useState(false),[key,setKey]=useState(()=>crypto.randomUUID());
 const editable=root===location.origin+'/'&&location.pathname==='/';
 useEffect(()=>{document.documentElement.lang=zh?'zh':'en';document.documentElement.classList.toggle('dark',dark);},[zh,dark]);
 async function load() {
  setBusy(true);setFailure('');setLogin(false);setThread(null);
  try {const r=await readEmailThread(id);const v=r.value;
   if(r.status===401&&v?.error?.code==='owner_auth_required'){setLogin(true);return;}
   if(!r.ok)throw new Error(r.status===403?'reply_unavailable':'email_thread_unavailable');
   if(v?.thread?.id!==id||!Array.isArray(v?.thread?.messages))throw new Error('email_thread_invalid');
   setThread(v.thread as Thread);
  }catch(e){setFailure(e instanceof Error?e.message:'email_thread_unavailable');}finally{setBusy(false);}
 }
 useEffect(()=>{void load();void readEditUrl().then(setRoot).catch(()=>setRoot(null));},[id]);
 async function reply() {
  if(!editable||!text.trim()||busy)return;
  setBusy(true);setFailure('');setQueued(false);
  try {await submitEmailReply(id,key,text);
   setText('');setKey(crypto.randomUUID());setQueued(true);await load();
  }catch(e){setFailure(e instanceof Error?e.message:'email_reply_unavailable');}finally{setBusy(false);}
 }
 async function logout() {
  setBusy(true);
  try {await endMemberSession();await load();}
  catch(e){setFailure(e instanceof Error?e.message:'logout_rejected');}finally{setBusy(false);}
 }
 const link=root?root+'#email-thread='+id:null;
 return <main><header><div><p className="eyebrow">Treer</p><h1>{zh?'邮件会话':'Email conversation'}</h1></div><div className="controls"><button onClick={()=>setZh(!zh)}>{zh?'English':'中文'}</button><button onClick={()=>setDark(!dark)}>{dark?(zh?'浅色':'Light'):(zh?'深色':'Dark')}</button><button disabled={busy} onClick={()=>void load()}>{zh?'刷新':'Refresh'}</button></div></header>
 <p>{zh?'邮件仅作提醒；在此回复，由端点 Agent 转交给固定接收者。人类身份由 App 声明，并非 Core 人类签名。':'Email is a notification. Replies here are relayed by the endpoint Agent to its fixed counterpart. Human attribution is App-asserted, not a Core human signature.'}</p>
 {failure&&<section role="alert"><p>{failure==='reply_unavailable'?(zh?'当前身份无权访问此会话，或成员/绑定已不可用。':'This identity cannot access the conversation, or its membership/binding is unavailable.'):failure}</p></section>}
 {login&&<section><h2>{zh?'使用 Treer 继续':'Continue with Treer'}</h2><p>{zh?'请使用 owner 绑定到此邮箱的工作区成员身份。链接本身不是访问凭据。':'Use the workspace member identity bound to this address by the owner. The link is not an access credential.'}</p>{root&&(editable?<a className="button" href={root+'api/email/auth/start?thread='+id}>{zh?'使用 Treer 继续':'Continue with Treer'}</a>:<a className="button" href={link!} target="_blank" rel="noopener noreferrer">{zh?'打开私有会话':'Open private conversation'}</a>)}</section>}
 <div role="status" aria-live="polite">{busy?(zh?'正在加载…':'Loading…'):queued?(zh?'回复已入队，尚未确认送达。':'Reply queued; delivery is not yet confirmed.'):''}</div>
 {thread&&<><section><h2>{zh?'会话':'Conversation'}</h2><dl><dt>{zh?'端点':'Endpoint'}</dt><dd><code>{thread.endpoint_id}</code></dd><dt>{zh?'固定接收 Agent':'Fixed Agent counterpart'}</dt><dd><code>{thread.recipient_agent_id}</code></dd></dl>
 {thread.messages.map(m=><article key={m.source_id} className="email-message"><p><strong>{m.member_id?(zh?'成员回复（App 声明）':'Member reply (App-asserted)'):(zh?'Agent 消息':'Agent message')}</strong> · <time dateTime={new Date(m.created_at*1000).toISOString()}>{new Date(m.created_at*1000).toLocaleString()}</time></p>{m.member_id&&<code>{m.member_id}</code>}<pre>{m.body}</pre></article>)}
 </section>{editable?<section><h2>{zh?'回复':'Reply'}</h2><form onSubmit={e=>{e.preventDefault();void reply();}}><label>{zh?'消息':'Message'}<textarea required maxLength={2000} rows={5} value={text} onChange={e=>{setText(e.target.value);setQueued(false);}} disabled={busy}/></label><button disabled={busy||!text.trim()}>{zh?'将回复加入队列':'Queue reply'}</button></form><button disabled={busy} onClick={()=>void logout()}>{zh?'退出 App 会话':'Sign out of App session'}</button><p>{zh?'退出不会结束 Treer 登录，入口注入身份仍可能有效。':'This does not end your Treer login; injected identity may remain available.'}</p></section>:link&&<a className="button" href={link} target="_blank" rel="noopener noreferrer">{zh?'打开私有会话以回复':'Open private conversation to reply'}</a>}</>}
 </main>;
}
