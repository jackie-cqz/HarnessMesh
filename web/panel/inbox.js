'use strict';
// One read-only view of task execution, artifact acceptance and ACP owner work.
// Links reuse existing inspectors; this module never submits or approves work.
const inboxState={sessions:null,sessionsObserved:null,sessionError:null,overviewError:null,taskRows:[],taskListError:null,taskObserved:null,taskObservations:new Map(),sessionObservations:new Map(),failures:new Map(),loading:false,lastRefresh:0,daemonPid:null,epoch:0,cursor:0,filter:'attention'};
const inboxCategoryNames={attention:'需要处理',approval:'本机审批',permission:'工具授权',execution:'执行中与等待',acceptance:'待验收',recent:'最近结果',all:'全部'};
const inboxTaskLabels={PENDING_APPROVAL:'待审批',QUEUED:'已排队',RUNNING:'执行中',CANCELLING:'取消中',WAITING_INPUT:'等待输入',SUCCEEDED:'执行成功',FAILED:'执行失败',CANCELLED:'已取消',REJECTED:'已拒绝',EXPIRED:'已过期',UNKNOWN:'执行结果不确定',UNCONFIRMED:'提交尚未确认'};
const inboxAcceptanceNames={passed:'核验通过',failed:'核验失败',pending:'待验收',not_required:'未设置工件验收',unknown:'验收状态未知'};
const inboxLatestTime=(a,b)=>Math.max(a||0,b||0)||null;

