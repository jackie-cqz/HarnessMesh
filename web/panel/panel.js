'use strict';
// All daemon and peer text is rendered with textContent, including tool payloads.
const $ = selector => document.querySelector(selector);
function el(tag, cls, text) { const node=document.createElement(tag); if(cls)node.className=cls; if(text!==undefined)node.textContent=String(text); return node; }
function button(text, cls, action) { const b=el('button',cls,text); b.addEventListener('click',action); return b; }
function empty(title, description='') { const node=el('div','empty'); node.append(el('div','empty-symbol','⊹'),el('strong','',title),el('span','',description)); return node; }
const labels={SUCCEEDED:'已完成',FAILED:'失败',PENDING_APPROVAL:'待审批',QUEUED:'已排队',RUNNING:'执行中',CANCELLING:'取消中',WAITING_INPUT:'等待输入',CANCELLED:'已取消',REJECTED:'已拒绝',EXPIRED:'已过期',UNKNOWN:'结果不确定',UNCONFIRMED:'待确认',idle:'可用',busy:'忙碌',unverified:'待验证',disabled:'已停用',retired:'已退役',active:'有效',stopped:'已停止',expired:'已过期',online:'在线',unreachable:'不可达',not_probed:'未探测',trusted:'已信任',revoked:'已撤销',failed_needs_input:'需要处理',healthy:'健康',running:'运行中',external:'外部进程'};
const active=new Set(['QUEUED','RUNNING','CANCELLING','WAITING_INPUT']);
const terminal=new Set(['SUCCEEDED','FAILED','CANCELLED','REJECTED','EXPIRED','UNKNOWN']);
function badge(value) { let tone=''; if(['SUCCEEDED','idle','healthy','online','active'].includes(value))tone='good'; else if(['PENDING_APPROVAL','WAITING_INPUT','unverified','UNCONFIRMED'].includes(value))tone='warn'; else if(['FAILED','UNKNOWN','unreachable','failed_needs_input'].includes(value))tone='bad'; else if(active.has(value)||value==='busy')tone='live'; return el('span',`badge ${tone}`,labels[value]||value||'未知'); }
const short=value=>value?String(value).slice(0,8):'—';
const time=value=>value?new Date(value*1000).toLocaleString('zh-CN',{month:'2-digit',day:'2-digit',hour:'2-digit',minute:'2-digit',hour12:false}):'—';
const fullTime=value=>value?new Date(value*1000).toLocaleString('zh-CN',{hour12:false}):'—';
const state={snapshot:null,view:'overview',filter:'all',agentFilter:'active',refreshing:false,selected:null,cursor:0,events:0,eventsLoading:false,detailLoading:false,detailVersion:0,actionBusy:false};
let access='';
try { access=new URLSearchParams(location.hash.slice(1)).get('access')||sessionStorage.getItem('hmesh.panel.access')||''; if(access)sessionStorage.setItem('hmesh.panel.access',access); } catch { access=new URLSearchParams(location.hash.slice(1)).get('access')||''; }
if(location.hash)history.replaceState(null,'',location.pathname);
function gate() { $('#access-gate').hidden=false; $('#app').hidden=true; $('#sync-state').textContent='需要访问链接'; }
async function api(path, options={}) {
  const response=await fetch('/api'+path,{...options,credentials:'omit',redirect:'error',headers:{Authorization:`Bearer ${access}`,...(options.body?{'content-type':'application/json'}:{})},signal:AbortSignal.timeout(20000)});
  if(response.status===401){gate();throw Error('访问令牌已失效，请从启动终端重新打开链接。');}
  const body=await response.json();
  if(!response.ok)throw Error(body.error?.code||`HTTP ${response.status}`);
  return body;
}
function unwrap(r) { if(r?.error)throw Error(r.error.code); return r?.data; }
function data(key,fallback=[]) { return state.snapshot?.[key]?.data??fallback; }
function allTasks() {
  const rows=data('tasks',{}), tasks=new Map();
  for(const t of rows.submitted||[])tasks.set(t.task_id,{...t.last_observed,task_id:t.task_id,_direction:'outbound',status:t.last_observed?.status||'UNCONFIRMED'});
  for(const t of rows.executions||[])tasks.set(t.task_id,{...t,_direction:'local'});
  return [...tasks.values()].sort((a,b)=>(b.created_at||0)-(a.created_at||0));
}
let toastTimer;
function toast(message) { clearTimeout(toastTimer); $('#toast').textContent=message; $('#toast').hidden=false; toastTimer=setTimeout(()=>$('#toast').hidden=true,6500); }
function showView(name) {
  const names={overview:['运行总览','节点、任务与执行状态，尽在一处。'],tasks:['任务','查看执行记录，逐项处理审批与异常。'],agents:['Agents','已登记的 Harness，以及它们的验证状态。'],services:['服务与连接','观察受管进程、服务租约和转发连接。'],nodes:['配对节点','连接可达性与项目授权分别呈现。']};
  if(!names[name])return;
  state.view=name;
  for(const b of document.querySelectorAll('[data-view]')){b.classList.toggle('selected',b.dataset.view===name);b.setAttribute('aria-current',b.dataset.view===name?'page':'false');}
  for(const view of document.querySelectorAll('.view'))view.hidden=view.id!==`${name}-view`;
  $('#page-title').textContent=names[name][0];$('#breadcrumb').textContent=names[name][0];$('#page-description').textContent=names[name][1];
  $('#metrics').hidden=name!=='overview';render();
}
function renderMetrics() {
  const rows=allTasks(), localTasks=rows.filter(t=>t._direction==='local'), agents=data('agents'), peers=data('peers');
  const pending=localTasks.filter(t=>t.status==='PENDING_APPROVAL').length;
  const metrics=[['在线节点',state.snapshot?.node?.error||state.snapshot?.peers?.error?'—':1+peers.filter(p=>p.node_connectivity==='online').length,'包含本机与已探测配对节点','01'],['本机 Agent',state.snapshot?.agents?.error?'—':agents.length,`${agents.filter(a=>a.probe?.managed_run==='passed').length} 个已通过模型验证`,'02'],['待审批任务',state.snapshot?.tasks?.error?'—':pending,'仅本机接收的待审批任务','03'],['本机进行中',state.snapshot?.tasks?.error?'—':localTasks.filter(t=>active.has(t.status)).length,'执行、排队与等待输入','04']];
  $('#metrics').replaceChildren(...metrics.map(([label,value,note,n],i)=>{const card=el('div',`metric ${i===2&&pending?'accent':''}`),title=el('div','metric-label',label);title.append(el('span','',n));card.append(title,el('div',`metric-number ${i===2&&pending?'emphasis':''}`,value),el('small','',note));return card;}));
  $('#nav-pending').textContent=pending;$('#nav-pending').hidden=!pending;
  $('#approval-title').textContent=pending?`${pending} 个任务，等待你的决定。`:'每一次执行，都有边界。';
  $('#review-pending').textContent=pending?'审阅待审批任务 ↗':'查看任务记录 ↗';
}
function table(tasks) {
  if(!tasks.length)return empty('这里暂时没有任务','任务开始后，执行状态和事件会显示在这里。');
  const t=el('table','task-table'),head=el('thead'),tr=el('tr');for(const text of ['任务 / 项目','状态','创建时间'])tr.append(el('th','',text));head.append(tr);const body=el('tbody');
  for(const task of tasks){const row=el('tr'),title=el('td');const b=button(task.request?.goal||'提交结果尚未确认','task-title',()=>openTask(task));b.title=task.request?.goal||task.task_id;title.append(b,el('div','task-sub',`${short(task.task_id)} · ${task.project_id||'—'} · ${task._direction==='local'?'本机执行':'发出 · 缓存'}`));const status=el('td');status.append(badge(task.status));row.append(title,status,el('td','task-time',time(task.created_at)));body.append(row);}t.append(head,body);return t;
}
function renderTasks() {
  const query=$('#task-search').value.trim().toLowerCase();
  const tasks=allTasks().filter(t=>{
    const matches=state.filter==='all'||(state.filter==='pending'&&t._direction==='local'&&t.status==='PENDING_APPROVAL')||(state.filter==='active'&&active.has(t.status))||(state.filter==='attention'&&['FAILED','UNKNOWN','WAITING_INPUT','UNCONFIRMED'].includes(t.status));
    return matches&&(!query||[t.task_id,t.project_id,t.request?.goal,t.agent_id].some(v=>String(v||'').toLowerCase().includes(query)));
  });
  $('#task-list').replaceChildren(table(tasks));
  for(const b of document.querySelectorAll('[data-filter]'))b.classList.toggle('selected',b.dataset.filter===state.filter);
}
function meta(pairs) { const dl=el('dl','entity-meta');for(const [key,value]of pairs)dl.append(el('dt','',key),el('dd','',value??'—'));return dl; }
function agentCard(agent,remote=false) {
  const card=el('article','entity-card'),top=el('div','entity-top'),title=el('div');title.append(el('h3','entity-name',agent.name),el('div','entity-harness',String(agent.harness||'').toUpperCase()));top.append(title,badge(agent.availability));
  card.append(top,meta([['项目',agent.project_ids?.join(' / ')],['模型验证',agent.probe?.managed_run==='passed'?'已通过':agent.probe?.managed_run||'未验证'],['Harness 版本',agent.probe?.version||'—']]),el('div','entity-id',agent.agent_id));
  if(!remote){const actions=el('div','entity-actions'),retired=state.agentFilter==='retired';actions.append(button(retired?'恢复 Agent':'退役 Agent','button secondary',()=>act('agents',agent.agent_id,retired?'restore':'retire',retired?'恢复 Agent？':'退役 Agent？',`${agent.name}\n${retired?'恢复后需通过 CLI 显式重新验证模型；不会自动调用模型。':'有未结束任务时服务端会拒绝退役。历史任务会保留。'}`)));card.append(actions);}return card;
}
function renderAgents() {
  const retired=state.agentFilter==='retired',agents=data(retired?'retired':'agents');$('#agent-list').replaceChildren(...(agents.length?agents.map(a=>agentCard(a)):[empty(retired?'没有已退役 Agent':'还没有登记 Agent',retired?'退役记录会保留在这里。':'使用 hmesh agent register 登记现有 Harness。')]));
  for(const b of document.querySelectorAll('[data-agent-filter]'))b.classList.toggle('selected',b.dataset.agentFilter===state.agentFilter);
}
function isActive(row) { return row.enabled&&row.expires_at>Date.now()/1000; }
function renderServices() {
  const rows=data('services').filter(s=>$('#show-inactive').checked||isActive(s));
  $('#service-list').replaceChildren(...(rows.length?rows.map(s=>{
    const card=el('article','entity-card'),top=el('div','entity-top');top.append(el('h3','entity-name',s.name||short(s.id)),badge(s.status?.state));
    card.append(top,meta([['项目',s.project_id],['进程',`${labels[s.status?.process]||s.status?.process||'—'}${s.status?.pid?` · PID ${s.status.pid}`:''}`],['HTTP 健康',labels[s.status?.http_health]||s.status?.http_health||'—'],['Revision / 启动',`${s.revision} / ${s.status?.starts??0}`],['租约截止',fullTime(s.expires_at)]]),el('div','entity-id',s.id));
    const actions=el('div','entity-actions'),restart=button('重启服务','button secondary',()=>act('services',s.id,'restart','重启这个服务？',`${s.name}\n将停止并重启该服务的受管进程。`));restart.disabled=!isActive(s)||!s.managed;const revoke=button('撤销服务','button secondary',()=>act('services',s.id,'revoke','撤销这个服务？',`${s.name}\n将关闭服务访问，并停止它的受管进程。`));revoke.disabled=!isActive(s);actions.append(restart,revoke);card.append(actions);return card;
  }):[empty('当前没有有效服务','可显示历史记录，或通过 CLI 发布受管服务。')]));
  const connections=[...data('connections').map(r=>({...r,_kind:'连接'})),...data('tunnels').map(r=>({...r,_kind:'隧道'}))].filter(s=>$('#show-inactive').checked||isActive(s));
  $('#connection-list').replaceChildren(...(connections.length?connections.map(c=>{const row=el('div','connection-row'),name=el('div');name.append(el('strong','',c.name||`${c._kind} ${short(c.id)}`),el('div','mono',c.id));row.append(name,el('div','mono',c.status?.endpoint||c.project_id||c._kind),badge(c.status?.state));return row;}):[empty('没有有效的转发连接或隧道')]));
}
function renderMesh() {
  const node=data('node',{}).node,peers=data('peers');const nodes=[];
  if(node)nodes.push({name:node.display_name,subtitle:'THIS NODE · 本机',online:true,local:true});
  for(const p of peers)nodes.push({name:p.card.display_name,subtitle:p.card.endpoint,online:p.node_connectivity==='online'});
  $('#mesh-nodes').replaceChildren(...(nodes.length?nodes.map(n=>{const r=el('div','mesh-node'),info=el('div','mesh-info');info.append(el('strong','',n.name),el('small','',n.local?n.subtitle:(n.online?'mTLS · 可达':'暂不可达')));const icon=el('span','mesh-icon',n.local?'M':'↗');r.append(icon,info,el('i',`dot ${n.online?'':'offline'}`));return r;}):[empty('等待读取节点')]));
}
function renderNodes() {
  const peers=data('peers');$('#node-list').replaceChildren(...(peers.length?peers.map(p=>{const card=el('article','entity-card'),top=el('div','entity-top');top.append(el('h3','entity-name',p.card.display_name),badge(p.node_connectivity));card.append(top,meta([['信任状态',labels[p.status]||p.status],['地址',p.card.endpoint],['可见 / 可运行',`${p.visible_agent_count??'—'} / ${p.runnable_agent_count??'—'}`],['Agent 注册表',p.registry_readable?'可读取':'未授权或不可达']]),el('div','entity-id',p.card.node_id));const actions=el('div','entity-actions'),b=button('查看远端 Agent','button secondary',()=>loadRemote(p));b.disabled=p.status!=='trusted';actions.append(b);card.append(actions);return card;}):[empty('还没有配对节点','通过 CLI 核对指纹并完成双端配对。')]));
}
async function loadRemote(peer) {
  const box=$('#remote-agents');box.hidden=false;box.replaceChildren(el('h2','',`${peer.card.display_name} / Agents`),empty('正在读取授权的 Agent…'));
  try { const rows=unwrap(await api(`/peers/${peer.card.node_id}/agents`));const grid=el('div','card-grid');grid.append(...(rows.length?rows.map(a=>agentCard(a,true)):[empty('该节点没有对本机可见的 Agent')]));box.replaceChildren(el('h2','',`${peer.card.display_name} / Agents`),grid); }
  catch(e){box.replaceChildren(el('h2','',`${peer.card.display_name} / Agents`),empty('远端 Agent 暂不可读取',e.message));}
}
function render() {
  if(!state.snapshot)return;
  renderMetrics();renderMesh();$('#recent-tasks').replaceChildren(table(allTasks().slice(0,6)));
  if(state.view==='tasks')renderTasks();if(state.view==='agents')renderAgents();if(state.view==='services')renderServices();if(state.view==='nodes')renderNodes();
}
async function refresh() {
  if(state.refreshing||!access)return;state.refreshing=true;$('#refresh').disabled=true;$('#sync-state').textContent='同步中…';
  try {
    state.snapshot=await api('/overview');const node=data('node',{});$('#access-gate').hidden=true;$('#app').hidden=false;
    $('#local-name').textContent=node.node?.display_name||'Daemon 不可读取';$('#local-network').textContent=node.network==='tailscale'?'TAILNET / OWNER LOCAL':node.network?'LOOPBACK / OWNER LOCAL':'RPC UNAVAILABLE';
    const names={node:'本机 Daemon',peers:'配对节点',agents:'Agent',retired:'退役 Agent',tasks:'任务',services:'服务',connections:'连接',tunnels:'隧道'};
    const failures=Object.entries(names).filter(([key])=>state.snapshot[key]?.error).map(([key,name])=>`${name}: ${state.snapshot[key].error.code}`);
    $('#alerts').replaceChildren(...(failures.length?[el('div','alert',`部分状态暂不可读取。${failures.join('；')}`)]:[]));
    $('#sync-state').textContent=failures.length?'部分数据不可用':'已同步 · 10 秒刷新';$('#last-observed').textContent=`最后观察 ${fullTime(state.snapshot.observed_at)}`;render();
  }catch(e){$('#sync-state').textContent='连接中断';$('#alerts').replaceChildren(el('div','alert',`读取失败：${e.message}。页面可能保留上次观察结果，未自动重试任何操作。`));}
  finally{state.refreshing=false;$('#refresh').disabled=false;}
}
function confirmAction(title,description) {
  return new Promise(resolve=>{const dialog=$('#confirm-dialog');$('#confirm-title').textContent=title;$('#confirm-description').textContent=description;let accepted=false;const yes=()=>{accepted=true;dialog.close();};const no=()=>dialog.close();const done=()=>{$('#confirm-accept').removeEventListener('click',yes);$('#confirm-cancel').removeEventListener('click',no);resolve(accepted);};$('#confirm-accept').addEventListener('click',yes);$('#confirm-cancel').addEventListener('click',no);dialog.addEventListener('close',done,{once:true});dialog.showModal();$('#confirm-cancel').focus();});
}
async function act(kind,id,name,title,description) {
  if(state.actionBusy)return;state.actionBusy=true;
  try {
    if(!await confirmAction(title,description))return;
    await api(`/${kind}/${id}/${name}`,{method:'POST',body:'{}'});
    toast('操作已提交，正在读取最新状态。');await refresh();if(state.selected&&$('#task-dialog').open)await readDetail(state.detailVersion);
  } catch(e){toast(`操作未确认：${e.message}。先刷新检查状态，请勿重复提交。`);}
  finally{state.actionBusy=false;}
}
async function openTask(task) {
  state.selected=task;state.cursor=0;state.events=0;const version=++state.detailVersion;
  state.lastDetail='';
  $('#task-detail').replaceChildren(empty('正在读取任务…'));$('#event-list').replaceChildren();$('#event-count').textContent='0 events';$('#load-events').disabled=false;$('#load-events').textContent='加载后续事件';
  if(!$('#task-dialog').open)$('#task-dialog').showModal();
  await Promise.all([readDetail(version),loadEvents(version)]);
}
async function readDetail(version) {
  if(state.detailLoading===version)return;state.detailLoading=version;
  const selected=state.selected;
  try {
    const response=await api(`/tasks/${selected.task_id}`);const task=unwrap(response.task);if(version!==state.detailVersion)return;
    state.selected={...task,_direction:selected._direction};
    const detailKey=JSON.stringify(response);if(state.lastDetail===detailKey)return;state.lastDetail=detailKey;
    const title=el('div','detail-id',task.task_id),status=badge(task.status),grid=el('dl','detail-grid');
    const agent=data('agents').find(a=>a.agent_id===task.agent_id)||data('retired').find(a=>a.agent_id===task.agent_id);
    const verification=response.verification?.data,verified=Array.isArray(verification)?verification.filter(v=>v.passed===true).length:0;
    for(const [k,v]of [['项目',task.project_id],['Agent',agent?.name||task.agent_id],['方向',selected._direction==='local'?'本机执行':'发往远端 · 本次实时查询'],['Attempt',task.last_attempt_id||'尚未创建'],['创建时间',fullTime(task.created_at)],['限额',`${task.request?.limits?.timeout_seconds??'—'} 秒 / ${(task.request?.limits?.max_output_bytes/1048576).toFixed(0)} MiB 输出`],['工件核验',response.verification?.error?'暂不可读取':verified?`${verified} 条核验通过`:'尚无通过记录']])grid.append(el('dt','',k),el('dd','',v));
    const goal=el('pre','goal',task.request?.goal||'—'),actions=el('div','detail-actions');
    if(selected._direction==='local'&&task.status==='PENDING_APPROVAL'){
      actions.append(button('批准执行','button',()=>act('tasks',task.task_id,'approve','批准执行这个任务？',`项目：${task.project_id}\nAgent：${agent?.name||task.agent_id}\n批准后可能调用模型并产生费用。\n\n${task.request.goal}`)),button('拒绝任务','button secondary',()=>act('tasks',task.task_id,'reject','拒绝这个任务？','拒绝后该任务不会执行，需要新的请求才能再次提交。')));
    }
    if(!terminal.has(task.status)&&task.status!=='UNCONFIRMED')actions.append(button('取消任务','button secondary',()=>act('tasks',task.task_id,'cancel','请求取消任务？','已发生的副作用不会撤销。仅在进程清理得到确认后，任务才会标记为已取消。')));
    if(selected._direction==='local'&&terminal.has(task.status))actions.append(button('归档事件','button secondary',()=>act('tasks',task.task_id,'archive','归档任务事件？','事件仍可完整回放；这是同库逻辑归档，不删除历史，也不缩减数据库文件。')));
    const children=[status,title,grid,el('h3','','任务请求'),goal,actions];if(task.error)children.push(el('div','alert',`${task.error.code}: ${task.error.message}`));$('#task-detail').replaceChildren(...children);
  }catch(e){if(version===state.detailVersion)$('#task-detail').replaceChildren(empty('任务暂不可读取',e.message));}
  finally{if(state.detailLoading===version)state.detailLoading=false;}
}
async function loadEvents(version=state.detailVersion) {
  if(state.eventsLoading===version||!state.selected||state.events>=2000)return;state.eventsLoading=version;$('#load-events').disabled=true;
  try {
    const rows=unwrap(await api(`/tasks/${state.selected.task_id}/events?after=${state.cursor}`));if(version!==state.detailVersion)return;
    if(!Array.isArray(rows))throw Error('INVALID_EVENT_RESPONSE');
    for(const event of rows){if(state.events>=2000)break;if(event.seq<=state.cursor)throw Error('INVALID_EVENT_CURSOR');const row=el('div','event'),head=el('div','event-header');head.append(el('span','',`#${event.seq}`),el('strong','',event.event_type),el('span','',time(event.created_at)));row.append(head,el('pre','',JSON.stringify(event.payload,null,2)));$('#event-list').append(row);state.cursor=event.seq;state.events++;}
    $('#event-count').textContent=`${state.events} events · seq ${state.cursor}`;
    $('#load-events').textContent=state.events>=2000?'已显示 2000 条，完整回放请使用 CLI':rows.length?'继续加载后续事件':'已读到当前末尾 · 再次检查';
  }catch(e){if(version===state.detailVersion)toast(`读取事件失败：${e.message}`);}
  finally{if(state.eventsLoading===version)state.eventsLoading=false;if(version===state.detailVersion)$('#load-events').disabled=state.events>=2000;}
}
for(const b of document.querySelectorAll('[data-view]'))b.addEventListener('click',()=>showView(b.dataset.view));
for(const b of document.querySelectorAll('[data-go]'))b.addEventListener('click',()=>showView(b.dataset.go));
for(const b of document.querySelectorAll('[data-filter]'))b.addEventListener('click',()=>{state.filter=b.dataset.filter;renderTasks();});
for(const b of document.querySelectorAll('[data-agent-filter]'))b.addEventListener('click',()=>{state.agentFilter=b.dataset.agentFilter;renderAgents();});
for(const b of document.querySelectorAll('[data-close]'))b.addEventListener('click',()=>$('#'+b.dataset.close).close());
$('.brand').addEventListener('click',e=>{e.preventDefault();showView('overview');});
$('#review-pending').addEventListener('click',()=>{state.filter=allTasks().some(t=>t._direction==='local'&&t.status==='PENDING_APPROVAL')?'pending':'all';showView('tasks');});
$('#task-search').addEventListener('input',renderTasks);$('#show-inactive').addEventListener('change',renderServices);
$('#load-events').addEventListener('click',()=>loadEvents());$('#refresh').addEventListener('click',refresh);
$('#task-dialog').addEventListener('close',()=>{state.selected=null;state.detailVersion++;});
$('#clear-access').addEventListener('click',()=>{try{sessionStorage.removeItem('hmesh.panel.access');}catch{}access='';toast('已清除，请重新打开终端中的访问链接。');});
if(access)refresh();else gate();
setInterval(()=>{if(!document.hidden)refresh();},10000);
setInterval(()=>{if(!document.hidden&&$('#task-dialog').open&&state.selected&&!terminal.has(state.selected.status)){readDetail(state.detailVersion);loadEvents();}},2500);
