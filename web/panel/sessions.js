'use strict';
// Agent sessions in codeg's layout: sessions and agents in the app sidebar,
// a centered conversation with the composer at the bottom, and a new-session
// screen with an agent picker. Agent and peer text is always set as text.
const sessionLabels={pending_approval:'等待批准',idle:'空闲',running:'进行中',waiting_permission:'等待授权',closed:'已关闭'};
const sessionTone={pending_approval:'warn',idle:'good',running:'live',waiting_permission:'warn',closed:''};
const conv={list:null,nodes:[],selected:null,cursor:0,events:[],record:null,remote:false,generation:0,busy:false,listLoading:false,target:null,openTools:new Set(),openThoughts:new Set(),heroConfig:{},settingBusy:false};

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
function noteText(message){
  const refused=/^the agent did not accept the setting (\S+) = "?([^":]*)"?/.exec(message||'');
  if(refused)return `Agent 没接受设置 ${refused[1]} = ${refused[2]}，保持它自己的选择`;
  for(const [pattern,text]of sessionNotes)if(pattern.test(message||''))return text;return message;
}
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
  if(nodeId&&agentId&&(conv.target?.node!==nodeId||conv.target?.agent!==agentId)){conv.target={node:nodeId,agent:agentId};conv.heroConfig={};}
  if(state.view!=='sessions')showView('sessions');else renderConversation();
  renderSessionList();$('#hero-input').focus();
}
// The settings a new session starts with: the agent's own defaults, its
// default model when it offers it, then what was picked here.
function heroDefaults(agent){
  const values={};
  for(const o of agent?.config_options||[]){
    values[o.id]=o.current;
    if(o.category==='model'&&agent.default_model&&(o.choices||[]).some(c=>c.value===agent.default_model))values[o.id]=agent.default_model;
  }
  return values;
}
function renderHero(){
  const ready=readyAgents();
  if(!conv.target||!ready.some(r=>r.node.node_id===conv.target.node&&r.agent.agent_id===conv.target.agent)){const first=ready.find(r=>r.node.local)||ready[0];conv.target=first?{node:first.node.node_id,agent:first.agent.agent_id}:null;conv.heroConfig={};}
  $('#hero-agents').replaceChildren(...(ready.length?ready.map(({node,agent})=>{
    const chip=el('button','agent-chip'),on=conv.target?.node===node.node_id&&conv.target?.agent===agent.agent_id;chip.type='button';chip.setAttribute('role','radio');chip.setAttribute('aria-checked',String(on));
    chip.append(icon('bot','sm'),el('span','',agent.name));if(!node.local)chip.append(el('small','',node.name));
    chip.addEventListener('click',()=>{if(!on){conv.target={node:node.node_id,agent:agent.agent_id};conv.heroConfig={};}renderHero();$('#hero-input').focus();});
    return chip;
  }):[el('div','side-empty','没有可用的 Agent。先在 Agents 页接入一个已安装的 ACP agent。')]));
  const target=ready.find(r=>r.node.node_id===conv.target?.node&&r.agent.agent_id===conv.target?.agent);
  $('#hero-target').replaceChildren(...(target?[icon('bot','xs'),el('span','',`${target.agent.name} · ${target.node.local?'本机':target.node.name}`)]:[]));
  const values={...heroDefaults(target?.agent),...conv.heroConfig},box=$('#hero-settings');
  box.hidden=!settingsSupported()||!target;
  renderSettings(box,target?.agent.config_options,values,!canWrite(),(id,value)=>{conv.heroConfig[id]=value;renderHero();},JSON.stringify([conv.target,target?.agent.config_options,values]));
  $('#hero-send').disabled=!target||!canWrite();
}
async function submitNew(event){
  event.preventDefault();
  const target=conv.target,message=$('#hero-input').value.trim(),node=conv.nodes.find(n=>n.node_id===target?.node);
  if(!target||!node||!message)return;
  const agent=(node.agents||[]).find(a=>a.agent_id===target.agent),defaults=heroDefaults(agent);
  // Only choices that differ from what the agent would start with.
  const config=Object.fromEntries(Object.entries(conv.heroConfig).filter(([id,value])=>defaults[id]!==value));
  $('#hero-send').disabled=true;$('#hero-error').hidden=true;
  try{
    const record=unwrap(await api('/sessions',{method:'POST',body:JSON.stringify({agent_id:target.agent,peer_id:node.local?null:node.node_id,message,...(Object.keys(config).length?{config}:{})})}));
    $('#hero-input').value='';conv.heroConfig={};await loadSessions();selectSession({...record,_remote:!node.local});
  }catch(e){$('#hero-error').textContent=`没有打开：${e.message||e.code}`;$('#hero-error').hidden=false;}
  finally{$('#hero-send').disabled=false;}
}