// Kept independent of browser state so status/freshness rules can be verified
// without a model, a running Node or a browser.
function buildInboxItems({tasks=[],sessions=[],localNodeId=null,taskObserved=null,sessionObserved=null}={}){
 const items=[];
 for(const task of tasks){
  const outbound=task._direction==='outbound',status=task.status||'UNCONFIRMED',check=task._acceptance||{state:task.request?.expected_artifacts?.length?'pending':'not_required'};
  let category='recent';
  if(status==='PENDING_APPROVAL')category=outbound?'execution':'approval';
  else if(['QUEUED','RUNNING','CANCELLING'].includes(status))category='execution';
  else if(['FAILED','UNKNOWN','WAITING_INPUT','UNCONFIRMED'].includes(status))category='attention';
  else if(status==='SUCCEEDED'&&check.state==='failed')category='attention';
  else if(status==='SUCCEEDED'&&['pending','unknown'].includes(check.state))category='acceptance';
  items.push({key:`task:${task.task_id}`,kind:'task',id:task.task_id,title:task.request?.goal||task.project_id||'提交尚未确认',nodeId:task.target_node_id,category,status,statusLabel:status==='PENDING_APPROVAL'&&outbound?'等待执行端审批':inboxTaskLabels[status]||status,acceptance:status==='SUCCEEDED'?check.state:null,observedAt:outbound?(task._observed||null):inboxLatestTime(task._observed,taskObserved),cached:outbound||!!task._sourceCached,requestId:task._request_id,record:task,needsOwner:category==='approval'});
 }
 for(const session of sessions){
  const remote=!!session._remote,status=session.status;
  const mayAnswer=session.approver!=='auto'&&(!remote||(session.approver==='origin'&&session.origin?.node_id===localNodeId));
  let category='recent',statusLabel=({pending_approval:'等待执行端批准',idle:session.turns?'本轮已结束':'等待第一条消息',running:'正在处理',waiting_permission:'等待工具授权',closed:'已关闭'})[status]||status;
  if(status==='pending_approval'){category=remote?'execution':'approval';if(!remote)statusLabel='本机会话待批准';}
  else if(status==='waiting_permission'){category=mayAnswer?'permission':'execution';statusLabel=mayAnswer?'等待你回答工具授权':session.approver==='auto'?'自动授权处理中':'等待执行端回答工具授权';}
  else if(status==='running')category='execution';
  if(session.last_error&&!['running','waiting_permission','pending_approval'].includes(status))category='attention';
  items.push({key:`session:${session.session_id}`,kind:'session',id:session.session_id,title:session.title||session.agent_name||'Agent 会话',nodeId:session.node_id,category,status,statusLabel,acceptance:null,observedAt:remote?(session._observed||null):inboxLatestTime(session._observed,sessionObserved),cached:remote||!!session._sourceCached,record:session,needsOwner:category==='approval'||category==='permission'});
 }
 return items.sort((a,b)=>Number(b.needsOwner)-Number(a.needsOwner)||((b.record.updated_at||b.record.created_at||0)-(a.record.updated_at||a.record.created_at||0))||a.key.localeCompare(b.key));
}
function inboxRemember(map,id,value){map.delete(id);map.set(id,value);if(map.size>512)map.delete(map.keys().next().value);}
function noteInboxOverviewError(error){inboxState.overviewError=error?(error.code||'OVERVIEW_UNAVAILABLE'):null;renderInbox();}
function inboxCheckInstance(){
 const pid=state.runtime?.daemon?.running?state.runtime.daemon.pid:null;
 if(!pid)return;
 if(inboxState.daemonPid&&inboxState.daemonPid!==pid){
  // A new owner process may have recovered interrupted turns. Old in-memory
  // observations must not override its new list, even within the same second.
  inboxState.taskObservations.clear();inboxState.sessionObservations.clear();inboxState.sessionsObserved=null;
  inboxState.sessionError=inboxState.sessions?'NODE_RESTARTED':null;inboxState.lastRefresh=0;inboxState.epoch++;
 }
 inboxState.daemonPid=pid;
}
function inboxObservationToken(){inboxCheckInstance();return inboxState.epoch;}
function noteInboxSession(record,observedAt=Date.now()/1000,epoch=inboxObservationToken()){
 if(!record?.session_id||epoch!==inboxObservationToken())return;
 inboxRemember(inboxState.sessionObservations,record.session_id,{record,observedAt});inboxState.failures.delete(`session:${record.session_id}`);
 if(typeof renderInbox==='function')renderInbox();
}
function inboxTasks(){
 const reply=state.snapshot?.tasks;
 if(reply?.data){inboxState.taskRows=allTasks();inboxState.taskObserved=state.snapshot.observed_at;inboxState.taskListError=null;}
 else if(reply?.error)inboxState.taskListError=reply.error.code||'TASK_LIST_UNAVAILABLE';
 return inboxState.taskRows.map(row=>{
  const observation=inboxState.taskObservations.get(row.task_id),detail=state.details.get(row.task_id);
  let task={...row,_sourceCached:!!inboxState.taskListError||!!inboxState.overviewError};
  const current=observation&&(observation.record.updated_at||0)>=(task.updated_at||0)&&(!detail||observation.observedAt>=(detail.observed_at||0))&&observation.observedAt>=(row._observed||0);
  if(current)task={...task,...observation.record,_observed:observation.observedAt};
  const response=current&&observation.verification?{observed_at:observation.observedAt,verification:observation.verification}:undefined;
  task._acceptance=acceptance(task,response);return task;
 });
}
function inboxSessions(){
 const rows=inboxState.sessions||{sessions:[],remote:[]};
 return [...(rows.sessions||[]).map(record=>({...record,_remote:false,_sourceCached:!!inboxState.sessionError})),...(rows.remote||[]).map(row=>({...row.session,_remote:true,_peer:row.peer_id}))].map(row=>{
  const observed=inboxState.sessionObservations.get(row.session_id);
  return observed&&(observed.record.updated_at||0)>=(row.updated_at||0)&&observed.observedAt>=(row._observed||0)?{...row,...observed.record,_observed:observed.observedAt}:row;
 });
}
function inboxModel(){
 inboxCheckInstance();
 const tasks=inboxTasks();
 // Unknown POST acknowledgements are held in the existing stable-ID ledger.
 if(typeof pendingDraft!=='undefined'&&pendingDraft&&!tasks.some(t=>t.task_id===pendingDraft.request_id))tasks.push({task_id:pendingDraft.request_id,_request_id:pendingDraft.request_id,_direction:'outbound',target_node_id:pendingDraft.peer_id,project_id:pendingDraft.project_id,request:{goal:pendingDraft.goal,expected_artifacts:pendingDraft.expected_artifacts},status:'UNCONFIRMED'});
 return buildInboxItems({tasks,sessions:inboxSessions(),localNodeId:data('node',{}).node?.node_id,taskObserved:inboxState.taskObserved,sessionObserved:inboxState.sessionsObserved});
}
function inboxMatches(item,category){return category==='all'||(category==='attention'?['attention','approval','permission','acceptance'].includes(item.category):item.category===category);}
async function inboxOpen(item){
 if(item.kind==='task'){await openTask(item.record);return;}
 showView('sessions');selectSession(item.record);
}
function inboxCard(item){
 const card=el('article','inbox-item');card.dataset.key=item.key;card.dataset.kind=item.kind;card.dataset.category=item.category;
 const text=el('div','inbox-item-main'),heading=el('div','inbox-item-heading');heading.append(el('span','inbox-kind',item.kind==='task'?'任务':'ACP 会话'),el('strong','inbox-status',item.statusLabel));
 if(item.acceptance)heading.append(el('span',`badge ${item.acceptance==='passed'?'good':item.acceptance==='failed'?'bad':'warn'}`,inboxAcceptanceNames[item.acceptance]||'验收状态未知'));
 const title=el('p','inbox-title',String(item.title).slice(0,240));
 const observed=el('p','inbox-observed',`${item.cached?'最近已确认 · ':'本机观察 · '}${item.observedAt?fullTime(item.observedAt):'时间未知'}`);if(item.observedAt)observed.dataset.observedAt=item.observedAt;
 text.append(heading,title,el('p','inbox-location',`${nodeName(item.nodeId)} · ${short(item.id)}`),observed);
 const failure=inboxState.failures.get(item.key);if(failure)text.append(el('p','inbox-connection-note',`当前读取失败：${failure}。以上保留最近确认结果，未重新执行。`));
 const actions=el('div','inbox-item-actions'),label=item.kind==='task'?'查看任务与验收':item.category==='permission'?'审阅工具授权':item.category==='approval'?'审阅会话审批':'打开会话';
 actions.append(button(label,'button secondary',()=>inboxOpen(item)));
 if(item.kind==='task'&&item.status==='UNCONFIRMED'&&item.requestId&&typeof recoverOriginal==='function'){
  const recover=button('检查原请求','text-button',()=>recoverOriginal(item.requestId));recover.disabled=!canWrite();actions.append(recover);
 }
 card.append(text,actions);return card;
}
function renderInbox(){
 const box=document.querySelector('#inbox-view');if(!box)return;
 const items=inboxModel(),actionCount=items.filter(item=>item.needsOwner).length,badgeNode=document.querySelector('#nav-inbox-count');
 if(badgeNode){badgeNode.textContent=actionCount;badgeNode.hidden=!actionCount;}
 if(state.view!=='inbox')return;
 const header=el('div','section-heading'),copy=el('div');copy.append(el('h2','','待办与运行状态'),el('p','form-note','审批、执行、验收分别记录。连接失败保留最近确认结果，不重建任务。'));
 const refresh=button(inboxState.loading?'正在读取…':'刷新待办','button secondary',()=>refreshInbox(true));refresh.id='inbox-refresh';refresh.disabled=inboxState.loading;header.append(copy,refresh);
 const filters=el('div','segmented inbox-filters');filters.setAttribute('aria-label','待办类型');
 for(const key of ['attention','approval','permission','execution','acceptance','recent','all']){
  const count=items.filter(item=>inboxMatches(item,key)).length;
  const select=button(`${inboxCategoryNames[key]} · ${count}`,key===inboxState.filter?'selected':'',()=>{inboxState.filter=key;renderInbox();});select.dataset.inboxFilter=key;filters.append(select);
 }
 const problems=el('section','inbox-connectivity');problems.id='inbox-connectivity';problems.setAttribute('aria-label','连接与读取状态');
 const notes=[];if(inboxState.overviewError)notes.push(`总览读取失败：${inboxState.overviewError}`);if(inboxState.taskListError)notes.push(`任务列表读取失败：${inboxState.taskListError}`);if(inboxState.sessionError)notes.push(`会话列表读取失败：${inboxState.sessionError}`);
 for(const [key,code]of inboxState.failures)notes.push(`${key.startsWith('task:')?'任务':'会话'} ${short(key.split(':')[1])}：${code}`);
 if(state.snapshot?.peers?.error)notes.push(`节点探测不可用：${state.snapshot.peers.error.code}`);
 for(const peer of data('peers'))if(peer.node_connectivity==='unreachable')notes.push(`${peer.card.display_name} 当前不可达；历史任务结果保持不变。`);
 if(notes.length){problems.append(el('h3','','当前连接与读取问题'),...notes.slice(0,12).map(note=>el('p','',note)),el('p','form-note','这里的读取问题不代表任务执行失败。可打开原记录核对；不会自动重提。'));}else problems.hidden=true;
 const shown=items.filter(item=>inboxMatches(item,inboxState.filter));
 const list=el('div','inbox-list');list.id='inbox-items';list.append(...(shown.length?shown.slice(0,100).map(inboxCard):[empty('当前分类没有待办','可以切换分类查看执行、验收和最近结果。')]));
 const foot=el('p','list-note',`显示 ${Math.min(shown.length,100)} / ${shown.length} 条。远端记录标注最后确认时间；未观察过的缓存时间保持未知。`);
 box.replaceChildren(header,filters,problems,list,foot);
}
function inboxCandidates(items){return items.filter(item=>item.kind==='task'?item.record._direction==='outbound'&&(inboxState.failures.has(item.key)||['UNCONFIRMED','PENDING_APPROVAL','QUEUED','RUNNING','CANCELLING','WAITING_INPUT','UNKNOWN'].includes(item.status)||(item.status==='SUCCEEDED'&&['pending','unknown'].includes(item.acceptance))):item.record._remote&&(item.status!=='closed'||inboxState.failures.has(item.key)));}
async function observeInboxItem(item,epoch){
 try{
  if(item.kind==='task'){
   const reply=unwrap(await api(`/tasks/${item.id}/observation`)),observedAt=reply.observed_at||Date.now()/1000;
   if(epoch!==inboxObservationToken())return;
   inboxRemember(inboxState.taskObservations,item.id,{record:reply.task,verification:reply.verification,observedAt});
   if(reply.verification?.error){inboxState.failures.set(item.key,reply.verification.error.code||'VERIFICATION_UNAVAILABLE');return;}
  }else{
   const reply=unwrap(await api(`/sessions/${item.id}/events?after=0&wait=0&status_only=true`));if(epoch!==inboxObservationToken())return;noteInboxSession(reply.session,Date.now()/1000,epoch);
  }
  inboxState.failures.delete(item.key);
 }catch(error){if(epoch===inboxObservationToken())inboxState.failures.set(item.key,error.code||'READ_UNAVAILABLE');}
}
async function refreshInbox(force=false){
 inboxCheckInstance();
 if(inboxState.loading||!access||(!force&&Date.now()-inboxState.lastRefresh<7500))return;
 const epoch=inboxState.epoch;inboxState.lastRefresh=Date.now();inboxState.loading=true;renderInbox();
 try{
  if(state.runtime?.capabilities?.sessions){
   try{const reply=await api('/sessions/summary');if(epoch!==inboxObservationToken())return;inboxState.sessions=unwrap(reply.sessions);inboxState.sessionsObserved=reply.observed_at;inboxState.sessionError=null;}
   catch(error){if(epoch!==inboxObservationToken())return;inboxState.sessionError=error.code||'SESSIONS_UNAVAILABLE';}
  }else{inboxState.sessions=null;inboxState.sessionError=null;}
  const candidates=inboxCandidates(inboxModel()),length=candidates.length;
  if(length){const selected=Array.from({length:Math.min(8,length)},(_,i)=>candidates[(inboxState.cursor+i)%length]);inboxState.cursor=(inboxState.cursor+selected.length)%length;
   let next=0;await Promise.all([0,1].map(async()=>{while(next<selected.length&&epoch===inboxObservationToken()){const item=selected[next++];await observeInboxItem(item,epoch);}}));
  }
 }finally{inboxState.loading=false;renderInbox();}
}
if(typeof window!=='undefined')setInterval(()=>{if(!document.hidden&&state.view==='inbox'&&!state.stopped)refreshInbox();},10000);
