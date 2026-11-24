'use strict';
// Agent sessions in codeg's layout: sessions and agents in the app sidebar,
// a centered conversation with the composer at the bottom, and a new-session
// screen with an agent picker. Agent and peer text is always set as text.
const sessionLabels={pending_approval:'等待批准',idle:'空闲',running:'进行中',waiting_permission:'等待授权',closed:'已关闭'};
const sessionTone={pending_approval:'warn',idle:'good',running:'live',waiting_permission:'warn',closed:''};
const conv={list:null,nodes:[],selected:null,cursor:0,events:[],record:null,remote:false,generation:0,busy:false,listLoading:false,target:null,openTools:new Set(),openThoughts:new Set()};

// Daemon notes are English; show them in the panel's language (null hides them).
const sessionNotes=[
  [/^opened /,null],
  [/^approved on this computer/,'已批准，开始执行'],
  [/^rejected on this computer/,'已拒绝'],
  [/^closed by its owner/,'会话已关闭'],
  [/^closed by the computer that opened it/,'会话已关闭'],
  [/^pairing was revoked/,'配对已撤销，会话关闭'],
  [/^the request was withdrawn/,'请求在批准前撤回'],
  [/^the agent reloaded this conversation/,'Agent 重新载入了之前的对话'],
  [/^the agent could not reload/,'Agent 没能载入之前的对话，接着对话但不带之前的上下文'],
  [/^this agent cannot reload/,'这个 Agent 不支持载入之前的对话，接着对话但不带之前的上下文'],
  [/^cancel requested/,'已请求停止'],
  [/^the turn was interrupted because HarnessMesh restarted/,'HarnessMesh 重启，这一轮被中断'],
];
function noteText(message){for(const [pattern,text]of sessionNotes)if(pattern.test(message||''))return text;return message;}
function icon(name,size=''){const svg=document.createElementNS('http://www.w3.org/2000/svg','svg');svg.setAttribute('class',`icon ${size}`.trim());svg.setAttribute('aria-hidden','true');const use=document.createElementNS('http://www.w3.org/2000/svg','use');use.setAttribute('href',`#i-${name}`);svg.append(use);return svg;}
function sessionBadge(status){return el('span',`badge ${sessionTone[status]||''}`,sessionLabels[status]||status||'未知');}
function localNodeId(){return data('node',{}).node?.node_id;}
function nodeLabel(id){const n=conv.nodes.find(n=>n.node_id===id);if(n)return n.local?`${n.name}（本机）`:n.name;return nodeName(id);}
function shortNode(id){const n=conv.nodes.find(n=>n.node_id===id);return n?n.name:short(id);}
function ago(seconds){const d=Math.max(0,Date.now()/1000-(seconds||0));if(d<60)return '刚刚';if(d<3600)return `${Math.floor(d/60)}分钟`;if(d<86400)return `${Math.floor(d/3600)}小时`;return `${Math.floor(d/86400)}天`;}
function allSessions(){
  const list=conv.list||{sessions:[],remote:[]};
  const rows=[...(list.sessions||[]).map(s=>({...s,_remote:false})),...(list.remote||[]).map(r=>({...r.session,_remote:true,_peer:r.peer_id}))];
  return rows.sort((a,b)=>(b.updated_at||0)-(a.updated_at||0));
}
function originText(s){
  const me=localNodeId();
  if(s.origin?.kind==='agent')return `由 ${s.origin.label||nodeLabel(s.origin.node_id)} 调用`;
  if(s.origin?.node_id===me)return '你打开的';
  return `由 ${nodeLabel(s.origin?.node_id)} 打开`;
}
function approverText(s){
  if(s.approver==='auto')return '工具权限自动允许';
  if(s.approver==='local')return s._remote?`工具权限由 ${nodeLabel(s.node_id)} 的主人回答`:'工具权限由你回答';
  return s.origin?.node_id===localNodeId()?'工具权限由你回答':`工具权限由 ${nodeLabel(s.origin?.node_id)} 回答`;
}
function mayAnswer(s){
  if(s.approver==='auto')return false;
  if(s._remote)return s.approver==='origin';
  return true;
}
function needsOwner(s){return (s.status==='pending_approval'&&!s._remote)||(s.status==='waiting_permission'&&mayAnswer(s));}

