'use strict';
const words = {
 en: {intro:'Tokens act as this App’s Agent under its existing Policy. They never represent your human account.',name:'Token name',days:'Expires after',authority:'I understand every token shares this App’s authority. Read-only automation needs a separate App with read-only enforcing Policy.',create:'Create token',createTitle:'Create a token',secret:'Shown once. Save in private credential storage; never paste in a URL or Agent prompt.',dismiss:'Clear secret',tokens:'Tokens',activation:'External API',reviewed:'I confirm this App may execute under its current workspace Policy.',enable:'Enable after confirmation',disable:'Disable external API',enabled:'Enabled',disabled:'Disabled',unavailable:'Activation unavailable: reviewed Policy acceptance evidence is not configured.',loading:'Loading…',auth:'Owner verification required. Open this App through the authenticated Treer Console.',error:'Request failed. No operation was retried.',created:'Token created. Save it before leaving this page.',revoked:'Revoked',revoke:'Revoke',confirm:'Revoke this token? New requests will be refused; already dispatched operations may finish.',empty:'No tokens. Create one after reviewing the App’s Policy.',last:'Last used',never:'Never',expires:'Expires',source:'Transport peer (not verified client IP)',ok:'Saved',auditTitle:'Execution audit',auditNote:'Latest 100 body-free records. Token identifiers are masked. Unknown outcomes do not prove completion; do not retry an operation from this view.',auditRefresh:'Refresh audit',auditEmpty:'No execution records.',auditFailed:'Audit unavailable. No operation was retried.',auditDenied:'Owner access denied. Previous audit records cleared.',auditLoading:'Loading audit…'},
 zh: {intro:'Token 以此 App 的 Agent 身份执行，受其现有 Policy 约束，不代表您的人类账号。',name:'Token 名称',days:'有效期',authority:'我理解所有 token 共享此 App 的权限。只读自动化需使用独立 App，并配置强制执行的只读 Policy。',create:'创建 token',createTitle:'创建 token',secret:'仅显示一次。请保存到私有凭据存储，不要粘贴到 URL 或 Agent prompt。',dismiss:'清除秘密',tokens:'Token 列表',activation:'外部 API',reviewed:'我确认此 App 可按其当前工作区 Policy 执行。',enable:'确认后启用',disable:'关闭外部 API',enabled:'已启用',disabled:'已关闭',unavailable:'无法启用：尚未配置审核后的 Policy 验收记录。',loading:'加载中…',auth:'需验证工作区 owner 身份。请通过已登录的 Treer Console 打开此 App。',error:'请求失败；没有自动重试任何操作。',created:'Token 已创建，请在离开页面前保存。',revoked:'已撤销',revoke:'撤销',confirm:'撤销此 token？后续请求将被拒绝，已派发的操作仍可能完成。',empty:'暂无 token，请在检查 App 的 Policy 后创建。',last:'最后使用',never:'从未',expires:'到期',source:'连接来源（非认证客户端 IP）',ok:'已保存',auditTitle:'执行审计',auditNote:'最近 100 条无正文记录。Token 标识已遮罩。结果未知不代表执行完成；此页面不重试操作。',auditRefresh:'刷新审计',auditEmpty:'暂无执行记录。',auditFailed:'审计暂不可用；没有重试任何操作。',auditDenied:'Owner 访问被拒绝，旧审计记录已清除。',auditLoading:'正在加载审计…'}
};
Object.assign(words.en, {
 connectionTitle:'Connect your Agent', connectionNote:'Copy one set of instructions for your Agent. The skill contains no token and grants no permissions.', connectionRefresh:'Refresh connection status', connectionLoading:'Checking connection configuration…', connectionBlocked:'Not ready for external access', connectionUnverified:'Configured — external reachability has not been verified', connectionDenied:'Owner verification required. Connection instructions cleared.', connectionFailed:'Connection status unavailable. Previous instructions cleared; refresh to try again.', connectionBase:'Data API base URL', connectionCopy:'Copy instructions for my Agent', connectionDownload:'Download SKILL.md', connectionPreview:'Read the instructions', connectionPreviewDescription:'Token-free instructions; you can also select and copy this text.', connectionPolicy:'Every request uses this App’s current Policy. A token is not your human account and does not grant additional authority.', connectionToken:'Provide the token separately through your Agent’s secret environment input or a private file. It is not included in copied instructions or the skill.', connectionCopied:'Instructions copied. Supply the token separately through private credential storage.', connectionFallback:'Automatic copy is unavailable. The instructions are selected below; press Ctrl/Cmd+C. They contain no token.', connectionSetup:'Connection snapshot: not verified ready.', connectionUnknown:'The server reported an additional readiness requirement.', connectionLoopback:'The data API is loopback-only; no external data endpoint is available.', connectionDisabled:'The external API is disabled.', connectionAcceptance:'Reviewed Policy acceptance evidence is not configured.', connectionReachability:'The configured HTTPS data endpoint has not been verified reachable.', connectionPrompt:'Prompt delivery is not configured; do not assume prompt is available.', connectionNoToken:'Create an unexpired token after reviewing Policy; deliver it separately, never in this skill.', connectionMissing:'Resolve these requirements before connecting:'
});
Object.assign(words.zh, {
 connectionTitle:'接入你的 Agent', connectionNote:'复制一段完整指令交给 Agent。Skill 不含 token，也不会授予额外权限。', connectionRefresh:'刷新接入状态', connectionLoading:'正在检查接入配置…', connectionBlocked:'尚未就绪，无法从外部接入', connectionUnverified:'已配置地址，但尚未验证外部可达性', connectionDenied:'需要验证 owner 身份，已清除接入指令。', connectionFailed:'暂时无法获取接入状态，旧指令已清除；可手动刷新。', connectionBase:'数据 API 基础地址', connectionCopy:'复制给我的 Agent', connectionDownload:'下载 SKILL.md', connectionPreview:'阅读接入指令', connectionPreviewDescription:'指令不含 token，也可以直接选中文本复制。', connectionPolicy:'每个请求都受此 App 当前 Policy 约束。Token 不代表你的人类账号，也不会增加权限。', connectionToken:'通过 Agent 的秘密环境变量输入或私有文件单独提供 token。复制内容和 skill 均不包含 token。', connectionCopied:'指令已复制。请通过私有凭据存储单独提供 token。', connectionFallback:'无法自动复制，已在下方选中指令；请按 Ctrl/Cmd+C。文本不含 token。', connectionSetup:'连接状态快照：尚未验证就绪。', connectionUnknown:'服务端报告了其他接入条件。', connectionLoopback:'数据 API 仅在本机回环地址监听，尚无外部数据入口。', connectionDisabled:'外部 API 尚未启用。', connectionAcceptance:'尚未配置审核后的 Policy 验收证据。', connectionReachability:'尚未验证已配置 HTTPS 数据入口的外部可达性。', connectionPrompt:'尚未配置 prompt 投递，不可假定该操作可用。', connectionNoToken:'检查 Policy 后创建未过期 token；请单独传递，不要写入此 skill。', connectionMissing:'接入前需要解决以下条件：'
});
Object.assign(words.en, {externalTitle:'External Agents',externalNote:'Register a stable address for an external Agent. Its token keeps the App’s current workspace CLI/API authority and accesses only its own inbox. It is not an inbox-only or read-only token.',externalRefresh:'Refresh registered Agents',externalLoading:'Loading registered Agents…',externalEmpty:'No external Agents registered. Create a stable key and display name below.',externalDenied:'Owner access denied. Previous registry records cleared.',externalFailed:'Registry unavailable. Previous records cleared; no operation was retried.',externalKey:'Stable key',externalKeyNote:'1–64 ASCII letters, digits, _ or -. Start with a letter or digit. The key cannot be renamed.',externalName:'Display name',externalDays:'Token expiry',externalAuthority:'I confirm this token may use the App’s current workspace Policy and CLI/API authority, plus this Agent’s own inbox.',externalCreate:'Register Agent and create token',externalCreated:'Agent registered. Save its token now; it will not be shown again.',externalAmbiguous:'Creation was not confirmed and was not retried. Refresh the registry and token list before taking another action. If the token was lost, revoke it; registry reads cannot recover the secret.',externalRecovered:'The Agent is registered, but its token cannot be recovered. Revoke the lost token in the token list before using a supported replacement-token flow.',externalRejected:'Creation was refused. Check the key, display name and capacity, then refresh the registry. No retry was sent.',externalInvalid:'Use a valid stable key and a nonempty display name of at most 128 UTF-8 bytes, and confirm authority.',externalAddress:'Stable address',externalCopy:'Copy address',externalCopied:'Address copied.',externalCopyFallback:'Address selected; press Ctrl/Cmd+C to copy.',externalInboxEnabled:'Inbox enabled',externalInboxDisabled:'Inbox disabled',externalCreatedAt:'Registered',externalSecret:'Agent token, shown once. It retains the App’s workspace CLI/API authority and accesses only this Agent’s inbox. Save privately; never paste into a URL or prompt.'});
Object.assign(words.zh, {externalTitle:'外部 Agent',externalNote:'为外部 Agent 注册稳定地址。其 token 保留 App 当前工作区的 CLI/API 权限，并只能访问自己的收件箱；并非仅限收件箱或只读的 token。',externalRefresh:'刷新已注册 Agent',externalLoading:'正在加载已注册 Agent…',externalEmpty:'尚未注册外部 Agent。请在下方填写稳定 key 和显示名称。',externalDenied:'Owner 访问被拒绝，旧注册记录已清除。',externalFailed:'暂时无法读取注册表，旧记录已清除；没有重试操作。',externalKey:'稳定 key',externalKeyNote:'1–64 个 ASCII 字母、数字、_ 或 -，以字母或数字开头。创建后不可改名。',externalName:'显示名称',externalDays:'Token 有效期',externalAuthority:'我确认此 token 可使用 App 当前工作区 Policy 下的 CLI/API 权限，并访问此 Agent 自己的收件箱。',externalCreate:'注册 Agent 并创建 token',externalCreated:'Agent 已注册，请立即保存 token；之后不会再次显示。',externalAmbiguous:'未能确认创建结果，没有重试。请先刷新注册表和 token 列表，再决定下一步；若 token 丢失，应撤销它，读取注册表无法恢复秘密。',externalRecovered:'已找到注册的 Agent，但无法恢复其 token。请先在 token 列表撤销丢失的 token，再使用受支持的凭据更换流程。',externalRejected:'创建被拒绝。请检查 key、显示名称和容量，并刷新注册表；没有发起重试。',externalInvalid:'请填写合法稳定 key、不超过 128 个 UTF-8 字节的非空显示名称，并确认权限。',externalAddress:'稳定地址',externalCopy:'复制地址',externalCopied:'地址已复制。',externalCopyFallback:'地址已选中，请按 Ctrl/Cmd+C 复制。',externalInboxEnabled:'收件箱已启用',externalInboxDisabled:'收件箱已关闭',externalCreatedAt:'注册时间',externalSecret:'Agent token，仅显示一次。保留 App 的工作区 CLI/API 权限，且只能访问此 Agent 的收件箱。请私密保存，不要粘贴到 URL 或 prompt。'});
let language=navigator.language.startsWith('zh')?'zh':'en',state=null;
const el=id=>document.getElementById(id),t=k=>words[language][k];
function feedback(s){el('feedback').textContent=s;}
function render(){document.documentElement.lang=language;el('language').value=language;
 for(const [id,key] of Object.entries({'intro':'intro','name-label':'name','days-label':'days','authority-label':'authority','create-title':'createTitle','create-button':'create','secret-label':'secret','dismiss':'dismiss','tokens-title':'tokens','activation-title':'activation','reviewed-label':'reviewed','activate':'enable','disable':'disable','audit-title':'auditTitle','audit-note':'auditNote','audit-refresh':'auditRefresh'}))el(id).textContent=t(key);
 renderDevice();
 renderIntegration();
 renderExternalAgents();
 if(!state)return;
 el('controls').hidden=false;el('identity').textContent=`${state.identity.workspace} · ${state.identity.app} · ${state.identity.agent}`;
 el('activation-status').textContent=state.enabled?t('enabled'):state.activation_available?t('disabled'):t('unavailable');
 el('activate').disabled=!state.activation_available||state.enabled;el('disable').disabled=!state.enabled;
 el('tokens').replaceChildren();if(!state.tokens.length)el('tokens').textContent=t('empty');
 for(const token of state.tokens){const row=document.createElement('div');row.className='token';const title=document.createElement('strong');title.textContent=token.name;const p=document.createElement('p');p.textContent=`${token.id} · ${t('expires')}: ${new Date(token.expires*1000).toLocaleString(language)} · ${t('last')}: ${token.last_used?new Date(token.last_used*1000).toLocaleString(language):t('never')}${token.ingress_peer?` · ${t('source')}: ${token.ingress_peer}`:''}`;const button=document.createElement('button');button.type='button';button.textContent=token.revoked?t('revoked'):t('revoke');button.disabled=token.revoked;button.onclick=()=>mutate(async()=>{if(!confirm(t('confirm')))return;await api('owner/revoke',{id:token.id});await load();feedback(t('ok'));});row.append(title,p,button);el('tokens').append(row);}
}
async function api(path,body){const r=await fetch(new URL(path,location.href),{method:body?'POST':'GET',cache:'no-store',credentials:'same-origin',headers:body?{'Content-Type':'application/json','X-External-Agent-CSRF':'1'}:{},body:body?JSON.stringify(body):undefined});if(!r.ok)throw new Error(r.status===401?'auth':'error');return r.json();}
async function load(){clearDevice();clearIntegration();clearExternalAgents();registryLoading=true;renderExternalAgents();try{state=await api('owner/state');render();await Promise.all([loadIntegration(),loadExternalAgents()]);}catch(error){registryLoading=false;state=null;el('controls').hidden=true;el('identity').textContent='';clearIntegration();clearExternalAgents();el('secret').value='';el('secret-area').hidden=true;el('integration-status').textContent=t(error.message==='auth'?'connectionDenied':'connectionFailed');throw error;}}
let auditEpoch=0;
function clearAudit(){auditEpoch++;el('audit').replaceChildren();el('audit-status').textContent='';}
async function loadAudit(){
 const epoch=++auditEpoch;el('audit').replaceChildren();el('audit-status').textContent=t('auditLoading');el('audit-refresh').disabled=true;
 let reader;
 try{
  const response=await fetch(new URL('owner/audit',location.href),{cache:'no-store',credentials:'same-origin',redirect:'error',signal:AbortSignal.timeout(10000)});
  if(!response.ok)throw new Error([401,403].includes(response.status)?'denied':'unavailable');
  if(response.headers.get('cache-control')!=='no-store')throw new Error('unavailable');
  reader=response.body.getReader();const chunks=[];let bytes=0;
  while(true){const chunk=await reader.read();if(chunk.done)break;bytes+=chunk.value.byteLength;if(bytes>131072)throw new Error('unavailable');chunks.push(chunk.value);}
  const data=JSON.parse(new TextDecoder().decode(concat(chunks,bytes)));
  if(!state||!data.identity||['workspace','app','agent'].some(k=>data.identity[k]!==state.identity[k])||!Array.isArray(data.requests)||data.requests.length>100)throw new Error('unavailable');
  for(const row of data.requests){
   for(const key of ['request_id','token','operation','outcome'])if(typeof row[key]!=='string'||row[key].length>256)throw new Error('unavailable');
   for(const key of ['target','started','ended'])if(row[key]!==null&&(typeof row[key]!=='string'||row[key].length>256))throw new Error('unavailable');
  }
  if(epoch!==auditEpoch)return;
  for(const row of data.requests){const item=document.createElement('div');item.className='token';const title=document.createElement('strong');title.textContent=`${row.operation} · ${row.outcome}`;const metadata=document.createElement('p');metadata.textContent=`${row.request_id} · token …${row.token.slice(-8)} · ${row.target??'—'} · ${row.started??'—'} → ${row.ended??'—'}`;item.append(title,metadata);el('audit').append(item);}
  el('audit-status').textContent=data.requests.length?'':t('auditEmpty');
 }catch(error){if(epoch===auditEpoch){el('audit').replaceChildren();el('audit-status').textContent=t(error.message==='denied'?'auditDenied':'auditFailed');}}
 finally{if(reader)await reader.cancel().catch(()=>{});if(epoch===auditEpoch)el('audit-refresh').disabled=false;}
}
function concat(chunks,size){const bytes=new Uint8Array(size);let offset=0;for(const chunk of chunks){bytes.set(chunk,offset);offset+=chunk.length;}return bytes;}
el('audit-refresh').onclick=loadAudit;
let busy=false;async function mutate(fn){if(busy)return;busy=true;try{await fn();}catch(e){feedback(t(e.message==='auth'?'auth':'error'));}finally{busy=false;}}
el('language').onchange=()=>{language=el('language').value;copyEpoch++;el('integration-feedback').textContent='';clearAudit();el('audit-refresh').disabled=false;render();};
el('create').onsubmit=e=>{e.preventDefault();mutate(async()=>{const r=await api('owner/tokens',{name:el('name').value,days:Number(el('days').value),authority_confirmed:el('authority').checked});secretIsAgent=false;secretTrigger='create-button';el('secret-label').textContent=t('secret');el('secret').value=r.plaintext;el('secret-area').hidden=false;el('secret').focus();el('secret').select();await load();feedback(t('created'));});};
el('dismiss').onclick=()=>{el('secret').value='';el('secret-area').hidden=true;el(secretTrigger).focus();};
async function activation(enabled){await api('owner/activation',{enabled,generation:state.generation,agent:state.identity.agent,policy_reviewed:el('reviewed').checked});await load();feedback(t('ok'));}
el('activate').onclick=()=>mutate(()=>activation(true));el('disable').onclick=()=>mutate(()=>activation(false));
let integrationEpoch=0,integrationData=null,copyEpoch=0;
function clearIntegration(){integrationEpoch++;copyEpoch++;integrationData=null;el('integration-content').hidden=true;el('integration-text').value='';el('integration-base').textContent='';el('integration-blockers').replaceChildren();el('integration-feedback').textContent='';el('integration-copy').disabled=true;}
function integrationReasons(data){
 const reasons=[];
 if(data.reachability==='loopback_only')reasons.push('connectionLoopback');
 else reasons.push('connectionReachability');
 if(!data.enabled)reasons.push('connectionDisabled');
 if(!data.activation_available)reasons.push('connectionAcceptance');
 if(!state?.tokens?.some(token=>!token.revoked&&Number.isFinite(token.expires)&&token.expires>Date.now()/1000))reasons.push('connectionNoToken');
 const known={loopback_only:'connectionLoopback',api_disabled:'connectionDisabled',disabled:'connectionDisabled',policy_acceptance_missing:'connectionAcceptance',policy_acceptance_unconfigured:'connectionAcceptance',external_reachability_unverified:'connectionReachability',data_mapping_not_verified:'connectionReachability',prompt_unavailable:'connectionPrompt',no_active_token:'connectionNoToken'};
 for(const code of data.blockers)reasons.push(known[code]||'connectionUnknown');
 return [...new Set(reasons)];
}
function integrationText(){return [t('connectionSetup'),t('connectionMissing'),...integrationReasons(integrationData).map(key=>'- '+t(key)),'',integrationData.skill_markdown].join('\n');}
function renderIntegration(){
 for(const [id,key] of Object.entries({'integration-title':'connectionTitle','integration-note':'connectionNote','integration-refresh':'connectionRefresh','integration-base-label':'connectionBase','integration-copy':'connectionCopy','integration-download':'connectionDownload','integration-preview-label':'connectionPreview','integration-preview-description':'connectionPreviewDescription','integration-policy':'connectionPolicy','integration-token':'connectionToken'}))el(id).textContent=t(key);
 el('integration-refresh').disabled=!state;
 if(!integrationData)return;
 el('integration-content').hidden=false;
 el('integration-status').textContent=t(!integrationData.enabled||!integrationData.activation_available||integrationData.reachability==='loopback_only'||integrationData.blockers.length?'connectionBlocked':'connectionUnverified');
 el('integration-base').textContent=integrationData.data_base_url;
 el('integration-blockers').replaceChildren();for(const key of integrationReasons(integrationData)){const item=document.createElement('li');item.textContent=t(key);el('integration-blockers').append(item);}
 if(!integrationData.prompt_available){const item=document.createElement('li');item.textContent=t('connectionPrompt');el('integration-blockers').append(item);}
 el('integration-text').value=integrationText();el('integration-copy').disabled=false;
}
async function loadIntegration(){
 clearIntegration();const epoch=integrationEpoch;el('integration-status').textContent=t('connectionLoading');el('integration-refresh').disabled=true;
 let reader;
 try{
  const response=await fetch(new URL('owner/integration',location.href),{cache:'no-store',credentials:'same-origin',redirect:'error',signal:AbortSignal.timeout(10000)});
  if(!response.ok)throw Error([401,403].includes(response.status)?'denied':'unavailable');
  if(response.headers.get('cache-control')!=='no-store')throw Error('unavailable');
  reader=response.body.getReader();const chunks=[];let size=0;
  while(true){const chunk=await reader.read();if(chunk.done)break;size+=chunk.value.byteLength;if(size>131072)throw Error('unavailable');chunks.push(chunk.value);}
  const data=JSON.parse(new TextDecoder().decode(concat(chunks,size)));
  if(!state||data.schema_version!==1||!data.identity||['workspace','app','agent'].some(k=>data.identity[k]!==state.identity[k])||!['loopback_only','configured_https_unverified'].includes(data.reachability)||['enabled','activation_available','prompt_available'].some(k=>typeof data[k]!=='boolean')||!Array.isArray(data.blockers)||data.blockers.length>16||data.blockers.some(code=>typeof code!=='string'||!code.length||code.length>64)||typeof data.skill_markdown!=='string'||!data.skill_markdown.length||data.skill_markdown.length>65536||data.manual_path!=='SKILL.md'||typeof data.data_base_url!=='string'||data.data_base_url.length>2048)throw Error('unavailable');
  const url=new URL(data.data_base_url);const loopback=url.hostname==='localhost'||url.hostname==='[::1]'||/^127\./.test(url.hostname);
  if(url.username||url.password||url.search||url.hash||url.pathname!=='/'||!(url.protocol==='https:'||url.protocol==='http:'&&loopback))throw Error('unavailable');
  if(data.reachability==='loopback_only'&&!loopback||data.reachability==='configured_https_unverified'&&url.protocol!=='https:')throw Error('unavailable');
  if(epoch!==integrationEpoch)return;
  integrationData=data;renderIntegration();
 }catch(error){if(epoch===integrationEpoch){clearIntegration();el('integration-status').textContent=t(error.message==='denied'?'connectionDenied':'connectionFailed');}}
 finally{if(reader)await reader.cancel().catch(()=>{});if(epoch===integrationEpoch||!integrationData)el('integration-refresh').disabled=!state;}
}
el('integration-refresh').onclick=loadIntegration;
el('integration-copy').onclick=async()=>{
 if(!integrationData)return;const epoch=++copyEpoch,text=integrationText();
 try{await navigator.clipboard.writeText(text);if(epoch===copyEpoch)el('integration-feedback').textContent=t('connectionCopied');}
 catch{if(epoch!==copyEpoch||!integrationData)return;el('integration-text').closest('details').open=true;el('integration-text').focus();el('integration-text').select();el('integration-feedback').textContent=t('connectionFallback');}
};
Object.assign(words.en,{externalReplace:'Create new token',externalTokenName:'New token name',externalTokenNote:'The stable address and Agent name stay unchanged. Old tokens remain valid until you revoke them in the token list.',externalCancel:'Cancel token creation',externalTokenCreated:'New token created. Save it now; old tokens have not been revoked.',externalTokenUncertain:'Token creation was not confirmed. Check the token list and revoke any lost token before creating another. Registry reads cannot confirm or recover this new token.'});
Object.assign(words.zh,{externalReplace:'签发新 token',externalTokenName:'新 token 名称',externalTokenNote:'稳定地址和 Agent 名称不变。旧 token 仍然有效，请在 token 列表中单独撤销。',externalCancel:'取消签发',externalTokenCreated:'新 token 已签发，请立即保存；旧 token 未被撤销。',externalTokenUncertain:'新 token 签发结果未确认。请检查 token 列表并撤销丢失的 token，再决定是否重新签发。读取注册表不能确认或恢复这个新 token。'});
Object.assign(words.en,{deviceTitle:'Authorize an external Agent',deviceNote:'Paste the short code shown by your Agent. Review the request before approving. Your Agent receives its credential directly; you do not need to copy a token.',deviceCode:'Device authorization code',deviceLookup:'Review request',deviceDays:'Credential expires after',deviceAuthority:'I authorize this Agent to use this App’s current workspace CLI/API authority and its own inbox.',deviceApprove:'Approve Agent',deviceDeny:'Deny request',deviceLoading:'Looking up the request…',deviceUnknown:'Request not found or unavailable. Check the code and try again.',deviceExpired:'This request has expired. Ask your Agent for a new code.',deviceDenied:'Request denied. No credential was delivered.',deviceApproved:'Request approved. Your Agent can now collect its credential.',deviceConsumed:'This request has already been completed. It cannot issue another credential.',deviceUncertain:'The result could not be confirmed. Review the code again before taking another action. No confirmation was retried.',deviceName:'Agent name',deviceKey:'Stable key',deviceScope:'Requested authority',deviceExpires:'Request expires',deviceInvalid:'Enter the short code provided by your Agent.',deviceLimited:'Too many code checks. Wait a minute before reviewing again; no request was retried.',deviceRefused:'Confirmation was refused or the request has changed. Review the code again before taking another action.',deviceOwnerDenied:'Owner verification required. Previous request details cleared.'});
Object.assign(words.zh,{deviceTitle:'授权外部 Agent',deviceNote:'粘贴 Agent 提供的短码，核对请求后确认。凭据由 Agent 直接领取，无需您复制 token。',deviceCode:'设备授权码',deviceLookup:'核对请求',deviceDays:'凭据有效期',deviceAuthority:'我授权此 Agent 使用此 App 当前工作区的 CLI/API 权限及自己的收件箱。',deviceApprove:'确认授权',deviceDeny:'拒绝请求',deviceLoading:'正在查询请求…',deviceUnknown:'未找到请求或暂不可用，请核对设备码。',deviceExpired:'请求已过期，请让 Agent 申请新设备码。',deviceDenied:'请求已拒绝，未交付凭据。',deviceApproved:'已确认授权，Agent 可直接领取凭据。',deviceConsumed:'该请求已完成，不能再次签发凭据。',deviceUncertain:'结果未确认，请重新核对设备码后再操作。系统未重试确认请求。',deviceName:'Agent 名称',deviceKey:'稳定 key',deviceScope:'请求权限',deviceExpires:'请求过期时间',deviceInvalid:'请输入 Agent 提供的短设备码。',deviceLimited:'设备码查询过于频繁，请稍候一分钟再核对；系统未重试。',deviceRefused:'确认被拒绝或请求状态已改变，请重新核对设备码后再操作。',deviceOwnerDenied:'需要验证 owner 身份，已清除旧请求信息。'});
let registryEpoch=0,registryData=null,registryPending=false,registryLoading=false,registryUncertain=false,registryUncertainKey='',registryTokenKey='',registryUncertainToken=false,registryCreateEpoch=0,secretIsAgent=false,secretTrigger='create-button';
function clearExternalAgents(){registryEpoch++;registryData=null;el('external-agents-list').replaceChildren();el('external-agent-feedback').textContent='';}
function validExternalAgent(agent){return agent&&typeof agent.key==='string'&&/^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(agent.key)&&typeof agent.name==='string'&&agent.name.trim().length>0&&new TextEncoder().encode(agent.name).length<=128&&agent.address===state.identity.app+'/'+agent.key&&typeof agent.enabled==='boolean'&&Number.isSafeInteger(agent.created)&&agent.created>=0&&agent.created<=8640000000000;}
function renderExternalAgents(){
 for(const [id,key] of Object.entries({'external-agents-title':'externalTitle','external-agents-note':'externalNote','external-agents-refresh':'externalRefresh','external-agent-key-label':'externalKey','external-agent-key-note':'externalKeyNote','external-agent-name-label':'externalName','external-agent-days-label':'externalDays','external-agent-authority-label':'externalAuthority','external-agent-create-button':'externalCreate'}))el(id).textContent=t(key);
 el('external-agent-name-label').textContent=t(registryTokenKey?'externalTokenName':'externalName');el('external-agent-create-button').textContent=t(registryTokenKey?'externalReplace':'externalCreate');el('external-agent-key').readOnly=!!registryTokenKey;el('external-agent-cancel').hidden=!registryTokenKey;el('external-agent-cancel').textContent=t('externalCancel');el('external-agent-cancel').disabled=registryPending;el('external-agent-token-note').hidden=!registryTokenKey;el('external-agent-token-note').textContent=t('externalTokenNote');
 if(secretIsAgent)el('secret-label').textContent=t('externalSecret');
 el('external-agents-refresh').disabled=!state||registryPending||registryLoading;el('external-agent-create-button').disabled=!state||registryPending||registryLoading||registryUncertain;
 if(!registryData)return;
 el('external-agents-list').replaceChildren();
 el('external-agents-status').textContent=registryData.length?'':t('externalEmpty');
 for(const agent of registryData){
  const row=document.createElement('div');row.className='external-agent-row';const name=document.createElement('strong');name.textContent=agent.name;
  const status=document.createElement('p');status.textContent=`${agent.key} · ${t(agent.enabled?'externalInboxEnabled':'externalInboxDisabled')} · ${t('externalCreatedAt')}: ${new Date(agent.created*1000).toLocaleString(language)}`;
  const label=document.createElement('label');label.textContent=t('externalAddress');const address=document.createElement('input');address.className='external-agent-address';address.readOnly=true;address.value=agent.address;address.setAttribute('aria-label',t('externalAddress')+' · '+agent.key);label.append(address);
  const copy=document.createElement('button');copy.type='button';copy.textContent=t('externalCopy');copy.onclick=async()=>{try{await navigator.clipboard.writeText(agent.address);el('external-agent-feedback').textContent=t('externalCopied');}catch{address.focus();address.select();el('external-agent-feedback').textContent=t('externalCopyFallback');}};
  const replace=document.createElement('button');replace.type='button';replace.className='external-agent-replace';replace.textContent=t('externalReplace');replace.disabled=!agent.enabled||registryPending||registryLoading||registryUncertain;replace.onclick=()=>{registryTokenKey=agent.key;el('external-agent-key').value=agent.key;el('external-agent-name').value='';el('external-agent-authority').checked=false;el('secret').value='';el('secret-area').hidden=true;renderExternalAgents();el('external-agent-name').focus();};
  row.append(name,status,label,copy,replace);el('external-agents-list').append(row);
 }
}
async function externalAgentRequest(body,replacement=false){
 const response=await fetch(new URL(replacement?'owner/external-agents/tokens':'owner/external-agents',location.href),{method:body?'POST':'GET',credentials:'same-origin',cache:'no-store',redirect:'error',signal:AbortSignal.timeout(10000),headers:body?{'Content-Type':'application/json','X-External-Agent-CSRF':'1'}:{},body:body?JSON.stringify(body):undefined});
 if(!response.ok){const error=Error([401,403].includes(response.status)?'denied':'refused');error.refused=response.status>=400&&response.status<500;throw error;}
 if(response.headers.get('cache-control')!=='no-store'||response.status!==(body?201:200))throw Error('unavailable');
 let reader;try{reader=response.body.getReader();let size=0;const chunks=[];while(true){const chunk=await reader.read();if(chunk.done)break;size+=chunk.value.byteLength;if(size>131072)throw Error('unavailable');chunks.push(chunk.value);}return JSON.parse(new TextDecoder().decode(concat(chunks,size)));}finally{if(reader)await reader.cancel().catch(()=>{});}
}
async function loadExternalAgents(){
 registryLoading=true;clearExternalAgents();const epoch=registryEpoch;el('external-agents-status').textContent=t('externalLoading');el('external-agents-refresh').disabled=true;
 try{
  const data=await externalAgentRequest();if(!state||!data||!Array.isArray(data.agents)||data.agents.length>100||data.agents.some(agent=>!validExternalAgent(agent))||new Set(data.agents.map(agent=>agent.key)).size!==data.agents.length)throw Error('unavailable');
  if(epoch!==registryEpoch)return;
  registryData=data.agents;const uncertainToken=registryUncertain&&registryUncertainToken;const recovered=registryUncertain&&!uncertainToken&&data.agents.some(agent=>agent.key===registryUncertainKey);registryUncertain=false;renderExternalAgents();if(uncertainToken){el('external-agent-feedback').textContent=t('externalTokenUncertain');}if(recovered){el('external-agent-key').value='';el('external-agent-name').value='';el('external-agent-authority').checked=false;el('external-agent-feedback').textContent=t('externalRecovered');}
 }catch(error){if(epoch===registryEpoch){registryData=null;el('external-agents-list').replaceChildren();el('external-agents-status').textContent=t(error.message==='denied'?'externalDenied':'externalFailed');if(error.message==='denied'){registryUncertain=true;el('secret').value='';el('secret-area').hidden=true;}}}
 finally{if(epoch===registryEpoch){registryLoading=false;renderExternalAgents();}}
}
el('external-agent-cancel').onclick=()=>{registryTokenKey='';el('external-agent-key').value='';el('external-agent-name').value='';el('external-agent-authority').checked=false;renderExternalAgents();el('external-agent-key').focus();};
el('external-agents-refresh').onclick=()=>load().catch(()=>{});
el('external-agent-create').onsubmit=async event=>{
 event.preventDefault();if(!state||busy||registryPending||registryUncertain)return;
 const replacement=!!registryTokenKey;const key=registryTokenKey||el('external-agent-key').value,name=el('external-agent-name').value.trim(),days=Number(el('external-agent-days').value);
 if(!/^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(key)||!name||new TextEncoder().encode(name).length>128||![30,90].includes(days)||!el('external-agent-authority').checked){el('external-agent-feedback').textContent=t('externalInvalid');return;}
 const epoch=++registryCreateEpoch;busy=true;registryPending=true;registryEpoch++;el('secret').value='';el('secret-area').hidden=true;el('external-agent-feedback').textContent='';renderExternalAgents();
 try{
  const data=await externalAgentRequest({key,name,days,authority_confirmed:true},replacement);
  if(!state||epoch!==registryCreateEpoch)return;
  if(!validExternalAgent(data.agent)||data.agent.key!==key||!data.token||typeof data.token.id!=='string'||typeof data.plaintext!=='string'||!/^ext1_[0-9a-f]{32}\.[0-9a-f]{64}$/.test(data.plaintext))throw Error('unavailable');
  secretIsAgent=true;secretTrigger='external-agent-create-button';el('secret-label').textContent=t('externalSecret');el('secret').value=data.plaintext;el('secret-area').hidden=false;el('secret').focus();el('secret').select();
  registryTokenKey='';el('external-agent-key').value='';el('external-agent-name').value='';el('external-agent-authority').checked=false;
  await load();if(epoch===registryCreateEpoch&&state&&el('secret').value)el('external-agent-feedback').textContent=t(replacement?'externalTokenCreated':'externalCreated');
 }catch(error){if(epoch===registryCreateEpoch&&state){registryUncertain=true;registryUncertainKey=key;registryUncertainToken=replacement;if(error.message==='denied'){clearExternalAgents();el('external-agents-status').textContent=t('externalDenied');}el('external-agent-feedback').textContent=t(error.message==='denied'?'externalDenied':error.refused?'externalRejected':replacement?'externalTokenUncertain':'externalAmbiguous');}}
 finally{if(epoch===registryCreateEpoch){busy=false;registryPending=false;renderExternalAgents();}}
};
addEventListener('pagehide',()=>{registryCreateEpoch++;});
let deviceRequest=null,deviceEpoch=0,devicePending=false,deviceTimer;
function clearDevice(){deviceEpoch++;deviceRequest=null;clearTimeout(deviceTimer);el('device-request').hidden=true;el('device-details').replaceChildren();el('device-authority').checked=false;}
function renderDevice(){
 for(const [id,key] of Object.entries({'device-title':'deviceTitle','device-note':'deviceNote','device-code-label':'deviceCode','device-lookup-button':'deviceLookup','device-days-label':'deviceDays','device-authority-label':'deviceAuthority','device-approve':'deviceApprove','device-deny':'deviceDeny'}))el(id).textContent=t(key);
 el('device-lookup-button').disabled=!state||devicePending;
 const pending=!!deviceRequest&&deviceRequest.status==='pending'&&deviceRequest.expires_at*1000>Date.now();
 el('device-approve').disabled=!state||devicePending||!pending||!el('device-authority').checked;el('device-deny').disabled=!state||devicePending||!pending;
 if(!deviceRequest)return;
 el('device-request').hidden=false;el('device-details').replaceChildren();
 const fields=[[t('deviceName'),deviceRequest.name],[t('deviceKey'),deviceRequest.key],[t('deviceScope'),t('externalAuthority')],[t('deviceExpires'),new Date(deviceRequest.expires_at*1000).toLocaleString(language)]];
 if(deviceRequest.existing_agent)fields.push([t('externalName'),deviceRequest.existing_name]);
 for(const [title,text] of fields){const dt=document.createElement('dt'),dd=document.createElement('dd');dt.textContent=title;dd.textContent=text;el('device-details').append(dt,dd);}
}
async function deviceApi(path,body){
 const response=await fetch(new URL(path,location.href),{method:'POST',credentials:'same-origin',cache:'no-store',redirect:'error',signal:AbortSignal.timeout(10000),headers:{'Content-Type':'application/json','X-External-Agent-CSRF':'1'},body:JSON.stringify(body)});
 if(!response.ok)throw Error(response.status===429?'deviceLimited':[401,403].includes(response.status)?'deviceOwnerDenied':response.status===410?'deviceExpired':'deviceUnknown');
 if(response.status!==200||response.headers.get('cache-control')!=='no-store')throw Error('deviceUnknown');
 const reader=response.body.getReader();let size=0;const chunks=[];try{while(true){const part=await reader.read();if(part.done)break;size+=part.value.length;if(size>16384)throw Error('deviceUnknown');chunks.push(part.value);}return JSON.parse(new TextDecoder().decode(concat(chunks,size)));}finally{await reader.cancel().catch(()=>{});}
}
function deviceStatus(status){return status==='claimed'?'deviceConsumed':status==='approved'?'deviceApproved':status==='denied'?'deviceDenied':'deviceExpired';}
el('device-code').oninput=()=>{clearDevice();el('device-status').textContent='';renderDevice();};
el('device-authority').onchange=renderDevice;
el('device-lookup').onsubmit=async event=>{
 event.preventDefault();if(!state||busy||devicePending)return;clearDevice();const code=el('device-code').value.trim().toUpperCase();
 if(!/^[A-F0-9]{4}-?[A-F0-9]{4}$/.test(code)){el('device-status').textContent=t('deviceInvalid');return;}
 const epoch=deviceEpoch;devicePending=true;renderDevice();el('device-status').textContent=t('deviceLoading');
 try{const data=await deviceApi('owner/device/lookup',{user_code:code});if(epoch!==deviceEpoch||!state)return;const request=data.request;
 if(!request||!['pending','approved','denied','claimed'].includes(request.status)||!Number.isSafeInteger(request.expires_at)||request.expires_at<0||request.expires_at>8640000000000||typeof request.name!=='string'||!request.name.trim()||new TextEncoder().encode(request.name).length>128||typeof request.key!=='string'||!/^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(request.key)||request.scope!=='workspace_operations_and_own_inbox'||typeof request.existing_agent!=='boolean'||(request.existing_agent?(typeof request.existing_name!=='string'||new TextEncoder().encode(request.existing_name).length>128):request.existing_name!==null))throw Error('deviceUnknown');
 deviceRequest={...request,user_code:code};el('device-status').textContent=request.expires_at*1000<=Date.now()?t('deviceExpired'):request.status==='pending'?'':t(deviceStatus(request.status));
 if(request.status==='pending'&&request.expires_at*1000>Date.now())deviceTimer=setTimeout(()=>{if(epoch===deviceEpoch){el('device-status').textContent=t('deviceExpired');renderDevice();}},Math.min(request.expires_at*1000-Date.now(),2147483647));
 }catch(error){if(epoch===deviceEpoch){clearDevice();el('device-status').textContent=t(words.en[error.message]?error.message:'deviceUnknown');}}
 finally{devicePending=false;renderDevice();}
};
async function decideDevice(approve){
 if(!state||busy||devicePending||!deviceRequest||deviceRequest.status!=='pending'||deviceRequest.expires_at*1000<=Date.now()||(approve&&!el('device-authority').checked))return;
 const request=deviceRequest,epoch=deviceEpoch,days=Number(el('device-days').value);if(![30,90].includes(days))return;
 devicePending=true;renderDevice();
 try{const data=await deviceApi('owner/device/decision',{user_code:request.user_code,approve,days,authority_confirmed:el('device-authority').checked});if(epoch!==deviceEpoch||!state)return;if(data.status!==(approve?'approved':'denied'))throw Error('deviceUncertain');clearDevice();el('device-status').textContent=t(deviceStatus(data.status));}
 catch(error){if(epoch===deviceEpoch){clearDevice();el('device-status').textContent=t(error.message==='deviceLimited'?'deviceLimited':error.message==='deviceOwnerDenied'?'deviceRefused':'deviceUncertain');}}
 finally{devicePending=false;renderDevice();}
}
el('device-approve').onclick=()=>decideDevice(true);el('device-deny').onclick=()=>decideDevice(false);
addEventListener('pagehide',()=>{clearDevice();el('device-code').value='';});
addEventListener('pagehide',()=>{el('secret').value='';clearAudit();clearIntegration();clearExternalAgents();state=null;});render();feedback(t('loading'));load().then(()=>feedback('')).catch(e=>feedback(t(e.message==='auth'?'auth':'error')));
