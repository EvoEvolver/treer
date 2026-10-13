'use strict';
const $ = id => document.getElementById(id);
const statuses = ['open','in_progress','blocked','done','closed'];
let people = [], current, meId, offset = 0, listItems = [], activityOffset = null, activePicker;
function node(tag, text, cls) { const n = document.createElement(tag); if (text !== undefined) n.textContent = text; if (cls) n.className = cls; return n; }
function message(text) { $('notice').textContent = text; }
async function api(path, method = 'GET', body) {
  const response = await fetch(new URL(path, document.baseURI), { method, credentials:'same-origin', headers:{Accept:'application/json', 'Content-Type':'application/json', 'X-Issues-Request':'1'}, ...(body !== undefined ? {body:JSON.stringify(body)} : {}) });
  const result = await response.json();
  if (!response.ok) { const error = new Error(result.error?.message || 'Request failed'); error.status = response.status; error.code = result.error?.code; throw error; }
  return result;
}
function run(fn) { return async () => { try { await fn(); } catch (e) { message(e.message); if (e.status === 401 && e.code === 'authentication_required') $('login').hidden = false; } }; }
function name(id) { return people.find(p => p.id === id)?.name || id || 'Unassigned'; }
function select(values, selected, nullable = false) {
  const n = node('select');
  if (nullable) n.add(new Option('Unassigned',''));
  for (const v of values) n.add(new Option(typeof v === 'string' ? v.replaceAll('_',' ') : `${v.name} (${v.kind})`, typeof v === 'string' ? v : v.id));
  if (selected && ![...n.options].some(o => o.value === selected)) n.add(new Option(name(selected)+' (unavailable)', selected));
  n.value = selected || ''; return n;
}
function label(text, control) { const n = node('label', text); (control.trigger || control).setAttribute('aria-label', text); n.append(control); return n; }
function avatar(person) { const n = node('span', person?.kind === 'agent' ? 'A' : person?.name?.slice(0,2).toUpperCase() || '–', 'avatar ' + (person?.kind || '')); n.setAttribute('aria-hidden','true');return n; }
function assigneePicker(selected, onChange, prompt = 'Unassigned') {
  const wrapper=node('div',undefined,'assignee-picker'),trigger=node('button',undefined,'assignee-trigger');trigger.type='button';wrapper.trigger=trigger;
  let choice=selected || '';
  Object.defineProperty(wrapper,'value',{get:()=>choice});
  const popup=node('div',undefined,'picker-popup');popup.hidden=true;popup.setAttribute('role','dialog');popup.setAttribute('aria-label','Choose assignee');
  const search=node('input');search.type='search';search.placeholder='Search people or Agent paths…';search.setAttribute('aria-label','Search assignees');search.className='picker-search';
  const content=node('div',undefined,'picker-content');popup.append(search,content);wrapper.append(trigger,popup);
  trigger.setAttribute('aria-haspopup','dialog');trigger.setAttribute('aria-expanded','false');
  function drawTrigger() { const person=people.find(p=>p.id===choice);trigger.replaceChildren(avatar(person),node('span',person?.name || (choice ? name(choice)+' (unavailable)' : prompt),'selected-name'),node('span','⌄','chevron')); }
  function close(focus=false) {popup.hidden=true;trigger.setAttribute('aria-expanded','false');if(activePicker?.wrapper===wrapper)activePicker=null;if(focus)trigger.focus();}
  function option(person) {
    const id=person?.id || '',item=node('button',undefined,'picker-option');item.type='button';item.dataset.principalId=id;item.setAttribute('aria-pressed',String(id===choice));
    item.setAttribute('aria-label',person ? `${person.name} · ${person.kind}${person.path?' · '+person.path:''}` : prompt);
    const text=node('span',person?.name || prompt,'person-label');if(person?.path)text.append(node('small',person.path,'person-path'));
    item.append(avatar(person),text);if(id===choice)item.append(node('span','✓'));
    item.onclick=()=>{choice=id;drawTrigger();close(true);onChange?.(id);};return item;
  }
  function drawChoices() {
    content.replaceChildren(option(null));const query=search.value.trim().toLowerCase();
    const visible=people.filter(p=>!query || `${p.name} ${p.path || ''}`.toLowerCase().includes(query));
    const humans=visible.filter(p=>p.kind==='human');if(humans.length){content.append(node('div','PEOPLE','picker-heading'));for(const p of humans)content.append(option(p));}
    const agents=visible.filter(p=>p.kind==='agent');
    if(agents.length) {
      content.append(node('div','AGENT TREE','picker-heading'));
      const root={folders:new Map(),agents:[]},unplaced=[];
      for(const p of agents) {
        if(typeof p.path!=='string' || !p.path.startsWith('/') || p.path==='/' || p.path.includes('//')){unplaced.push(p);continue;}
        const components=p.path.slice(1).split('/');components.pop();let branch=root;
        for(const part of components){if(!branch.folders.has(part))branch.folders.set(part,{folders:new Map(),agents:[]});branch=branch.folders.get(part);}branch.agents.push(p);
      }
      function renderBranch(branch, target, path='') {
        for(const [folder,subtree] of [...branch.folders].sort((a,b)=>a[0].localeCompare(b[0]))) {
          const group=node('details',undefined,'picker-folder');group.open=true;
          const summary=node('summary','▱ '+folder);summary.title=path+'/'+folder;const children=node('div',undefined,'picker-folder-group');
          renderBranch(subtree,children,path+'/'+folder);group.append(summary,children);target.append(group);
        }
        for(const p of branch.agents.sort((a,b)=>a.name.localeCompare(b.name)))target.append(option(p));
      }
      renderBranch(root,content);
      if(unplaced.length){content.append(node('div','PLACEMENT UNAVAILABLE','picker-heading'));for(const p of unplaced)content.append(option(p));}
    }
    if(!visible.length)content.append(node('p','No matching people or Agents','picker-empty'));
  }
  trigger.onclick=()=>{if(!popup.hidden){close();return;}activePicker?.close();activePicker={wrapper,close};search.value='';drawChoices();popup.hidden=false;popup.style.left='0px';const bounds=popup.getBoundingClientRect();popup.style.left=(Math.max(12,Math.min(bounds.left,innerWidth-bounds.width-12))-bounds.left)+'px';trigger.setAttribute('aria-expanded','true');search.focus();};
  search.oninput=drawChoices;
  popup.onkeydown=event=>{
    if(event.key==='Escape'){event.preventDefault();close(true);return;}
    if(event.key==='ArrowDown' || event.key==='ArrowUp') {
      event.preventDefault();const items=[...popup.querySelectorAll('input,summary,button')].filter(e=>e.getClientRects().length);
      const index=items.indexOf(document.activeElement),direction=event.key==='ArrowDown'?1:-1;items[(index+direction+items.length)%items.length]?.focus();
    }
    if(event.key==='Tab'){const items=[...popup.querySelectorAll('input,summary,button')].filter(e=>e.getClientRects().length);if(event.shiftKey && document.activeElement===items[0] || !event.shiftKey && document.activeElement===items.at(-1)){event.preventDefault();close(true);}}
  };
  drawTrigger();return wrapper;
}
document.addEventListener('pointerdown',event=>{if(activePicker && !activePicker.wrapper.contains(event.target))activePicker.close();});
function button(text, fn) { const n = node('button', text); n.type = 'button'; n.onclick = run(async()=>{n.disabled=true;try{await fn();}finally{n.disabled=false;}}); return n; }
function linkIssue(id, title) { const n = node('a', title || `#${id}`); n.href = `#issue=${id}`; return n; }
function row(issue) {
  const n=button('',()=>{location.hash=`issue=${issue.id}`;});n.className='issue-row'+(current?.id===issue.id?' selected':'');
  const status=node('span',undefined,'status-symbol status-'+issue.status);status.title=issue.status.replaceAll('_',' ');
  const title=node('div',undefined,'issue-title');title.append(node('span',issue.title));
  if(issue.labels.length || issue.progress.total || issue.claim){const line=node('div',undefined,'issue-subline');for(const text of issue.labels)line.append(node('span',text,'tag'));if(issue.progress.total)line.append(node('span',`${issue.progress.completed}/${issue.progress.total} complete`));if(issue.claim)line.append(node('span',' · Claimed'));title.append(line);}
  const person=people.find(p=>p.id===issue.assignee),meta=node('span',undefined,'issue-meta');
  const priority=node('span',{low:'▂',normal:'▃',high:'▅',urgent:'!' }[issue.priority],'priority');priority.title=issue.priority+' priority';
  const face=avatar(person);face.title=name(issue.assignee);meta.append(priority,face);
  n.append(status,node('span','ISS-'+issue.id,'issue-id'),title,meta);n.setAttribute('aria-label',`ISS-${issue.id} ${issue.title} · ${issue.status.replaceAll('_',' ')} · ${name(issue.assignee)}`);return n;
}
function renderList() {
  $('list').replaceChildren();
  const query=$('search').value.trim().toLowerCase();const visible=listItems.filter(i=>!query || `${i.title} ISS-${i.id} ${i.labels.join(' ')}`.toLowerCase().includes(query));
  $('issue-count').textContent=listItems.length;$('total').textContent=visible.length;
  if (!visible.length) { $('list').append(node('p','No issues match. Create an issue to begin.', 'empty')); return; }
  if ($('view').value !== 'tree') {
    for(const state of statuses) {const group=visible.filter(i=>i.status===state);if(!group.length)continue;
      const heading=node('div',undefined,'status-group');heading.append(node('span',undefined,'status-symbol status-'+state),node('strong',state.replaceAll('_',' ')),node('span',String(group.length),'count'));$('list').append(heading,...group.map(row));}return;
  }
  const ids = new Set(visible.map(i => i.id));
  function branch(issue) { const n = node('div'); n.append(row(issue)); const children = visible.filter(i => i.parent_id === issue.id); if (children.length) { const group = node('div', undefined, 'tree-group'); group.append(...children.map(branch)); n.append(group); } return n; }
  $('list').append(...visible.filter(i => !ids.has(i.parent_id)).map(branch));
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
async function mutate(path, method, body) { const controls=[...$('detail').querySelectorAll('button,input,textarea,select')];const enabled=controls.filter(n=>!n.disabled);enabled.forEach(n=>n.disabled=true);try{const result=await api(path,method,body);await load();await show(result.issue.id);notification(result);}finally{enabled.forEach(n=>n.disabled=false);} }
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
  const detail = $('detail');const top=node('div',undefined,'detail-topline');top.append(node('span',`ISS-${id}`,'meta'),button('Back to list',()=>{$('collection').scrollIntoView({behavior:'smooth',block:'start'});}));top.lastChild.className='detail-back';detail.replaceChildren(top,node('h2', issue.title));renderList();
  detail.append(node('p', `${issue.status.replaceAll('_',' ')} · ${issue.priority} · created by ${name(issue.creator)}`, 'meta'));
  detail.append(node('p', issue.description || 'No description.', 'description'));
  for (const tag of issue.labels) detail.append(node('span', tag, 'tag'));
  if (issue.parent_id) { const p = node('p','Parent: '); p.append(linkIssue(issue.parent_id)); detail.append(p); }
  if (issue.blocked_by.length) { const p = node('p','Blocked by: '); for (const dep of issue.blocked_by) p.append(linkIssue(dep), document.createTextNode(' ')); detail.append(p); }
  const state = select(statuses, issue.status), assignee = assigneePicker(issue.assignee);
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
  const assignee=assigneePicker(issue?.assignee), priority=select(['low','normal','high','urgent'],issue?.priority || 'normal');
  form.append(label('Title',title),label('Description (Markdown source, rendered as text)',description),label('Assignee',assignee),label('Priority',priority),label('Labels (comma separated)',labels),label('Parent issue ID',parentInput),label('Blocked by issue IDs (comma separated)',dependencies));
  const save=node('button',issue?'Save issue':'Create issue');save.type='submit';form.append(save,button('Cancel',()=>issue?show(issue.id):detail.replaceChildren(node('p','Select an issue.', 'empty'))));
  form.onsubmit=async e=>{e.preventDefault();save.disabled=true;try{const body={title:title.value,description:description.value,assignee:assignee.value || null,priority:priority.value,labels:labels.value.split(',').map(s=>s.trim()).filter(Boolean),parent_id:parentInput.value?Number(parentInput.value):null,blocked_by:dependencies.value.split(',').map(s=>s.trim()).filter(Boolean).map(Number)};if(issue)body.version=issue.version;const result=await api(issue?`v1/issues/${issue.id}`:'v1/issues',issue?'PATCH':'POST',body);notification(result);await load();history.pushState(null,'',`#issue=${result.issue.id}`);await show(result.issue.id);}catch(e){message(e.message);}finally{save.disabled=false;}};
  detail.append(form);title.focus();detail.scrollIntoView({block:'start',behavior:'smooth'});
}
$('theme').onclick=()=>{const theme=document.documentElement.dataset.theme==='dark'?'light':'dark';document.documentElement.dataset.theme=theme;localStorage.setItem('issues-theme',theme);};
const saved=localStorage.getItem('issues-theme');if(saved)document.documentElement.dataset.theme=saved;
$('new').onclick=()=>edit();$('search').oninput=renderList;$('refresh').onclick=run(()=>load());$('more').onclick=run(()=>load(false));
for(const id of ['filter-status','filter-assignee','filter-label'])$(id).onchange=run(()=>load());$('view').onchange=renderList;
window.addEventListener('hashchange',run(async()=>{const id=new URLSearchParams(location.hash.slice(1)).get('issue');if(id && /^\d+$/.test(id))await show(Number(id));}));
run(async()=>{const me=await api('v1/me');meId=me.principal.id;people=(await api('v1/directory')).principals;$('identity').textContent=me.principal.name;$('login').hidden=true;$('tracker').hidden=false;$('new').hidden=false;const filter=assigneePicker('',value=>{$('filter-assignee').value=value;run(()=>load())();},'Anyone');filter.trigger.setAttribute('aria-label','Assignee filter');$('assignee-filter').replaceChildren(filter);message('Workspace collaboration · Core Message notifications are best effort; check activity for delivery status.');await load();const id=new URLSearchParams(location.hash.slice(1)).get('issue');if(id && /^\d+$/.test(id))await show(Number(id));})().catch(()=>{});

function navigation(id, status='',assignee='',view='list') {
  for(const button of document.querySelectorAll('.nav-link'))button.classList.toggle('active',button.id===id);
  $('list-title').textContent={ 'nav-all':'All issues','nav-mine':'Assigned to me','nav-active':'In progress','nav-tree':'Issue hierarchy' }[id];$('filter-status').value=status;$('filter-assignee').value=assignee;$('view').value=view;
  const picker=assigneePicker(assignee,value=>{$('filter-assignee').value=value;run(()=>load())();},'Anyone');picker.trigger.setAttribute('aria-label','Assignee filter');$('assignee-filter').replaceChildren(picker);
  run(()=>load())();
}
$('nav-all').onclick=()=>navigation('nav-all');$('nav-mine').onclick=()=>navigation('nav-mine','',meId);$('nav-active').onclick=()=>navigation('nav-active','in_progress');$('nav-tree').onclick=()=>navigation('nav-tree','','','tree');
document.addEventListener('keydown',event=>{if(event.ctrlKey || event.metaKey || event.altKey || event.target.closest('input,textarea,select,[contenteditable=true]'))return;if(event.key==='c' && !$('tracker').hidden){event.preventDefault();edit();}if(event.key==='/'){event.preventDefault();$('search').focus();}});