async function loadSessions(){
  if(conv.listLoading||!access)return;conv.listLoading=true;
  try{
    const reply=await api('/sessions');
    if(reply.sessions?.error)throw Object.assign(Error(reply.sessions.error.code),reply.sessions.error);
    conv.list=reply.sessions.data;conv.nodes=reply.nodes?.data||conv.nodes;
    // The open conversation may know a newer state than this list snapshot.
    if(conv.record)syncListRow(conv.record,false);
    renderSessionList();
    if(state.view==='sessions'&&!conv.selected)renderHero();
  }catch(e){$('#session-list').replaceChildren(el('div','side-empty',`会话暂不可读取：${e.message}`));}
  finally{conv.listLoading=false;}
}
function renderSessions(){if(!conv.list)loadSessions();else renderSessionList();}
// Called when the sessions view becomes visible.
function sessionsShown(){loadSessions();renderConversation();if(conv.selected){conv.generation++;follow(conv.generation);}}
function syncListRow(record,render=true){
  if(!conv.list)return;
  const local=(conv.list.sessions||[]).find(s=>s.session_id===record.session_id);
  const remote=(conv.list.remote||[]).find(r=>r.session.session_id===record.session_id);
  const target=local||remote?.session;if(!target)return;
  if((target.updated_at||0)>(record.updated_at||0))return;
  if(target.status===record.status&&target.turns===record.turns&&target.title===record.title)return;
  Object.assign(target,{status:record.status,turns:record.turns,title:record.title,updated_at:record.updated_at});
  if(render)renderSessionList();
}

// ---- Sidebar: sessions grouped by computer, then the agents ----
function stateClass(status){return {running:'running',waiting_permission:'waiting',pending_approval:'pending',idle:'idle',closed:'closed'}[status]||'idle';}
function sessionRow(s,child){
  const row=el('button',`session-row${child?' child':''}${s.session_id===conv.selected?' selected':''}${s.status==='closed'?' closed':''}`);row.type='button';
  const dot=el('span',`state-dot ${stateClass(s.status)}`);dot.title=sessionLabels[s.status]||s.status;
  row.append(dot);if(child)row.prepend(icon('corner-down-right','xs'));
  row.append(el('span','session-row-title',s.title||`${s.agent_name} · 新会话`),el('span','session-row-time',ago(s.updated_at)));
  row.title=`${s.agent_name} · ${sessionLabels[s.status]||s.status} · ${originText(s)}`;
  row.addEventListener('click',()=>{if(state.view!=='sessions')showView('sessions');selectSession(s);});
  return row;
}
function renderSessionList(){
  renderAgentDirectory();
  const rows=allSessions(),waiting=rows.filter(needsOwner).length;
  $('#nav-sessions').hidden=!waiting;$('#nav-sessions').textContent=waiting;
  if(!rows.length){$('#session-list').replaceChildren(el('div','side-empty','还没有会话'));return;}
  const ids=new Set(rows.map(r=>r.session_id)),groups=new Map();
  for(const s of rows){if(!groups.has(s.node_id))groups.set(s.node_id,[]);groups.get(s.node_id).push(s);}
  const order=[...groups.keys()].sort((a,b)=>(a===localNodeId()?-1:b===localNodeId()?1:0));
  $('#session-list').replaceChildren(...order.map(node=>{
    const group=el('div','session-group'),head=el('div','session-group-name');head.append(icon('monitor','xs'),el('span','',nodeLabel(node)));group.append(head);
    const items=groups.get(node),inGroup=new Set(items.map(s=>s.session_id));
    // A delegated session sits under its caller when both are in this group.
    const parentOf=s=>s.origin?.chain?.at(-1)?.session_id;
    const roots=items.filter(s=>!(parentOf(s)&&inGroup.has(parentOf(s))));
    const place=(s,depth)=>{group.append(sessionRow(s,depth>0));for(const c of items.filter(c=>parentOf(c)===s.session_id&&ids.has(c.session_id)))place(c,depth+1);};
    for(const s of roots)place(s,0);
    return group;
  }));
}
function renderAgentDirectory(){
  const box=$('#session-agents');if(!box)return;
  const groups=conv.nodes.map(n=>{
    const group=el('div','session-group'),head=el('div','session-group-name');head.append(icon('monitor','xs'),el('span','',n.local?`${n.name}（本机）`:`${n.name}${n.reachable?'':' · 连不上'}`));group.append(head);
    const agents=(n.agents||[]).filter(a=>a.availability!=='disabled');
    if(!agents.length)group.append(el('div','side-empty','没有可用 Agent'));
    for(const a of agents){
      const ready=a.probe?.version_supported&&n.reachable,row=el('button','agent-row');row.type='button';row.disabled=!ready||!canWrite();
      row.append(icon('bot','sm'),el('span','agent-row-name',a.name),el('span','agent-row-kind',String(a.harness||'')));
      row.title=ready?'和这个 Agent 开新会话':'这个 Agent 现在不能用';
      row.addEventListener('click',()=>startNew(n.node_id,a.agent_id));group.append(row);
    }
    return group;
  });
  box.replaceChildren(...(groups.length?groups:[el('div','side-empty','还没有 Agent')]));
}
for(const label of document.querySelectorAll('.side-label'))label.addEventListener('click',()=>{const open=label.getAttribute('aria-expanded')!=='true';label.setAttribute('aria-expanded',String(open));$('#'+label.getAttribute('aria-controls')).hidden=!open;});

