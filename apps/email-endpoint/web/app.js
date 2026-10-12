const here = new URL(location.href);
const match = here.pathname.match(/\/t\/(th_[a-f0-9]{32})\/$/);
const root = new URL(match ? '../../' : './', here);
document.querySelector('#css').href = new URL('style.css', root);
const $ = s => document.querySelector(s);
async function api(path, body, key) {
  const r = await fetch(new URL(path, root), body ? {method:'POST',headers:{'Content-Type':'application/json','Idempotency-Key':key},body:JSON.stringify(body)} : {});
  const v = await r.json();
  if (!r.ok) throw Object.assign(new Error(v.error?.message || 'Request failed'), {status:r.status});
  return v;
}
function showMessage(m, prepend=false) {
  const article=document.createElement('article');
  const meta=document.createElement('p');meta.className='meta';meta.textContent=`${m.sender?.name || m.sender?.id || 'Member'} · ${m.created_at || ''}`;
  const body=document.createElement('p');body.className='message-body';body.textContent=m.body;
  article.append(meta,body);
  prepend ? $('#messages').prepend(article) : $('#messages').append(article);
}
let next, pendingKey;
$('#body').addEventListener('input',()=>{pendingKey=null;});
async function load(older=false) {
  const t=await api('v1/threads/'+match[1]+(older ? '?before='+next : ''));
  $('#title').textContent=t.subject;
  if(!older) $('#messages').replaceChildren();
  (older ? [...t.messages].reverse() : t.messages).forEach(m=>showMessage(m,older));
  next=t.next_before;$('#older').hidden=!next;
  $('#status').textContent='Reply here to reach the initiating Agent.';
  $('#reply').hidden=false;
}
$('#older').onclick=()=>load(true).catch(e=>$('#status').textContent=e.message);
$('#reply').onsubmit=async e=>{
  e.preventDefault();const button=$('#reply button');button.disabled=true;
  pendingKey ||= crypto.randomUUID();
  try {
    const r=await api('v1/threads/'+match[1],{body:$('#body').value},pendingKey);
    $('#result').textContent=r.wake==='prompted'?'Message delivered; Agent prompted.':r.wake==='disabled'?'Message delivered. Agent wake is disabled.':'Message delivered. Agent wake was denied or unavailable.';
    $('#body').value='';pendingKey=null;await load();
  } catch(e) {$('#result').textContent=e.message;} finally {button.disabled=false;}
};
(async()=>{
  try {
    await api('api/auth/session');
    if(match) await load();
    else {
      $('#status').textContent='Open a notification link to read and reply to its conversation.';
      try {
        const a=await api('v1/admin/outbox');$('#admin').hidden=false;
        a.notifications.forEach(n=>{
          const article=document.createElement('article');const link=document.createElement('a');link.href=new URL('t/'+n.thread_id+'/',root);link.textContent=n.subject;
          const summary=document.createElement('p');summary.textContent=n.summary;
          const status=document.createElement('p');status.className='meta';status.textContent=`${n.status} · ${n.transport||a.mode} · attempts ${n.attempts}${n.error?' · '+n.error:''}`;
          article.append(link,summary,status);$('#entries').append(article);
        });
      } catch(e){if(e.status!==403) throw e;}
    }
  } catch(e){
    $('#status').textContent=e.status===401?'Sign in to view your conversation.':e.message;
    if(e.status===401){const login=$('#login');login.hidden=false;login.href=new URL('api/auth/start?return_to='+encodeURIComponent(match?'/t/'+match[1]+'/':'/'),root);}
  }
})();
