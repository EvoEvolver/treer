// All paths stay relative to the App root, including embedded tunnel prefixes.
export async function readSettings() {
 const response=await fetch('api/settings',{credentials:'same-origin',headers:{Accept:'application/json'}});
 return {ok:response.ok,status:response.status,value:await response.json()};
}
export async function readLoginUrl():Promise<string|null> {
 const response=await fetch('api/endpoint/auth/config',{credentials:'same-origin'});
 if(!response.ok)return null;const v=await response.json();
 if(typeof v.login_url!=='string')return null;const u=new URL(v.login_url);
 // The destination is configured server state, never external message content.
 return u.protocol==='https:'||(u.protocol==='http:'&&['127.0.0.1','[::1]'].includes(u.hostname))?u.href:null;
}
export async function endSession() {
 const response=await fetch('api/endpoint/auth/logout',{method:'POST',credentials:'same-origin',headers:{'X-Endpoint-Request':'1'}});
 if(!response.ok)throw new Error('logout_rejected');
}

export async function readEditUrl():Promise<string|null> {
 const response=await fetch('api/endpoint/auth/config',{credentials:'same-origin'});
 if(!response.ok)return null;const value=await response.json();
 if(typeof value.settings_url!=='string')return null;const u=new URL(value.settings_url);
 return u.pathname==='/'&&(u.protocol==='https:'||(u.protocol==='http:'&&['127.0.0.1','[::1]'].includes(u.hostname)))?u.href:null;
}
export async function saveConfiguration(command:unknown) {
 const response=await fetch('api/configuration',{method:'POST',credentials:'same-origin',headers:{'Content-Type':'application/json','X-Endpoint-Request':'1'},body:JSON.stringify(command)});
 const value=await response.json();
 if(!response.ok)throw new Error(typeof value?.error?.code==='string'?value.error.code:'configuration_unavailable');
 return value;
}

export async function saveTelegramToken(command:unknown) {
 const response=await fetch('api/telegram/token',{method:'POST',credentials:'same-origin',headers:{'Content-Type':'application/json','X-Endpoint-Request':'1'},body:JSON.stringify(command)});
 const value=await response.json();
 if(!response.ok)throw new Error(typeof value?.error?.code==='string'?value.error.code:'telegram_token_unavailable');
 return value;
}

function emailPath(id:string) {
 if(!/^[a-f0-9]{64}$/.test(id))throw new Error('email_thread_invalid');
 return 'api/email/thread/'+id;
}
// Only allowlisted categories reach the UI; never reflect arbitrary server errors.
export function emailFailure(status:number,code:unknown){
 if(status===401&&(code==='owner_auth_required'||code==='owner_identity_invalid'))return 'email_login_required';
 if(status===403&&(code==='current_workspace_member_required'||code==='current_human_owner_required'))return 'email_member_required';
 if(status===403&&(code==='owner_csrf_rejected'||code==='email_request_host_rejected'||code==='email_request_context_rejected'))return 'email_context_rejected';
 if(status===403)return 'email_access_unavailable';
 return 'email_service_unavailable';
}
async function emailErrorCode(r:Response){
 const reader=r.body?.getReader();if(!reader)return undefined;
 const chunks:Uint8Array[]=[];let size=0;
 try{for(;;){const p=await reader.read();if(p.done)break;size+=p.value.byteLength;if(size>4096)return undefined;chunks.push(p.value);}const bytes=new Uint8Array(size);let at=0;for(const p of chunks){bytes.set(p,at);at+=p.length;}return JSON.parse(new TextDecoder().decode(bytes))?.error?.code;}catch{return undefined;}finally{void reader.cancel().catch(()=>{});}
}
export async function readEmailThread(id:string) {
 const r=await fetch(emailPath(id),{credentials:'same-origin',cache:'no-store',redirect:'error',signal:AbortSignal.timeout(10000),headers:{Accept:'application/json'}});
 if(!r.ok)return {status:r.status,ok:false,value:null,failure:emailFailure(r.status,await emailErrorCode(r))};
 return {status:r.status,ok:true,value:await r.json(),failure:''};
}
export async function submitEmailReply(id:string,key:string,body:string) {
 const r=await fetch(emailPath(id),{method:'POST',credentials:'same-origin',headers:{'Content-Type':'application/json','X-Endpoint-Request':'1'},body:JSON.stringify({key,body}),redirect:'error',signal:AbortSignal.timeout(10000)});
 if(!r.ok)throw new Error(emailFailure(r.status,await emailErrorCode(r)));
}
export async function endMemberSession() {
 const r=await fetch('api/email/auth/logout',{method:'POST',credentials:'same-origin',headers:{'X-Endpoint-Request':'1'}});
 if(!r.ok)throw new Error('logout_rejected');
}

export async function readHumanInbox(){
 const r=await fetch('api/email/inbox',{credentials:'same-origin',cache:'no-store',redirect:'error',signal:AbortSignal.timeout(10000),headers:{Accept:'application/json'}});
 if(!r.ok)return {ok:false,status:r.status,value:null};
 const reader=r.body!.getReader();let size=0;const chunks:Uint8Array[]=[];
 try{while(true){const part=await reader.read();if(part.done)break;size+=part.value.byteLength;if(size>65536)throw Error('inbox_unavailable');chunks.push(part.value);}const bytes=new Uint8Array(size);let offset=0;for(const chunk of chunks){bytes.set(chunk,offset);offset+=chunk.length;}return {ok:true,status:r.status,value:JSON.parse(new TextDecoder().decode(bytes))};}finally{await reader.cancel().catch(()=>{});}
}

// Owner-only read preview uses the existing CSRF-protected configuration route.
export async function previewEmailSetup(){
 const r=await fetch('api/configuration',{method:'POST',credentials:'same-origin',redirect:'error',cache:'no-store',signal:AbortSignal.timeout(10000),headers:{'Content-Type':'application/json','X-Endpoint-Request':'1'},body:JSON.stringify({action:'preview_email_setup'})});
 const reader=r.body!.getReader();let size=0;const chunks:Uint8Array[]=[];
 try{while(true){const part=await reader.read();if(part.done)break;size+=part.value.byteLength;if(size>262144)throw Error('email_setup_unavailable');chunks.push(part.value);}const bytes=new Uint8Array(size);let offset=0;for(const chunk of chunks){bytes.set(chunk,offset);offset+=chunk.length;}const value=JSON.parse(new TextDecoder().decode(bytes));if(!r.ok)throw Error(typeof value?.error?.code==='string'?value.error.code:'email_setup_unavailable');return value;}finally{await reader.cancel().catch(()=>{});}
}
