'use strict';
const $ = id => document.getElementById(id);
const statuses = ['open','in_progress','blocked','done','closed'];
let people = [], current, offset = 0, listItems = [], activityOffset = null;
function node(tag, text, cls) { const n = document.createElement(tag); if (text !== undefined) n.textContent = text; if (cls) n.className = cls; return n; }
function message(text) { $('notice').textContent = text; }
async function api(path, method = 'GET', body) {
  const response = await fetch(new URL(path, document.baseURI), { method, credentials:'same-origin', headers:{Accept:'application/json', 'Content-Type':'application/json', 'X-Issues-Request':'1'}, ...(body !== undefined ? {body:JSON.stringify(body)} : {}) });
  const result = await response.json();
  if (!response.ok) throw new Error(result.error?.message || 'Request failed');
  return result;
}
function run(fn) { return async () => { try { await fn(); } catch (e) { message(e.message); } }; }
function name(id) { return people.find(p => p.id === id)?.name || id || 'Unassigned'; }
function select(values, selected, nullable = false) {
  const n = node('select');
  if (nullable) n.add(new Option('Unassigned',''));
  for (const v of values) n.add(new Option(typeof v === 'string' ? v.replaceAll('_',' ') : `${v.name} (${v.kind})`, typeof v === 'string' ? v : v.id));
  if (selected && ![...n.options].some(o => o.value === selected)) n.add(new Option(name(selected)+' (unavailable)', selected));
  n.value = selected || ''; return n;
}
function label(text, control) { const n = node('label', text); control.setAttribute('aria-label', text); n.append(control); return n; }
function button(text, fn) { const n = node('button', text); n.type = 'button'; n.onclick = run(fn); return n; }
function linkIssue(id, title) { const n = node('a', title || `#${id}`); n.href = `#issue=${id}`; return n; }
function row(issue) {
  const n = button('', () => { location.hash = `issue=${issue.id}`; }); n.className = 'issue-row';
  n.append(node('strong', `#${issue.id} ${issue.title}`), node('div', `${issue.status.replaceAll('_',' ')} · ${issue.priority} · ${name(issue.assignee)}`, 'meta'));
  for (const text of issue.labels) n.append(node('span', text, 'tag'));
  if (issue.progress.total) n.append(node('div', `${issue.progress.completed}/${issue.progress.total} descendants complete`, 'meta'));
  if (issue.claim) n.append(node('div', `Claimed by ${name(issue.claim.holder)} until ${issue.claim.expires_at}`, 'meta'));
  return n;
}
function renderList() {
  $('list').replaceChildren();
  if (!listItems.length) { $('list').append(node('p','No issues match. Create an issue to begin.', 'empty')); return; }
  if ($('view').value !== 'tree') { $('list').append(...listItems.map(row)); return; }
  const ids = new Set(listItems.map(i => i.id));
  function branch(issue) { const n = node('div'); n.append(row(issue)); const children = listItems.filter(i => i.parent_id === issue.id); if (children.length) { const group = node('div', undefined, 'tree-group'); group.append(...children.map(branch)); n.append(group); } return n; }
  $('list').append(...listItems.filter(i => !ids.has(i.parent_id)).map(branch));
}
async function load(reset = true) {
  const params = new URLSearchParams({limit:'100', offset: reset ? '0' : String(offset)});
  for (const key of ['status','assignee','label']) { const value = $('filter-'+key).value.trim(); if(value) params.set(key, value); }
  const result = await api('v1/issues?'+params);
  listItems = reset ? result.issues : [...listItems, ...result.issues]; offset = result.next_offset;
  $('more').hidden = offset === null; renderList();
}
function notification(result) {
  const n = result.notification;
  message(n?.message ? n.message+' ('+n.status+')' : 'Saved. Notifications: '+(n?.status || 'best effort'));
}
async function mutate(path, method, body) { const result = await api(path,method,body); notification(result); await load(); await show(result.issue.id); }
function activities(target, result) {
  for (const event of result.activity) {
    const item = node('article', undefined, 'activity');
    item.append(node('div', `${name(event.actor)} · ${new Date(event.at).toLocaleString()}`, 'meta'), node('p', event.summary));
    if (event.body) item.append(node('p', event.body, 'comment-body'));
    for (const [key, value] of Object.entries(event.changes || {})) {
      if (key === 'version') continue;
      const titles = {title:'Title', description:'Description', assignee:'Assignee', parent_id:'Parent', blocked_by:'Blocked by', labels:'Labels', priority:'Priority', status:'Status'};
      const display = key === 'description' ? 'updated' : key === 'assignee' ? name(value) : Array.isArray(value) ? value.join(', ') || 'none' : String(value ?? 'none');
      item.append(node('div', `${titles[key] || key}: ${display}`, 'meta description'));
    }
    if (['disabled','unavailable','pending'].includes(event.notification.status)) item.append(node('p', `Notification ${event.notification.status}${event.notification.code ? ': '+event.notification.code : ''}`, 'warning'));
    target.append(item);
  }
}
async function show(id) {
  const result = await api(`v1/issues/${id}`); const issue = result.issue; current = issue;
  const detail = $('detail'); detail.replaceChildren(node('div', `ISSUE #${id}`, 'meta'),node('h2', issue.title));
  detail.append(node('p', `${issue.status.replaceAll('_',' ')} · ${issue.priority} · created by ${name(issue.creator)}`, 'meta'));
  detail.append(node('p', issue.description || 'No description.', 'description'));
  for (const tag of issue.labels) detail.append(node('span', tag, 'tag'));
  if (issue.parent_id) { const p = node('p','Parent: '); p.append(linkIssue(issue.parent_id)); detail.append(p); }
  if (issue.blocked_by.length) { const p = node('p','Blocked by: '); for (const dep of issue.blocked_by) p.append(linkIssue(dep), document.createTextNode(' ')); detail.append(p); }
  const state = select(statuses, issue.status), assignee = select(people,issue.assignee,true);
  const controls = node('div', undefined, 'row'); controls.append(label('Status', state),label('Assignee',assignee)); detail.append(controls);
  const actions = node('div', undefined, 'actions');
  actions.append(button('Save status / assignee', () => mutate(`v1/issues/${id}`,'PATCH',{status:state.value,assignee:assignee.value || null,version:issue.version})), button('Edit issue', () => edit(issue)),button('Add child', () => edit(null,id)));
  const claim = issue.claim;
  actions.append(button(claim ? 'Renew lease' : 'Claim for 15 min', () => mutate(`v1/issues/${id}/claim`,'POST',{action:claim?'renew':'acquire',seconds:900})));
  if (claim) actions.append(button('Release lease', () => mutate(`v1/issues/${id}/claim`,'POST',{action:'release'})));
  detail.append(actions);
  if(claim) detail.append(node('p', `Lease: ${name(claim.holder)} until ${new Date(claim.expires_at).toLocaleString()}`, 'meta'));
  detail.append(node('h3', `Children · ${issue.progress.completed}/${issue.progress.total} descendants complete`));
  const childList = node('div'); childList.append(...result.children.map(row)); detail.append(childList);
  if (result.children_total > result.children.length) { let childOffset = result.children.length; const moreChildren = button('More children', async () => { const page = await api(`v1/issues/${id}/children?limit=100&offset=${childOffset}`); childList.append(...page.issues.map(row)); childOffset = page.next_offset; moreChildren.hidden = childOffset === null; }); detail.append(moreChildren); }
  if(!result.children.length) detail.append(node('p','No child issues.', 'empty'));
  const form = node('form', undefined, 'form'), comment = node('textarea'); comment.required=true; comment.maxLength=8192;
  form.append(label('Comment (text / Markdown source)', comment)); const send = node('button','Post comment'); send.type='submit'; form.append(send);
  form.onsubmit = async e => {e.preventDefault();send.disabled=true;try{await mutate(`v1/issues/${id}/comment`,'POST',{body:comment.value});}catch(e){message(e.message);}finally{send.disabled=false;}};
  detail.append(node('h3','Comments & activity'),form); const timeline = node('div'); activities(timeline,result); detail.append(timeline);
  activityOffset = result.next_activity_offset;
  const more = button('Older activity', async () => {const older = await api(`v1/issues/${id}?offset=${activityOffset}`);activities(timeline,older);activityOffset=older.next_activity_offset;more.hidden=activityOffset===null;}); more.hidden=activityOffset===null;detail.append(more);
}
function edit(issue = null, parent = null) {
  const detail = $('detail'); detail.replaceChildren(node('h2',issue?'Edit issue':'New issue'));
  const form = node('form', undefined, 'form'); const title=node('input'), description=node('textarea'), labels=node('input'), dependencies=node('input'), parentInput=node('input');
  title.required=true;title.maxLength=300;title.value=issue?.title || '';description.value=issue?.description || '';labels.value=issue?.labels.join(', ') || '';dependencies.value=issue?.blocked_by.join(', ') || '';parentInput.type='number';parentInput.min='1';parentInput.value=issue?.parent_id || parent || '';
  const assignee=select(people,issue?.assignee,true), priority=select(['low','normal','high','urgent'],issue?.priority || 'normal');
  form.append(label('Title',title),label('Description (Markdown source, rendered as text)',description),label('Assignee',assignee),label('Priority',priority),label('Labels (comma separated)',labels),label('Parent issue ID',parentInput),label('Blocked by issue IDs (comma separated)',dependencies));
  const save=node('button',issue?'Save issue':'Create issue');save.type='submit';form.append(save,button('Cancel',()=>issue?show(issue.id):detail.replaceChildren(node('p','Select an issue.', 'empty'))));
  form.onsubmit=async e=>{e.preventDefault();save.disabled=true;try{const body={title:title.value,description:description.value,assignee:assignee.value || null,priority:priority.value,labels:labels.value.split(',').map(s=>s.trim()).filter(Boolean),parent_id:parentInput.value?Number(parentInput.value):null,blocked_by:dependencies.value.split(',').map(s=>s.trim()).filter(Boolean).map(Number)};if(issue)body.version=issue.version;const result=await api(issue?`v1/issues/${issue.id}`:'v1/issues',issue?'PATCH':'POST',body);notification(result);await load();location.hash=`issue=${result.issue.id}`;await show(result.issue.id);}catch(e){message(e.message);}finally{save.disabled=false;}};
  detail.append(form);title.focus();detail.scrollIntoView({block:'start',behavior:'smooth'});
}
$('theme').onclick=()=>{const theme=document.documentElement.dataset.theme==='dark'?'light':'dark';document.documentElement.dataset.theme=theme;localStorage.setItem('issues-theme',theme);};
const saved=localStorage.getItem('issues-theme');if(saved)document.documentElement.dataset.theme=saved;
$('new').onclick=()=>edit();$('refresh').onclick=run(()=>load());$('more').onclick=run(()=>load(false));
for(const id of ['filter-status','filter-assignee','filter-label'])$(id).onchange=run(()=>load());$('view').onchange=renderList;
window.addEventListener('hashchange',run(async()=>{const id=new URLSearchParams(location.hash.slice(1)).get('issue');if(id && /^\d+$/.test(id))await show(Number(id));}));
run(async()=>{const me=await api('v1/me');people=(await api('v1/directory')).principals;$('identity').textContent=me.principal.name;$('login').hidden=true;$('tracker').hidden=false;for(const person of people)$('filter-assignee').add(new Option(`${person.name} (${person.kind})`,person.id));message('Workspace collaboration · Core Message notifications are best effort; check activity for delivery status.');await load();const id=new URLSearchParams(location.hash.slice(1)).get('issue');if(id && /^\d+$/.test(id))await show(Number(id));})().catch(()=>{});
