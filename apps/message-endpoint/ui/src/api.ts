// All paths stay relative to the App root, including embedded tunnel prefixes.
export async function readSettings() {
 const response=await fetch('api/settings',{credentials:'same-origin',headers:{Accept:'application/json'}});
 return {ok:response.ok,status:response.status,value:await response.json()};
}
export async function readLoginUrl():Promise<string|null> {
 const response=await fetch('api/auth/config',{credentials:'same-origin'});
 if(!response.ok)return null;const v=await response.json();
 if(typeof v.login_url!=='string')return null;const u=new URL(v.login_url);
 // The destination is configured server state, never external message content.
 return u.protocol==='https:'||(u.protocol==='http:'&&['127.0.0.1','[::1]'].includes(u.hostname))?u.href:null;
}
export async function endSession() {
 const response=await fetch('api/auth/logout',{method:'POST',credentials:'same-origin',headers:{'X-Endpoint-Request':'1'}});
 if(!response.ok)throw new Error('logout_rejected');
}