// ---- New session ----
function readyAgents(){return conv.nodes.flatMap(n=>(n.agents||[]).filter(a=>a.availability!=='disabled'&&a.probe?.version_supported&&n.reachable).map(a=>({node:n,agent:a})));}
function startNew(nodeId,agentId){
  conv.selected=null;conv.record=null;conv.generation++;
  if(nodeId&&agentId)conv.target={node:nodeId,agent:agentId};
  if(state.view!=='sessions')showView('sessions');else renderConversation();
  renderSessionList();$('#hero-input').focus();
}
function renderHero(){
  const ready=readyAgents();
  if(!conv.target||!ready.some(r=>r.node.node_id===conv.target.node&&r.agent.agent_id===conv.target.agent)){const first=ready.find(r=>r.node.local)||ready[0];conv.target=first?{node:first.node.node_id,agent:first.agent.agent_id}:null;}
  $('#hero-agents').replaceChildren(...(ready.length?ready.map(({node,agent})=>{
    const chip=el('button','agent-chip'),on=conv.target?.node===node.node_id&&conv.target?.agent===agent.agent_id;chip.type='button';chip.setAttribute('role','radio');chip.setAttribute('aria-checked',String(on));
    chip.append(icon('bot','sm'),el('span','',agent.name));if(!node.local)chip.append(el('small','',node.name));
    chip.addEventListener('click',()=>{conv.target={node:node.node_id,agent:agent.agent_id};renderHero();$('#hero-input').focus();});
    return chip;
  }):[el('div','side-empty','没有可用的 Agent。先在 Agents 页接入一个已安装的 ACP agent。')]));
  const target=ready.find(r=>r.node.node_id===conv.target?.node&&r.agent.agent_id===conv.target?.agent);
  $('#hero-target').replaceChildren(...(target?[icon('bot','xs'),el('span','',`${target.agent.name} · ${target.node.local?'本机':target.node.name}`)]:[]));
  $('#hero-send').disabled=!target||!canWrite();
}
async function submitNew(event){
  event.preventDefault();
  const target=conv.target,message=$('#hero-input').value.trim(),node=conv.nodes.find(n=>n.node_id===target?.node);
  if(!target||!node||!message)return;
  $('#hero-send').disabled=true;$('#hero-error').hidden=true;
  try{
    const record=unwrap(await api('/sessions',{method:'POST',body:JSON.stringify({agent_id:target.agent,peer_id:node.local?null:node.node_id,message})}));
    $('#hero-input').value='';await loadSessions();selectSession({...record,_remote:!node.local});
  }catch(e){$('#hero-error').textContent=`没有打开：${e.message||e.code}`;$('#hero-error').hidden=false;}
  finally{$('#hero-send').disabled=false;}
}