// ---- Session settings (model, mode, reasoning) ----
const settingOrder={model:0,mode:1,thought_level:2,model_config:3};
const settingNames={model:'模型',mode:'模式',thought_level:'思考强度',model_config:'模型参数'};
function settingsSupported(){return state.runtime?.capabilities?.agent_settings===true;}
function settingControl(option,value,disabled,onChange){
  if(option.kind==='boolean'){
    const toggle=el('button','setting-pill toggle');toggle.type='button';toggle.disabled=disabled;toggle.setAttribute('aria-pressed',String(value===true));
    toggle.append(el('span','',option.name));toggle.addEventListener('click',()=>onChange(option.id,value!==true));return toggle;
  }
  const pill=el('label','setting-pill'),select=document.createElement('select'),groups=new Map();
  if(option.category&&option.category!=='model')pill.append(el('span','setting-label',settingNames[option.category]||option.name));
  select.disabled=disabled;select.setAttribute('aria-label',settingNames[option.category]||option.name);
  for(const choice of option.choices||[]){
    const item=el('option','',choice.name);item.value=choice.value;item.selected=choice.value===value;
    if(choice.group){if(!groups.has(choice.group)){const g=document.createElement('optgroup');g.label=choice.group;groups.set(choice.group,g);select.append(g);}groups.get(choice.group).append(item);}
    else select.append(item);
  }
  select.addEventListener('change',()=>onChange(option.id,select.value));
  pill.title=`${settingNames[option.category]||option.name}：${(option.choices||[]).find(c=>c.value===value)?.name||value}`;
  pill.append(select,icon('chevron-down','xs'));return pill;
}
// Rebuilt only when something changed, so an open dropdown is not replaced
// by a background refresh.
function renderSettings(box,options,values,disabled,onChange,key){
  if(box.dataset.key===key)return;box.dataset.key=key;
  const shown=[...(options||[])].filter(o=>o.kind==='boolean'||(o.choices||[]).length>1).sort((a,b)=>(settingOrder[a.category]??9)-(settingOrder[b.category]??9));
  box.replaceChildren(...shown.map(o=>settingControl(o,values?.[o.id]??o.current,disabled,onChange)));
}
async function changeSetting(id,value){
  if(!conv.selected||conv.settingBusy)return;conv.settingBusy=true;renderConversation();
  try{const record=unwrap(await api(`/sessions/${conv.selected}/config`,{method:'POST',body:JSON.stringify({config_id:id,value})}));conv.record={...conv.record,...record};}
  catch(e){toast(`没有切换：${e.message||e.code}`);$('#composer-settings').dataset.key='';}
  finally{conv.settingBusy=false;renderConversation();}
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
    else if(e.kind==='config_changed'){
      const option=(s.config_options||[]).find(o=>o.id===p.config_id);
      col.append(el('div','event-note center',`已切换${settingNames[option?.category]||p.name||p.config_id}：${p.value_name??p.value}`));
    }
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
  send.disabled=!canWrite()||conv.busy||conv.settingBusy||!(busy||s.status==='idle');
  input.disabled=s.status==='closed'||!canWrite();
  input.placeholder=s.status==='closed'?'会话已关闭':s.status==='pending_approval'?'等对方电脑批准后才能继续':busy?`${s.agent_name} 正在处理，可以先写下一条`:`回复 ${s.agent_name}…`;
  $('#composer-target').replaceChildren(icon('bot','xs'),el('span','',`${s.agent_name} · ${conv.remote?shortNode(s.node_id):'本机'}`));
  // Settings change between turns, and only by whoever opened the session.
  const opener=conv.remote||s.origin?.node_id===localNodeId(),settings=$('#composer-settings');
  const locked=!canWrite()||busy||conv.settingBusy||s.status==='closed'||!opener;
  settings.hidden=!settingsSupported();
  settings.title=!opener?`这个会话由 ${nodeLabel(s.origin?.node_id)} 打开，设置由对方改`:busy?'这一轮结束后可以切换':'';
  renderSettings(settings,s.config_options,null,locked,changeSetting,JSON.stringify([s.session_id,s.config_options,locked]));
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

// ---- Model service per agent (Agents page) ----
// Keys go to the local daemon once and are never shown again; the dialog
// only learns whether one is stored and its last four characters.
const serviceModes={
  agent_login:['用它自己的登录','用 Agent 自己的账号和设置，这里什么都不加'],
  api_key:['API key','用你的 key 连它平常用的服务'],
  custom:['自定义地址','连一个你指定的模型服务地址'],
};
const service={agent:null,view:null};
function hintText(hint,empty){return hint?.set?`已设置${hint.last4?` · 末 4 位 ${hint.last4}`:''}，留空保持不变`:empty;}
// OpenCode is the agent that takes a model list (for a custom address).
// The daemon stores at most this many.
const SERVICE_MAX_MODELS=100;
function serviceTakesList(mode){return service.agent?.harness==='opencode'&&mode==='custom';}
// Its own login has nothing to test; OpenCode's own service is tested inside OpenCode.
function serviceTestable(mode){return state.runtime?.capabilities?.agent_service_test===true&&mode!=='agent_login'&&!(service.agent?.harness==='opencode'&&mode==='api_key');}
function setServiceMode(mode){
  $('#service-modes').dataset.mode=mode;
  for(const b of $('#service-modes').children)b.setAttribute('aria-checked',String(b.dataset.mode===mode));
  $('#service-key-label').hidden=mode==='agent_login';$('#service-url-label').hidden=mode!=='custom';
  $('#service-url').required=mode==='custom';
  $('#service-model').placeholder=serviceTakesList(mode)?'不填就用下面第一个':mode==='custom'?'这个地址上的模型名':'不填就用 Agent 自己的默认';
  $('#service-model-list-label').hidden=!serviceTakesList(mode);
  $('#service-test-row').hidden=!serviceTestable(mode);
  showServiceTest(null);
}
function serviceModelList(){return [...new Set($('#service-model-list').value.split('\n').map(m=>m.trim()).filter(Boolean))];}
function serviceDatalist(names){
  const seen=new Set();
  $('#service-models').replaceChildren(...names.filter(n=>n.value&&!seen.has(n.value)&&seen.add(n.value)).map(n=>{const o=document.createElement('option');o.value=n.value;if(n.name)o.label=n.name;return o;}));
}
function showServiceTest(result){
  const out=$('#service-test-result'),fill=$('#service-fill');
  service.tested=result?.models||[];fill.hidden=true;
  if(!result){out.className='muted';out.textContent='先查模型列表，不花 token；列不出来才发一次很小的请求';return;}
  const took=` · ${result.latency_ms} ms`;
  if(result.ok&&result.method==='models'){
    // Listing models proves the address answers; some services list them without checking the key.
    const chosen=$('#service-model').value.trim();
    const missing=chosen&&result.models.length&&!result.models.includes(chosen)?`；默认模型 ${chosen} 不在其中`:'';
    out.className='ok';out.textContent=`能连上，这个服务列出了 ${result.models.length} 个模型${missing}${took}`;
    if(result.models.length){
      serviceDatalist([...result.models.map(value=>({value})),...[...$('#service-models').options].map(o=>({value:o.value,name:o.label}))]);
      // Offer only models the list does not already have, and only while they fit.
      const listed=serviceModelList(),fresh=result.models.filter(m=>!listed.includes(m));
      if(serviceTakesList($('#service-modes').dataset.mode)&&fresh.length){
        // The daemon also puts the default model first when it is missing.
        const extra=chosen&&!listed.includes(chosen)&&!fresh.includes(chosen)?1:0;
        if(listed.length+fresh.length+extra<=SERVICE_MAX_MODELS){service.tested=fresh;fill.hidden=false;fill.textContent=`填入 ${fresh.length} 个模型`;}
        else out.textContent+=`。超过 ${SERVICE_MAX_MODELS} 个，请挑几个手动填进「可切换的模型」`;
      }
    }
  }else if(result.ok){out.className='ok';out.textContent=`能连上，模型能正常回复${took}`;}
  else{out.className='failed';out.textContent=`没连上：${result.message}`;}
}
async function testService(){
  const mode=$('#service-modes').dataset.mode,key=$('#service-key').value.trim();
  const body={mode,base_url:mode==='custom'?$('#service-url').value.trim()||null:null,model:$('#service-model').value.trim()||serviceModelList()[0]||null};
  if(key)body.api_key=key;
  const run=$('#service-test');run.disabled=true;run.textContent='测试中…';
  $('#service-test-result').className='muted';$('#service-test-result').textContent='正在连接…';$('#service-fill').hidden=true;
  try{showServiceTest(unwrap(await api(`/agents/${service.agent.agent_id}/service/test`,{method:'POST',body:JSON.stringify(body)})));}
  catch(e){$('#service-test-result').className='failed';$('#service-test-result').textContent=`没有测试：${e.message||e.code}`;}
  finally{run.disabled=false;run.textContent='测试连接';}
}
function serviceEnvRow(name='',hint=null){
  const row=el('div','service-env-row'),key=document.createElement('input'),value=document.createElement('input'),remove=button('删除','ghost small',()=>row.remove());
  key.className='env-name';key.value=name;key.placeholder='变量名，如 HTTPS_PROXY';key.maxLength=128;key.setAttribute('aria-label','变量名');
  value.className='env-value';value.type='password';value.autocomplete='off';value.placeholder=hintText(hint,'值');value.setAttribute('aria-label','变量值');
  row.dataset.existing=name;row.append(key,value,remove);return row;
}
async function openService(agent){
  try{service.view=unwrap(await api(`/agents/${agent.agent_id}/service`));}catch(e){return toast(`读取模型服务失败：${e.message||e.code}`);}
  service.agent=agent;const v=service.view;
  $('#service-title').textContent=`${agent.name} 的模型服务`;$('#service-subtitle').textContent=v.modes.length>1?String(agent.harness||''):`${agent.harness||''} · 用它自己的登录，这里可以设默认模型和额外环境变量`;
  // A type with only its own login has nothing to choose between.
  $('#service-modes').hidden=v.modes.length<2;
  $('#service-modes').replaceChildren(...v.modes.map(mode=>{
    const b=el('button','service-mode');b.type='button';b.dataset.mode=mode;b.setAttribute('role','radio');
    b.append(el('strong','',serviceModes[mode]?.[0]||mode),el('span','',serviceModes[mode]?.[1]||''));
    b.addEventListener('click',()=>setServiceMode(mode));return b;
  }));
  $('#service-key').value='';$('#service-key').placeholder=hintText(v.api_key,'粘贴你的 key');
  $('#service-url').value=v.base_url||'';$('#service-model').value=v.default_model||'';
  $('#service-model-list').value=(v.models||[]).join('\n');
  const models=(agent.config_options||[]).find(o=>o.category==='model')?.choices||[];
  serviceDatalist([...(v.models||[]).map(value=>({value})),...models.map(c=>({value:c.value,name:c.name}))]);
  $('#service-env').replaceChildren(...v.env.map(e=>serviceEnvRow(e.name,e.value)));
  $('#service-env-box').open=v.env.length>0;
  $('#service-injected').hidden=!v.injected.length;$('#service-injected').textContent=`启动 ${agent.name} 时会设置：${v.injected.join('、')}`;
  $('#service-message').textContent='';setServiceMode(v.mode);
  $('#service-dialog').showModal();$('#service-modes').querySelector('[aria-checked="true"]')?.focus();
}
function closeService(){
  // Typed secrets do not stay in the page.
  $('#service-key').value='';for(const input of document.querySelectorAll('#service-env .env-value'))input.value='';
  if($('#service-dialog').open)$('#service-dialog').close();
}
async function saveService(event){
  event.preventDefault();
  const mode=$('#service-modes').dataset.mode,key=$('#service-key').value.trim(),env={};
  for(const row of $('#service-env').children){
    const name=row.querySelector('.env-name').value.trim(),value=row.querySelector('.env-value').value;
    if(name)env[name]=value?value:(row.dataset.existing===name?null:'');
  }
  const body={mode,base_url:mode==='custom'?$('#service-url').value.trim()||null:null,default_model:$('#service-model').value.trim()||null,models:serviceTakesList(mode)?serviceModelList():[],env};
  if(mode!=='agent_login'&&key)body.api_key=key;
  $('#service-save').disabled=true;$('#service-message').textContent='';
  try{
    service.view=unwrap(await api(`/agents/${service.agent.agent_id}/service`,{method:'POST',body:JSON.stringify(body)}));
    closeService();toast(`已保存 ${service.agent.name} 的模型服务`);
    await refresh();loadSessions();
  }catch(e){$('#service-message').textContent=`没有保存：${e.message||e.code}`;}
  finally{$('#service-save').disabled=false;}
}
$('#service-form').addEventListener('submit',saveService);
$('#service-cancel').addEventListener('click',closeService);
$('#close-service').addEventListener('click',closeService);
$('#service-dialog').addEventListener('close',()=>{$('#service-key').value='';});
$('#service-test').addEventListener('click',testService);
$('#service-fill').addEventListener('click',()=>{
  const merged=[...new Set([...serviceModelList(),...service.tested])];
  $('#service-model-list').value=merged.join('\n');$('#service-fill').hidden=true;
  $('#service-test-result').textContent+=`，已填入`;
});
$('#service-env-add').addEventListener('click',()=>{$('#service-env').append(serviceEnvRow());$('#service-env').lastChild.querySelector('input').focus();});

$('#new-session').addEventListener('click',()=>startNew());
$('#hero-composer').addEventListener('submit',submitNew);
$('#hero-input').addEventListener('keydown',e=>{if(e.key==='Enter'&&(e.metaKey||e.ctrlKey))submitNew(e);});
$('#composer').addEventListener('submit',sendMessage);
$('#composer-input').addEventListener('keydown',e=>{if(e.key==='Enter'&&(e.metaKey||e.ctrlKey)&&conv.record?.status==='idle')sendMessage(e);});
setInterval(()=>{if(!document.hidden)loadSessions();},10000);
