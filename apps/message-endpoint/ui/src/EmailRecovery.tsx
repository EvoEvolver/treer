import {useState} from 'react';
import {saveConfiguration} from './api';
type Item={intent_id:string;generation:number;state:string;abandonable:boolean;reference:string|null;terminal:boolean};
type Delivery={status:'email_delivery_status';endpoint_id:string;revision:number;paused:boolean;delivery:{prepared:number;abandoned:number;dispatching:number;uncertain:number;provider_accepted:number;at_capacity:boolean};items:Item[];truncated:boolean};
export function EmailRecovery({endpoint,zh,disabled,refresh}:{endpoint:string;zh:boolean;disabled:boolean;refresh:()=>Promise<void>}) {
 const [value,setValue]=useState<Delivery|null>(null),[busy,setBusy]=useState(false),[error,setError]=useState(''),[confirm,setConfirm]=useState(''),[reference,setReference]=useState('');
 async function run(item?:Item,action?:string){
  if(item&&(!value?.paused||confirm!==item.intent_id))return;
  setBusy(true);setError('');
  try{const result=await saveConfiguration(item?{action,endpoint_id:endpoint,revision:value!.revision,intent_id:item.intent_id,confirm:true,...(action==='resolve_email_observed'?{reference}: {})}:{action:'inspect_email_delivery',endpoint_id:endpoint});if(result.status!=='email_delivery_status'||result.endpoint_id!==endpoint)throw new Error('email_recovery_unavailable');setValue(result as Delivery);setConfirm('');setReference('');if(item)await refresh();}
  catch(e){setValue(null);setError(e instanceof Error?e.message:'email_recovery_unavailable');}finally{setBusy(false);}
 }
 return <details className="recovery"><summary>{zh?'邮件发送状态与恢复':'Email send status and recovery'}</summary>
 <p>{zh?'状态不确定的邮件可能已发出，不会自动重发。核对、放弃和清理需要暂停绑定。核对仅记录 owner 观察，不代表 provider 证明；不会恢复发送。':'Uncertain mail may already have been sent. No automatic resend. Pause the binding before reconciliation, abandonment or cleanup. Reconciliation records an owner observation, not provider proof; sending stays paused.'}</p>
 <button disabled={disabled||busy} onClick={()=>void run()}>{zh?'检查邮件投递':'Inspect email delivery'}</button>{error&&<p role="alert">{error}</p>}
 {value&&<><p>{zh?'已准备':'Prepared'}: {value.delivery.prepared} · {zh?'结果不明（待核对）':'Uncertain (held)'}: {value.delivery.uncertain} · {zh?'发送中':'Dispatching'}: {value.delivery.dispatching} · {zh?'Treer 平台已接受':'Accepted by Treer'}: {value.delivery.provider_accepted} · {zh?'已放弃':'Abandoned'}: {value.delivery.abandoned}</p><p>{zh?'平台接受不等于收件箱送达。结果不明不代表失败；核对前不会自动重发。':'Platform acceptance is not mailbox delivery. Uncertain does not mean failed; there is no automatic resend before reconciliation.'}</p>
 {value.delivery.at_capacity&&<p role="status">{zh?'容量已满，保持暂停以便核对。':'Capacity full; keep paused for reconciliation.'}</p>}{value.truncated&&<p>{zh?'仅显示前 20 项。':'Showing first 20 items.'}</p>}
 {value.items.map(item=><div key={item.intent_id}><code>{item.intent_id}</code> · {({prepared:zh?'已准备':'Prepared',dispatching:zh?'发送中':'Dispatching',uncertain:zh?'结果不明（待核对）':'Uncertain (held)',provider_accepted:zh?'Treer 平台已接受':'Accepted by Treer',owner_observed_receipt:zh?'owner 已观察到收件':'Owner-observed receipt',abandoned:zh?'已放弃':'Abandoned'} as Record<string,string>)[item.state]??item.state} · {item.generation}
 {(item.abandonable||item.terminal||item.state==='uncertain')&&<>
 <label><input type="checkbox" checked={confirm===item.intent_id} disabled={disabled||busy||!value.paused} onChange={e=>{setConfirm(e.target.checked?item.intent_id:'');setReference('');}}/>{zh?'确认对此项操作；不恢复发送。':'Confirm an action on this item; sending stays paused.'}</label>
 {item.abandonable&&<button disabled={disabled||busy||!value.paused||confirm!==item.intent_id} onClick={()=>void run(item,'abandon_prepared_email')}>{zh?'放弃 prepared 项':'Abandon prepared item'}</button>}
 {item.state==='uncertain'&&!item.terminal&&<><p>{zh?'仅在目标邮箱实际看到该邮件及 reference 时核对。未收到、等待时间或冲突证据均不可核对；保持不确定。':'Reconcile only after observing the mail and reference in the intended mailbox. Absence, elapsed time or conflicting evidence leaves this item uncertain.'}</p><label>{zh?'输入邮箱中看到的 16 位 reference':'Enter the 16-character reference observed in the mailbox'}<input value={confirm===item.intent_id?reference:''} maxLength={16} autoComplete="off" disabled={disabled||busy||!value.paused||confirm!==item.intent_id} onChange={e=>setReference(e.target.value)}/></label><button disabled={disabled||busy||!value.paused||confirm!==item.intent_id||!/^[0-9a-f]{16}$/.test(reference)} onClick={()=>void run(item,'resolve_email_observed')}>{zh?'记录 owner 已观察到收件':'Record owner-observed receipt'}</button></>}
 {item.terminal&&<><p>{zh?'清理至少终结 7 天的会话内容，每批每类最多 20 条；长会话可重复推进；保留去重 tombstone，关闭会话。旧 outbox 文件由 worker 后续清理；备份不受影响。':'Retire payloads from a fully terminal conversation terminal for at least 7 days, in batches of at most 20 records per class; repeat for long conversations. Dedupe tombstones remain and the conversation closes. The worker subsequently removes its outbox files; backups remain.'}</p><button disabled={disabled||busy||!value.paused||confirm!==item.intent_id} onClick={()=>void run(item,'purge_email_payload')}>{zh?'清理终结会话内容':'Retire terminal conversation payloads'}</button></>}
 </>}</div>)}</>}
 </details>;
}