// ---- Conversation ----
function selectSession(s){
  conv.selected=s.session_id;conv.remote=!!s._remote;conv.record=s;conv.cursor=0;conv.events=[];conv.generation++;conv.openTools.clear();conv.openThoughts.clear();
  renderSessionList();renderConversation();follow(conv.generation);
}
async function follow(generation){
  // One long-poll at a time; switching sessions or pages ends it.
  let failures=0;
  while(generation===conv.generation&&state.view==='sessions'&&!state.stopped&&conv.selected){
    try{
      const page=unwrap(await api(`/sessions/${conv.selected}/events?after=${conv.cursor}&wait=${document.hidden?0:10}`));
      if(generation!==conv.generation)return;
      failures=0;conv.record={...conv.record,...page.session};conv.cursor=page.next;syncListRow(conv.record);
      if(page.events.length)conv.events.push(...page.events);
      renderConversation();
      if(document.hidden)await new Promise(r=>setTimeout(r,3000));
    }catch(e){
      if(generation!==conv.generation)return;
      failures++;$('#conversation-note').textContent=`读取会话失败：${e.message}`;$('#conversation-note').hidden=false;
      await new Promise(r=>setTimeout(r,Math.min(15000,1500*failures)));
    }
  }
}
function openPermissions(){
  const open=new Map();
  for(const e of conv.events){
    if(e.kind==='permission_request')open.set(e.payload.key,e.payload);
    else if(e.kind==='permission_resolved')open.delete(e.payload.key);
    else if(e.kind==='turn_end'||e.kind==='error')open.clear();
  }
  return open;
}
function toolIcon(kind){return {execute:'terminal',read:'file-text',search:'search',edit:'pencil',delete:'pencil',move:'pencil'}[kind]||'wrench';}
const toolStates={pending:'等待',in_progress:'进行中',completed:'完成',failed:'失败'};
function renderHeader(s){
  if(!s){const crumb=el('div','crumb');crumb.append(icon('square-pen','sm'),el('strong','','新会话'));$('#conversation-header').replaceChildren(crumb);return;}
  const crumb=el('div','crumb');crumb.append(el('span','',shortNode(s.node_id)),icon('chevron-right','sm'),el('strong','',s.agent_name));
  if(s.title){crumb.append(icon('chevron-right','sm'),el('span','',s.title));}
  const actions=el('div','chat-actions');actions.append(sessionBadge(s.status));
  if(s.status==='pending_approval'&&!conv.remote&&canWrite())actions.append(button('拒绝','ghost',()=>sessionAction('reject')),button('批准','button',()=>sessionAction('approve')));
  if(s.status!=='closed'&&canWrite())actions.append(button('关闭会话','ghost',async()=>{if(await confirmAction('关闭这个会话？',`${s.agent_name}\n关闭后会停掉它的 Agent 进程，不能再继续对话。`))sessionAction('close');}));
  $('#conversation-header').replaceChildren(crumb,actions);
}
function renderConversation(){
  const s=conv.record;
  $('#conversation').hidden=!s;$('#conversation-empty').hidden=!!s;
  // "New session" is the current page only while no conversation is open.
  $('#new-session').classList.toggle('selected',state.view==='sessions'&&!s);
  renderHeader(s);
  if(!s){renderHero();return;}
  $('#conversation-note').hidden=true;
  const stream=$('#conversation-stream'),atBottom=stream.scrollHeight-stream.scrollTop-stream.clientHeight<80;
  const col=el('div','chat-col'),meta=el('div','chat-meta');meta.id='conversation-meta';
  meta.append(el('span','',originText({...s,_remote:conv.remote})),el('span','',approverText({...s,_remote:conv.remote})),el('span','',`第 ${s.turns||0} 轮`));
  if(s.origin?.chain?.length)meta.append(el('span','conversation-chain',`调用链：${s.origin.chain.map(l=>shortNode(l.node_id)).join(' → ')} → ${shortNode(s.node_id)}`));
  col.append(meta);
  const requests=new Map();let text=null,thought=null;
  for(const e of conv.events){
    const p=e.payload||{};
    if(e.kind==='agent_message_chunk'){if(!text){text=el('div','msg agent');text.append(el('div','msg-text',''));col.append(text);}thought=null;text.lastChild.textContent+=p.text||'';continue;}
    if(e.kind==='agent_thought_chunk'){
      if(!thought){const key=e.seq;thought=el('details','thought');thought.open=conv.openThoughts.has(key);const sum=el('summary');sum.append(icon('lightbulb','sm'),el('span','','思考'));thought.append(sum,el('div','msg-text',''));const box=thought;box.addEventListener('toggle',()=>box.open?conv.openThoughts.add(key):conv.openThoughts.delete(key));col.append(box);}
      thought.lastChild.textContent+=p.text||'';continue;
    }
    text=null;thought=null;
    if(e.kind==='user_message'){
      const m=el('div','msg user'),mine=!p.from?.label&&(!p.from?.node_id||p.from.node_id===localNodeId());
      if(!mine)m.append(el('div','msg-from',p.from?.label||nodeLabel(p.from.node_id)));
      m.append(el('div','msg-text',p.text||''));col.append(m);
    }else if(e.kind==='tool_call'||e.kind==='tool_call_update'){
      const id=String(p.id);let row=col.querySelector(`[data-tool="${CSS.escape(id)}"]`);
      if(!row){row=el('details','tool');row.dataset.tool=id;row.open=conv.openTools.has(id);row.addEventListener('toggle',()=>row.open?conv.openTools.add(id):conv.openTools.delete(id));col.append(row);}
      for(const k of ['title','kind','status','input','output'])if(p[k]!==undefined&&p[k]!==null)row.dataset[k]=p[k];
      const status=row.dataset.status||'pending',sum=el('summary'),body=el('div','tool-body');
      sum.append(icon(toolIcon(row.dataset.kind)),el('span','tool-name',row.dataset.title||row.dataset.kind||'工具'),el('span',`tool-state ${status}`,toolStates[status]||status),icon('chevron-down','sm'));
      if(row.dataset.input)body.append(el('span','','输入'),el('pre','',row.dataset.input));
      if(row.dataset.output)body.append(el('span','','结果'),el('pre','',row.dataset.output));
      row.replaceChildren(sum,...(body.childElementCount?[body]:[]));
    }else if(e.kind==='plan'){
      const box=el('div','plan');for(const entry of p.entries||[]){const r=el('div',`plan-entry ${entry.status||''}`);r.append(icon(entry.status==='completed'?'check':'circle','sm'),el('span','',entry.content));box.append(r);}col.append(box);
    }else if(e.kind==='permission_request'){requests.set(p.key,p);}
    else if(e.kind==='permission_resolved'){
      const r=requests.get(p.key),note=el('div','event-note');note.append(icon('shield-alert','sm'),el('span','',`${p.allow?'已允许':'已拒绝'}${p.by==='auto'?'（自动）':p.by==='origin'?'（打开者）':''}${r?.title?` · ${r.title}`:''}`));col.append(note);
    }else if(e.kind==='turn_end'){
      const reason={cancelled:'本轮已停止',max_tokens:'达到输出上限',max_turn_requests:'达到步数上限',refusal:'Agent 拒绝了这一轮'}[p.stop_reason];
      if(reason||!['end_turn'].includes(p.stop_reason))col.append(el('div','event-note center',reason||`本轮结束（${p.stop_reason}）`));
    }else if(e.kind==='error'){const n=el('div','event-note bad');n.append(icon('x-circle','sm'),el('span','',noteText(p.message)||'出错了'));col.append(n);}
    else if(e.kind==='status'){const t=noteText(p.message);if(t!==null)col.append(el('div','event-note center',t||sessionLabels[p.status]||p.status));}
  }
  const open=openPermissions();
  if(s.status==='running'&&!open.size)col.append(el('div','working','正在处理…'));
  stream.replaceChildren(col);
  if(atBottom)stream.scrollTop=stream.scrollHeight;
  // Open permission requests sit above the composer.
  const who={...s,_remote:conv.remote};
  $('#permission-dock').replaceChildren(...[...open.values()].map(p=>{
    const card=el('div','permission'),head=el('div','permission-head');head.append(icon('shield-alert'),el('span','',`${s.agent_name} 想要执行这个操作`));
    card.append(head,el('div','permission-title',p.title||p.kind||'工具调用'));
    if(mayAnswer(who)&&canWrite()){const row=el('div','permission-actions');row.append(button('拒绝','button secondary',()=>answerPermission(p.key,false)),button('允许一次','button',()=>answerPermission(p.key,true)));card.append(row);}
    else card.append(el('div','permission-who',approverText(who)));
    return card;
  }));
  const busy=s.status==='running'||s.status==='waiting_permission',send=$('#composer-send'),input=$('#composer-input');
  send.replaceChildren(icon(busy?'square':'send'));send.classList.toggle('stop',busy);send.setAttribute('aria-label',busy?'停止本轮':'发送');
  send.disabled=!canWrite()||conv.busy||!(busy||s.status==='idle');
  input.disabled=s.status==='closed'||!canWrite();
  input.placeholder=s.status==='closed'?'会话已关闭':s.status==='pending_approval'?'等对方电脑批准后才能继续':busy?`${s.agent_name} 正在处理，可以先写下一条`:`回复 ${s.agent_name}…`;
  $('#composer-target').replaceChildren(icon('bot','xs'),el('span','',`${s.agent_name} · ${conv.remote?shortNode(s.node_id):'本机'}`));
}
async function sessionAction(name){
  try{const record=unwrap(await api(`/sessions/${conv.selected}/${name}`,{method:'POST',body:'{}'}));conv.record={...conv.record,...record};renderConversation();loadSessions();}
  catch(e){toast(`操作失败：${e.message||e.code}`);}
}
async function answerPermission(key,allow){
  try{await api(`/sessions/${conv.selected}/permissions/${encodeURIComponent(key)}`,{method:'POST',body:JSON.stringify({allow})});}
  catch(e){toast(`回答失败：${e.message||e.code}`);}
}
async function sendMessage(event){
  event.preventDefault();
  const s=conv.record;if(!s||conv.busy)return;
  if(s.status==='running'||s.status==='waiting_permission')return sessionAction('cancel');
  const input=$('#composer-input'),message=input.value.trim();if(!message)return;
  conv.busy=true;renderConversation();
  try{const record=unwrap(await api(`/sessions/${conv.selected}/prompt`,{method:'POST',body:JSON.stringify({message})}));input.value='';conv.record={...conv.record,...record};}
  catch(e){toast(`发送失败：${e.message||e.code}`);}
  finally{conv.busy=false;renderConversation();loadSessions();}
}

$('#new-session').addEventListener('click',()=>startNew());
$('#hero-composer').addEventListener('submit',submitNew);
$('#hero-input').addEventListener('keydown',e=>{if(e.key==='Enter'&&(e.metaKey||e.ctrlKey))submitNew(e);});
$('#composer').addEventListener('submit',sendMessage);
$('#composer-input').addEventListener('keydown',e=>{if(e.key==='Enter'&&(e.metaKey||e.ctrlKey)&&conv.record?.status==='idle')sendMessage(e);});
setInterval(()=>{if(!document.hidden)loadSessions();},10000);
