import {useEffect,useRef,useState} from 'react';
import {readEditUrl,readEmailThread,submitEmailReply,endMemberSession} from './api';
type Message={source_id:string;body:string;created_at:number;member_id:string|null};
type Thread={id:string;endpoint_id:string;recipient_agent_id:string;messages:Message[]};
function safeFailure(e:unknown){const code=e instanceof Error?e.message:'';return ['email_login_required','email_member_required','email_context_rejected','email_access_unavailable'].includes(code)?code:'email_service_unavailable';}
// getRandomValues also works on HTTP virtual-network origins; randomUUID does not.
function replyKey(){return Array.from(crypto.getRandomValues(new Uint8Array(16)),v=>v.toString(16).padStart(2,'0')).join('');}
export function EmailThread({id}:{id:string}) {
 const [zh,setZh]=useState(navigator.language.startsWith('zh')),[dark,setDark]=useState(matchMedia('(prefers-color-scheme: dark)').matches);
 const [thread,setThread]=useState<Thread|null>(null),[busy,setBusy]=useState(false),[failure,setFailure]=useState(''),[root,setRoot]=useState<string|null>(null),[text,setText]=useState(''),[queued,setQueued]=useState(false),[key,setKey]=useState(()=>replyKey());
 const operation=useRef<{key:string;body:string}|null>(null);const [uncertain,setUncertain]=useState(false);
 const editable=root===location.origin+'/'&&location.pathname==='/';
 useEffect(()=>{document.documentElement.lang=zh?'zh':'en';document.documentElement.classList.toggle('dark',dark);},[zh,dark]);
 async function load() {
  setBusy(true);setFailure('');setThread(null);
  try {const r=await readEmailThread(id);const v=r.value;
   if(!r.ok)throw new Error(r.failure);
   if(v?.thread?.id!==id||!Array.isArray(v?.thread?.messages))throw new Error('email_thread_invalid');
   setThread(v.thread as Thread);
  }catch(e){setFailure(safeFailure(e));}finally{setBusy(false);}
 }
 useEffect(()=>{void load();void readEditUrl().then(setRoot).catch(()=>setRoot(null));},[id]);
 async function reply() {
  if(!editable||!text.trim()||busy)return;
  setBusy(true);setFailure('');setQueued(false);
  const submitted=operation.current??{key,body:text};operation.current=submitted;
  try {await submitEmailReply(id,submitted.key,submitted.body);
   operation.current=null;setUncertain(false);setText('');setKey(replyKey());setQueued(true);await load();
  }catch(e){setUncertain(true);setFailure(safeFailure(e));}finally{setBusy(false);}
 }
 async function logout() {
  setBusy(true);
  try {await endMemberSession();await load();}
  catch(e){setFailure('email_logout_unavailable');}finally{setBusy(false);}
 }
 const link=root?root+'#email-thread='+id:null;
 return <main><header><div><p className="eyebrow">Treer</p><h1>{zh?'邮件会话':'Email conversation'}</h1></div><div className="controls"><button onClick={()=>setZh(!zh)}>{zh?'English':'中文'}</button><button onClick={()=>setDark(!dark)}>{dark?(zh?'浅色':'Light'):(zh?'深色':'Dark')}</button><button disabled={busy} onClick={()=>void load()}>{zh?'刷新':'Refresh'}</button></div></header>
 <nav><a className="button" href="#inbox">{zh?'所有会话':'All conversations'}</a></nav>
 <p>{zh?'邮件仅作提醒；在此回复，由端点 Agent 转交给固定接收者。人类身份由 App 声明，并非 Core 人类签名。':'Email is a notification. Replies here are relayed by the endpoint Agent to its fixed counterpart. Human attribution is App-asserted, not a Core human signature.'}</p>
 {failure&&<section role="alert"><h2>{failure==='email_login_required'?(zh?'请登录 Treer':'Sign in with Treer'):failure==='email_member_required'?(zh?'需要工作区成员身份':'Workspace member identity required'):failure==='email_context_rejected'?(zh?'请求入口被拒绝':'Request context rejected'):failure==='email_access_unavailable'?(zh?'无法访问此会话':'Conversation access unavailable'):(zh?'暂时无法完成请求':'Request unavailable')}</h2><p>{failure==='email_login_required'?(zh?'App 身份缺失或已失效。请通过正常 Treer 登录继续；链接本身不是凭据。':'The App identity is missing or invalid. Continue through normal Treer sign-in; the link is not a credential.'):failure==='email_member_required'?(zh?'此入口需要有效的人类工作区成员身份。请使用与联系人绑定的身份重新认证。':'This route requires a valid human workspace member. Re-authenticate with the identity bound to the contact.'):failure==='email_context_rejected'?(zh?'此请求的入口或浏览器上下文未被接受。请打开专用 App 页面再重试；不会自动重发回复。':'The request origin or browser context was not accepted. Open the dedicated App page and try again; replies are never resent automatically.'):failure==='email_access_unavailable'?(zh?'当前身份无法访问此会话，或会话、成员、绑定不可用。此提示不确认会话是否存在。重新认证不会授予额外权限。':'This identity cannot access the conversation, or the conversation, membership or binding is unavailable. This message does not confirm that a conversation exists. Re-authentication does not grant additional access.'):failure==='email_logout_unavailable'?(zh?'未能确认退出。请刷新检查，不会自动重试。':'Sign-out was not confirmed. Refresh to check; no retry is automatic.'):(zh?'服务或网络暂不可用。读取失败不代表没有消息；请刷新重试。':'The service or network is unavailable. A failed read does not mean there are no messages; refresh to try again.')}</p>
 {root&&failure==='email_login_required'&&<a className="button" href={editable?root+'api/email/auth/start?thread='+id:link!} target={editable?undefined:'_blank'} rel={editable?undefined:'noopener noreferrer'}>{zh?'使用 Treer 继续':'Continue with Treer'}</a>}
 {root&&['email_login_required','email_member_required','email_access_unavailable'].includes(failure)&&<><p>{zh?'请使用 owner 绑定到此联系人的成员身份。此操作重新走 OAuth，但不自动退出 App 或 Treer；如需更换 Treer 账户，请在正常登录页面完成。':'Use the member identity bound to this contact by the owner. This restarts OAuth without automatically signing out of the App or Treer. Change your Treer account through the normal sign-in page if needed.'}</p><a className="button" href={root+'api/email/auth/start?thread='+id+'&reauth=1'} target={editable?undefined:'_blank'} rel={editable?undefined:'noopener noreferrer'}>{zh?'重新认证此会话':'Re-authenticate for this conversation'}</a></>}
 {root&&failure==='email_context_rejected'&&<a className="button" href={link!} target="_blank" rel="noopener noreferrer">{zh?'打开私有会话':'Open private conversation'}</a>}
 </section>}
 <div role="status" aria-live="polite">{busy?(zh?'正在加载…':'Loading…'):queued?(zh?'回复已入队，尚未确认送达。':'Reply queued; delivery is not yet confirmed.'):''}</div>
 {thread&&<><section><h2>{zh?'会话':'Conversation'}</h2><dl><dt>{zh?'端点':'Endpoint'}</dt><dd><code>{thread.endpoint_id}</code></dd><dt>{zh?'固定接收 Agent':'Fixed Agent counterpart'}</dt><dd><code>{thread.recipient_agent_id}</code></dd></dl>
 {thread.messages.map(m=><article key={m.source_id} className="email-message"><p><strong>{m.member_id?(zh?'成员回复（App 声明）':'Member reply (App-asserted)'):(zh?'Agent 消息':'Agent message')}</strong> · <time dateTime={new Date(m.created_at*1000).toISOString()}>{new Date(m.created_at*1000).toLocaleString()}</time></p>{m.member_id&&<code>{m.member_id}</code>}<pre>{m.body}</pre></article>)}
 </section>{editable?<section><h2>{zh?'回复':'Reply'}</h2><form onSubmit={e=>{e.preventDefault();void reply();}}><label>{zh?'消息':'Message'}<textarea readOnly={uncertain} required maxLength={2000} rows={5} value={text} onChange={e=>{setText(e.target.value);setQueued(false);}} disabled={busy}/></label><button disabled={busy||!text.trim()}>{uncertain?(zh?'重试完全相同的回复':'Retry identical reply'):(zh?'将回复加入队列':'Queue reply')}</button>{uncertain&&<p role="status">{zh?'上次回复结果未确认，可能已入队。正文已保留；重试使用相同的正文和请求标识，不会自动发送。':'The previous result is unconfirmed and may already be queued. Your message is preserved; an explicit retry sends the identical body and request key. No retry is automatic.'}</p>}</form><button disabled={busy} onClick={()=>void logout()}>{zh?'退出 App 会话':'Sign out of App session'}</button><p>{zh?'退出不会结束 Treer 登录，入口注入身份仍可能有效。':'This does not end your Treer login; injected identity may remain available.'}</p></section>:link&&<a className="button" href={link} target="_blank" rel="noopener noreferrer">{zh?'打开私有会话以回复':'Open private conversation to reply'}</a>}</>}
 </main>;
}
