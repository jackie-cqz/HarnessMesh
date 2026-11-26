'use strict';
// Conversations use the executing computer's Agent directory and project list.
const sessionLabels={pending_approval:'等待批准',idle:'空闲',running:'进行中',waiting_permission:'等待授权',closed:'已关闭'};
const sessionTone={pending_approval:'warn',idle:'good',running:'live',waiting_permission:'warn',closed:''};
const conv={list:null,nodes:[],selected:null,cursor:0,events:[],record:null,remote:false,generation:0,busy:false,listLoading:false,target:null,openTools:new Set(),openThoughts:new Set(),heroConfig:{},heroProject:null,settingBusy:false};

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
  [/^continued a session the agent already had; this agent does not show/,'已接上 Agent 里已有的会话；这个 Agent 不回放之前的记录。'],
  [/^continued a session the agent already had/,'已接上 Agent 里已有的会话，上面是之前的记录。如果终端里还开着这个会话，先关掉它，避免两边同时写。'],
  [/^this agent cannot continue its earlier sessions/,'这个 Agent 不能接着它之前的会话'],
  [/^this project is no longer shared/,'这个项目已经不再共享给配对的电脑'],
  [/^the turn was interrupted because HarnessMesh restarted/,'HarnessMesh 重启，这一轮被中断'],
  [/^Retrying \(attempt \d+\/\d+/,'模型服务在重试，稍等'],
  [/^Retry finished, resuming\./,'重试结束，继续'],
  [/^New version available:/,'Agent 有新版本可用'],
  [/^the agent ended the turn without an answer/,'Agent 这一轮没有回答：检查它的登录和模型服务（可能是限流或密钥问题）'],
  [/agent process exited/,'Agent 进程已退出，这一轮中断'],
  [/^the agent did not stop in time/,'停止超时，已结束 Agent 进程'],
  [/^the turn exceeded its time budget; stopping it/,'这一轮超过时间预算，正在停止'],
  [/^the turn exceeded its time budget/,'这一轮超过时间预算，已停止'],
];
function connectionProblem(message){
  const text=String(message||'');
  if(/could not reach that computer/.test(text))return '连不上这台电脑：确认它开着 HarnessMesh，并且两台电脑的网络互通。';
  if(/RPC transport failed/.test(text))return '连接中断，恢复后会自动重试。';
  if(/RPC deadline exceeded/.test(text))return '对方电脑没有及时响应。';
  return text;
}
function noteText(message){
  const refused=/^the agent did not accept the setting (\S+) = "?([^":]*)"?/.exec(message||'');
  if(refused)return `Agent 没接受设置 ${refused[1]} = ${refused[2]}，保持它自己的选择`;
  for(const [pattern,text]of sessionNotes)if(pattern.test(message||''))return text;return message;
}
function icon(name,size=''){const svg=document.createElementNS('http://www.w3.org/2000/svg','svg');svg.setAttribute('class',`icon ${size}`.trim());svg.setAttribute('aria-hidden','true');const use=document.createElementNS('http://www.w3.org/2000/svg','use');use.setAttribute('href',`#i-${name}`);svg.append(use);return svg;}
function sessionBadge(status){return el('span',`badge ${sessionTone[status]||''}`,sessionLabels[status]||status||'未知');}
function localNodeId(){return data('node',{}).node?.node_id;}
// This computer's own marks on sessions: a new name, pinned, hidden.
let markIndex={list:null,map:new Map()};
function markOf(id){
  const list=conv.list?.marks||[];
  if(markIndex.list!==list)markIndex={list,map:new Map(list.map(m=>[m.session_id,m]))};
  return markIndex.map.get(id);
}
function sessionTitle(s){return markOf(s.session_id)?.title||s.title||`${s.agent_name} · 新会话`;}
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
function harnessFor(s){
  const node=conv.nodes.find(n=>n.node_id===s.node_id);
  const current=node?.agents?.find(a=>a.agent_id===s.agent_id);
  const retired=s.node_id===localNodeId()&&data('retired').find(a=>a.agent_id===s.agent_id);
  return current?.harness||retired?.harness||s.harness||'';
}
function sessionProduct(s){
  const node=conv.nodes.find(n=>n.node_id===s.node_id);
  return agentProduct(node,s.agent_id,harnessFor(s));
}
function approverText(s){
  if(harnessFor(s)==='pi')return '工具调用不经过 HarnessMesh 审批';
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
  let directoryUpdated=false;
  try{
    // Projects come with the list so sessions are grouped under them at once.
    const [reply]=await Promise.all([api('/sessions'),typeof readProjects==='function'?readProjects():null]);
    if(reply.nodes?.data){
      const previous=new Map(conv.nodes.map(node=>[node.node_id,node]));
      conv.nodes=reply.nodes.data.map(node=>{
        const last=previous.get(node.node_id);
        // Keep the names of an offline peer's Agents and shared projects for
        // the draft while blocking session creation until a fresh reply.
        return !node.reachable&&last?{...node,agents:last.agents,projects:last.projects,agent_kinds:last.agent_kinds}:node;
      });
    }else if(state.view==='agents'){
      conv.nodes=await readRegistryDirectory();
    }else{
      conv.nodes=conv.nodes.map(node=>({...node,reachable:false}));
    }
    directoryUpdated=true;
    if(reply.sessions?.error)throw Object.assign(Error(reply.sessions.error.code),reply.sessions.error);
    conv.list=reply.sessions.data;
    // The open conversation may know a newer state than this list snapshot.
    if(conv.record)syncListRow(conv.record,false);
    renderSessionList();
  }catch(e){
    if(!directoryUpdated)conv.nodes=conv.nodes.map(node=>({...node,reachable:false}));
    $('#session-list').replaceChildren(el('div','side-empty',`会话暂不可读取：${e.message}`));
  }
  finally{
    conv.listLoading=false;
    if(state.view==='sessions'&&!conv.selected)renderHero();
    if(state.view==='agents')renderAgents();
  }
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

// ---- Sidebar: computers, projects and conversations ----
function stateClass(status){return {running:'running',waiting_permission:'waiting',pending_approval:'pending',idle:'idle',closed:'closed'}[status]||'idle';}
function sessionRow(s,child){
  const row=el('button',`session-row${child?' child':''}${s.session_id===conv.selected?' selected':''}${s.status==='closed'?' closed':''}`);row.type='button';
  const dot=el('span',`state-dot ${stateClass(s.status)}`);dot.title=sessionLabels[s.status]||s.status;
  if(s.session_id===conv.selected)row.setAttribute('aria-current','true');
  if(child)row.append(icon('corner-down-right','xs'));
  const product=sessionProduct(s),mark=el('span','session-product');
  mark.append(productIcon(product.kind),dot);row.append(mark);
  row.append(el('span','session-row-title',sessionTitle(s)));
  if(markOf(s.session_id)?.pinned){const pin=icon('pin','xs');pin.classList.add('row-pin');row.append(pin);}
  row.append(el('span','session-row-time',ago(s.updated_at)));
  row.title=[...new Set([product.label,s.agent_name,sessionLabels[s.status]||s.status,originText(s)])].join(' · ');
  row.setAttribute('aria-label',sessionTitle(s)+'，'+row.title);
  row.addEventListener('click',()=>{if(state.view!=='sessions')showView('sessions');selectSession(s);});
  return row;
}
function sessionComputerHeading(id){
  const node=conv.nodes.find(n=>n.node_id===id);
  const heading=el('div','session-group-name'),emblem=el('span','computer-icon');
  emblem.append(icon('monitor','sm'));
  heading.setAttribute('role','heading');heading.setAttribute('aria-level','3');
  heading.append(emblem,el('span','computer-name',node?.name||nodeLabel(id)),el('span','computer-kind',id===localNodeId()?'本机':'已配对'));
  return heading;
}
function renderSessionList(){
  const rows=allSessions(),waiting=rows.filter(needsOwner).length;
  $('#nav-sessions').hidden=!waiting;$('#nav-sessions').textContent=waiting;
  const grouped=typeof projectsSupported==='function'&&projectsSupported();
  for(const id of ['session-tools','session-footer'])if(!grouped)$('#'+id).hidden=true;
  if(grouped){renderProjectSessions(rows);return;}
  if(!rows.length){$('#session-list').replaceChildren(el('div','side-empty','还没有会话'));return;}
  const ids=new Set(rows.map(r=>r.session_id)),groups=new Map();
  for(const s of rows){if(!groups.has(s.node_id))groups.set(s.node_id,[]);groups.get(s.node_id).push(s);}
  const order=[...groups.keys()].sort((a,b)=>(a===localNodeId()?-1:b===localNodeId()?1:0));
  $('#session-list').replaceChildren(...order.map(node=>{
    const group=el('div','session-group');group.append(sessionComputerHeading(node));
    const items=groups.get(node),inGroup=new Set(items.map(s=>s.session_id));
    // A delegated session sits under its caller when both are in this group.
    const parentOf=s=>s.origin?.chain?.at(-1)?.session_id;
    const roots=items.filter(s=>!(parentOf(s)&&inGroup.has(parentOf(s))));
    const place=(s,depth)=>{group.append(sessionRow(s,depth>0));for(const c of items.filter(c=>parentOf(c)===s.session_id&&ids.has(c.session_id)))place(c,depth+1);};
    for(const s of roots)place(s,0);
    return group;
  }));
}
for(const label of document.querySelectorAll('.side-label'))label.addEventListener('click',()=>{const open=label.getAttribute('aria-expanded')!=='true';label.setAttribute('aria-expanded',String(open));$('#'+label.getAttribute('aria-controls')).hidden=!open;});
const sidebarToggle=$('#sidebar-toggle');
sidebarToggle.addEventListener('click',()=>{
  const expanded=sidebarToggle.getAttribute('aria-expanded')==='true';
  sidebarToggle.setAttribute('aria-expanded',String(!expanded));
});

// ---- New session ----
function sessionReady(node,agent){
  return !!(state.runtime?.capabilities?.sessions&&node?.reachable&&!node.directoryOnly&&agent&&agent.availability!=='disabled'&&agent.probe?.version_supported);
}
function readyAgents(){return conv.nodes.flatMap(node=>node.agents.filter(agent=>sessionReady(node,agent)).map(agent=>({node,agent})));}
// An explicit project sets the draft directory; null clears an earlier choice.
function startNew(nodeId,agentId,project){
  resetSessionCommands();
  if(!nodeId&&$('#hero-input').value.trimStart().startsWith('/'))$('#hero-input').value='';
  $('#hero-error').hidden=true;
  $('#hero-command-status').hidden=true;
  conv.selected=null;conv.record=null;conv.generation++;
  if(nodeId&&agentId&&(conv.target?.node!==nodeId||conv.target?.agent!==agentId)){conv.target={node:nodeId,agent:agentId};conv.heroConfig={};}
  if(project!==undefined)conv.heroProject=project&&nodeId?{node:nodeId,value:project.project_id,name:project.name}:null;
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
let lastHeroTarget=null;
function renderHero(){
  if(!conv.target){
    const ready=readyAgents(),first=ready.find(row=>row.node.local)||ready[0];
    if(first)conv.target={node:first.node.node_id,agent:first.agent.agent_id};
  }
  const node=conv.nodes.find(n=>n.node_id===conv.target?.node);
  const agent=node?.agents.find(a=>a.agent_id===conv.target.agent);
  const target=agent?{node,agent}:null,available=sessionReady(node,agent);
  // Keep an explicitly selected destination even if access changes. Only a
  // deliberate choice in Agents may redirect a draft to another executor.
  if(target)lastHeroTarget=target;
  const display=target||(lastHeroTarget?.node.node_id===conv.target?.node&&lastHeroTarget?.agent.agent_id===conv.target?.agent?lastHeroTarget:null);
  const choose=button('','setting-pill toggle',()=>openAgentDirectory());
  choose.type='button';
  const label=display?`${display.agent.name} · ${display.node.local?'本机':display.node.name}`:'选择 Agent';
  const product=display&&agentProduct(display.node,display.agent.agent_id,display.agent.harness);
  choose.append(product?productIcon(product.kind):icon('bot','xs'),el('span','',label),icon('chevron-down','xs'));
  choose.setAttribute('aria-label',display?`${product.label}，${label}，点击更换 Agent`:'选择执行会话的 Agent');
  choose.title=choose.getAttribute('aria-label');
  $('#hero-target').replaceChildren(choose);
  const project=typeof heroProject==='function'?heroProject(display):null;
  if(typeof renderHeroProject==='function')renderHeroProject(display,!available,project);
  const values={...heroDefaults(target?.agent),...conv.heroConfig},box=$('#hero-settings');
  box.hidden=!settingsSupported()||!target;
  const heroNote=$('#hero-note');
  if(heroNote)heroNote.textContent=conv.target&&!available
    ?'当前 Agent 暂不可用。草稿和执行目标已保留，可等待恢复或另选 Agent。'
    :project?.missing?'之前选择的项目已不可用。请在输入框下方重新选择工作目录。'
    :!target?'从 Agents 选择一个可用的执行目标。':target.agent.harness==='pi'
    ?'Pi 的工具调用会直接执行，不在 HarnessMesh 里请求审批；本机装了 pi-mcp-adapter 时，会话里可以调用其他 Agent。'
    :'发出后 Agent 会调用模型；它要动文件或执行命令时，会在这里问你。';
  renderSettings(box,target?.agent.config_options,values,!available||!canWrite(),(id,value)=>{conv.heroConfig[id]=value;renderHero();},JSON.stringify([conv.target,target?.agent.config_options,values,available,canWrite()]));
  $('#hero-send').disabled=!available||!!project?.missing||!canWrite()||openingHero?.target===conv.target;
}
let openingHero=null;
async function submitNew(event,discover=false){
  event?.preventDefault();
  const target=conv.target,input=$('#hero-input'),draft=input.value,message=draft.trim(),node=conv.nodes.find(n=>n.node_id===target?.node);
  const agent=node?.agents.find(a=>a.agent_id===target.agent);
  if(!message||!canWrite()||!sessionReady(node,agent))return;
  const project=typeof heroProject==='function'?heroProject({node,agent}):null;
  if(project?.missing)return;
  // A slash draft opens an empty session to get this Agent's live commands.
  // The draft must never become a first prompt, even on an older daemon.
  const slash=message.startsWith('/');
  if(slash&&!sessionCommandsSupported()){
    $('#hero-error').textContent='当前 Daemon 不提供会话命令，请升级后再试。';$('#hero-error').hidden=false;return;
  }
  if(discover&&!slash)return;
  const generation=conv.generation,isCurrent=()=>generation===conv.generation&&conv.target===target&&state.view==='sessions';
  if(openingHero?.generation===generation&&openingHero.target===target)return;
  const opening={generation,target};openingHero=opening;
  const defaults=heroDefaults(agent);
  const config=Object.fromEntries(Object.entries(conv.heroConfig).filter(([id,value])=>defaults[id]!==value));
  $('#hero-send').disabled=true;$('#hero-error').hidden=true;
  const status=$('#hero-command-status');status.hidden=!slash;status.textContent='正在打开会话以读取 Agent 命令…';
  try{
    const record=unwrap(await api('/sessions',{method:'POST',body:JSON.stringify({agent_id:target.agent,peer_id:node.local?null:node.node_id,...(!slash?{message}:{}),...(Object.keys(config).length?{config}:{}),...(project?.chosen?{project_id:project.chosen}:{})})}));
    loadSessions();
    if(!isCurrent()||(slash&&!input.value.trimStart().startsWith('/')))return;
    const nextDraft=slash?input.value:(input.value===draft?'':input.value);
    input.value='';conv.heroConfig={};selectSession({...record,_remote:!node.local});
    const composer=$('#composer-input');composer.value=nextDraft;composer.focus();
    if(slash)openSessionCommands();
  }catch(e){if(isCurrent()){$('#hero-error').textContent=`没有打开：${e.message||e.code}`;$('#hero-error').hidden=false;}}
  finally{if(openingHero===opening){openingHero=null;status.hidden=true;if(state.view==='sessions'&&!conv.selected)renderHero();}}
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
  if(!conv.selected||conv.settingBusy)return;
  const sessionId=conv.selected,generation=conv.generation;
  conv.settingBusy=true;renderConversation();
  try {
    const record=unwrap(await api(`/sessions/${sessionId}/config`,{method:'POST',body:JSON.stringify({config_id:id,value})}));
    if(generation===conv.generation&&sessionId===conv.selected)conv.record={...conv.record,...record};
    return generation===conv.generation&&sessionId===conv.selected;
  } catch(e){
    if(generation===conv.generation&&sessionId===conv.selected){toast(`没有切换：${e.message||e.code}`);$('#composer-settings').dataset.key='';}
  }
  finally{conv.settingBusy=false;renderConversation();}
}

// ---- Conversation ----
function selectSession(s){
  resetSessionCommands();
  conv.selected=s.session_id;conv.remote=!!s._remote;conv.record=s;conv.cursor=0;conv.events=[];conv.generation++;conv.openTools.clear();conv.openThoughts.clear();
  renderSessionList();renderConversation();follow(conv.generation);
}
async function follow(generation){
  // One long-poll at a time; switching sessions or pages ends it.
  let failures=0;
  while(generation===conv.generation&&state.view==='sessions'&&!state.stopped&&conv.selected){
    try{
      const inboxEpoch=typeof inboxObservationToken==='function'?inboxObservationToken():null;
      const page=unwrap(await api(`/sessions/${conv.selected}/events?after=${conv.cursor}&wait=${document.hidden?0:10}`));
      if(generation!==conv.generation)return;
      failures=0;$('#conversation-note').hidden=true;conv.record={...conv.record,...page.session};conv.cursor=page.next;syncListRow(conv.record);
      if(typeof noteInboxSession==='function')noteInboxSession(conv.record,Date.now()/1000,inboxEpoch);
      if(page.events.length)conv.events.push(...page.events);
      if(page.events.some(e=>e.kind==='commands_updated'||e.kind==='config_changed'))refreshOpenCommands();
      renderConversation();
      if(document.hidden)await new Promise(r=>setTimeout(r,3000));
    }catch(e){
      if(generation!==conv.generation)return;
      failures++;$('#conversation-note').textContent=`读取会话失败：${connectionProblem(e.message)}`;$('#conversation-note').hidden=false;
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
const toolStates={pending:'等待',in_progress:'进行中',completed:'完成',failed:'失败',cancelled:'已取消'};
function renderHeader(s){
  if(!s){const crumb=el('div','crumb');crumb.append(icon('square-pen','sm'),el('strong','','新会话'));$('#conversation-header').replaceChildren(crumb);return;}
  const crumb=el('div','crumb');crumb.append(el('span','',shortNode(s.node_id)),icon('chevron-right','sm'),el('strong','',s.agent_name));
  const title=markOf(s.session_id)?.title||s.title;
  if(title){crumb.append(icon('chevron-right','sm'),el('span','',title));}
  const actions=el('div','chat-actions');actions.append(sessionBadge(s.status));
  if(hasProjectWork()&&canWrite()&&s.origin?.node_id===localNodeId()&&s.status!=='pending_approval'){
    actions.append(button('作为任务执行','button secondary',()=>startTaskFromSession(s)));
  }

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
  renderSessionWork();
  if(!s){renderHero();return;}
  $('#conversation-note').hidden=true;
  const stream=$('#conversation-stream'),atBottom=stream.scrollHeight-stream.scrollTop-stream.clientHeight<80;
  const col=el('div','chat-col'),meta=el('div','chat-meta');meta.id='conversation-meta';
  meta.append(el('span','',originText({...s,_remote:conv.remote})),el('span','',approverText({...s,_remote:conv.remote})),el('span','',`第 ${s.turns||0} 轮`));
  const projectName=typeof sessionProjectName==='function'?sessionProjectName({...s,_remote:conv.remote}):null;
  if(projectName)meta.append(el('span','',`项目：${projectName}`));
  if(harnessFor({...s,_remote:conv.remote})==='pi'){const note=el('span','', 'Pi：工具不经审批');note.title='HarnessMesh 会把本次会话的 MCP 服务桥接给 Pi（需要本机安装 pi-mcp-adapter）；没装时不能调用其他 Agent。';meta.append(note);}
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
      const reason={cancelled:'本轮已停止',timeout:'超过时间预算',max_tokens:'达到输出上限',max_turn_requests:'达到步数上限',refusal:'Agent 拒绝了这一轮'}[p.stop_reason];
      if(reason||!['end_turn'].includes(p.stop_reason))col.append(el('div','event-note center',reason||`本轮结束（${p.stop_reason}）`));
      if(p.stop_reason==='cancelled'&&harnessFor({...s,_remote:conv.remote})==='pi')col.append(el('div','event-note center','重新载入对话后，Agent 可能重试这条被取消的命令'));
    }else if(e.kind==='error'){const n=el('div','event-note bad');n.append(icon('x-circle','sm'),el('span','',noteText(p.message)||'出错了'));col.append(n);}
    // A status without a message only moves the session along.
    else if(e.kind==='status'){if(!p.message)continue;const t=noteText(p.message);if(t!==null)col.append(el('div','event-note center',t||sessionLabels[p.status]||p.status));}
    else if(e.kind==='config_changed'){
      const option=(s.config_options||[]).find(o=>o.id===p.config_id);
      col.append(el('div','event-note center',`已切换${settingNames[option?.category]||p.name||p.config_id}：${p.value_name??p.value}`));
    }
  }
  const open=openPermissions();
  if(s.status==='pending_approval')col.append(el('div','event-note center',conv.remote?`等待 ${shortNode(s.node_id)} 批准后才能继续`:'等待在这里批准后才能继续'));
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
  input.placeholder=s.status==='closed'?'会话已关闭':s.status==='pending_approval'?'等对方电脑批准后才能继续':busy?`${s.agent_name} 正在处理，可以先写下一条`:`回复 ${s.agent_name}，或输入 / 查看命令…`;
  const targetBox=$('#composer-target'),product=sessionProduct(s);
  targetBox.title=product.label+' · '+s.agent_name+' · '+shortNode(s.node_id);
  targetBox.replaceChildren(productIcon(product.kind),el('span','',`${s.agent_name} · ${conv.remote?shortNode(s.node_id):'本机'}`));
  // Settings change between turns, and only by whoever opened the session.
  const opener=conv.remote||s.origin?.node_id===localNodeId(),settings=$('#composer-settings');
  const locked=!canWrite()||busy||conv.settingBusy||s.status==='closed'||!opener;
  settings.hidden=!settingsSupported();
  settings.title=!opener?`这个会话由 ${nodeLabel(s.origin?.node_id)} 打开，设置由对方改`:busy?'这一轮结束后可以切换':'';
  renderSettings(settings,s.config_options,null,locked,changeSetting,JSON.stringify([s.session_id,s.config_options,locked]));
  updateOpenCommandStatus();
}
async function sessionAction(name){
  const id=conv.selected,generation=conv.generation;
  try {
    const record=unwrap(await api(`/sessions/${id}/${name}`,{method:'POST',body:'{}'}));
    if(generation===conv.generation&&id===conv.selected){conv.record={...conv.record,...record};renderConversation();}
    loadSessions();
  } catch(e){toast(`操作失败：${e.message||e.code}`);}
}
async function answerPermission(key,allow){
  try{await api(`/sessions/${conv.selected}/permissions/${encodeURIComponent(key)}`,{method:'POST',body:JSON.stringify({allow})});}
  catch(e){toast(`回答失败：${e.message||e.code}`);}
}
async function sendMessage(event){
  event.preventDefault();
  const s=conv.record;if(!s||conv.busy)return;
  if(s.status==='running'||s.status==='waiting_permission')return sessionAction('cancel');
  const input=$('#composer-input'),draft=input.value,message=draft.trim();if(!message)return;
  if(message.startsWith('/'))return submitSessionCommand();
  const id=conv.selected,generation=conv.generation;
  conv.busy=true;renderConversation();
  try {
    const record=unwrap(await api(`/sessions/${id}/prompt`,{method:'POST',body:JSON.stringify({message})}));
    if(generation===conv.generation&&id===conv.selected){
      if(input.value===draft)input.value='';
      conv.record={...conv.record,...record};
    }
  }
  catch(e){toast(`发送失败：${e.message||e.code}`);}
  finally{conv.busy=false;renderConversation();loadSessions();}
}

// ---- Model service per agent (Agents page) ----
// Keys go to the local daemon once and are never shown again; the dialog
// only learns whether one is stored and its last four characters.
const serviceModes={
  agent_login:['用它自己的登录','用 Agent 自己的账号和设置，这里什么都不加'],
  api_key:['单独给它一个 key','用你的 key 连它平常用的服务'],
  custom:['自定义地址','连你指定的模型服务，可以从预设里选'],
};
const service={agent:null,view:null,revision:0};
const protocolNames={'openai-chat':'OpenAI Chat Completions','anthropic-messages':'Anthropic Messages','openai-responses':'OpenAI Responses'};
// What the agent is set up with on its own, in words; the daemon learned it
// from the agent's own report or status command, never its credential files.
const dshSetupHint='可在 DSH 的「设置 → 模型」中配置账号，或在下方单独给这个 Agent 配置服务。';
const ownLoginHint={'claude-code':'在终端运行 claude 并登录','codex':'在终端运行 codex login','opencode':'在终端运行 opencode auth login','pi':'在 pi 里用 /login'};
function ownLoginText(own){
  if(!own)return '';
  const parts=[];
  if(own.kind==='api_key')parts.push(`API key${own.detail?`（来自 ${own.detail}）`:''}`);
  else if(own.kind==='subscription'){
    const plan=(own.detail||'').split(' · ')[1];
    const name=own.label==='ChatGPT'?'ChatGPT 账号':/subscription/i.test(own.label||'')?`${own.label.replace(/\s*subscription/i,'')} 订阅`:(own.label||'订阅账号');
    parts.push(plan?`${name}（${plan} 套餐）`:name);
  }
  else if(own.kind==='credentials')parts.push(`已存的凭据：${own.detail||''}`);
  else if(own.kind==='providers'){const m=/(\d+) providers, (\d+) models/.exec(own.detail||'');parts.push(m?`${m[1]} 个服务商、${m[2]} 个模型`:(own.detail||'已配好的服务商'));}
  else if(own.kind==='gateway')parts.push(`网关${own.detail?`（${own.detail}）`:''}`);
  else parts.push([own.label,own.kind==='subscription'?null:own.detail].filter(Boolean).join(' · ')||'已登录');
  if(own.account)parts.push(own.account);
  return parts.join('，');
}
function ownDefaultModel(agent){const o=(agent?.config_options||[]).find(o=>o.category==='model');const c=o?.choices?.find(c=>c.value===o.current);return c?.name||o?.current||'';}
function showOwnLogin(own,checking=false){
  const box=$('#service-own'),title=$('#service-own-title'),detail=$('#service-own-detail');
  if(state.runtime?.capabilities?.agent_own_login!==true){box.hidden=true;return;}
  box.hidden=false;box.className='service-own '+(checking&&!own?'':own?.state||'');
  $('#service-own-check').disabled=checking;$('#service-own-check').textContent=checking?'检测中…':'重新检测';
  const mode=$('#service-modes').dataset.mode,overriding=mode&&mode!=='agent_login'&&service.view?.mode!=='agent_login';
  const model=ownDefaultModel(service.agent),withModel=model?`默认模型 ${model}。`:'';
  if(checking&&!own){title.textContent='正在检测它自己的配置…';detail.textContent='只问它自己，不读它的凭据文件。';return;}
  if(own?.state==='configured'){
    title.textContent='它自己已经配好了';
    const tail=overriding?'现在用的是这里单独配的设置，会盖过它自己的；选「用它自己的登录」就恢复。':mode&&mode!=='agent_login'?'保存后改用这里的设置，盖过它自己的；选回「用它自己的登录」就不改。':'不用在这里填 key；想让它改用别的 key 或服务，在下面重新配置。';
    detail.textContent=`检测到 ${ownLoginText(own)}。${withModel}${tail}`;
  }else if(own?.state==='not_configured'){
    title.textContent='没检测到它自己的登录';
    detail.textContent=service.agent?.harness==='dsh'?dshSetupHint:`${ownLoginHint[service.agent?.harness]?ownLoginHint[service.agent.harness]+'，':''}或者在下面单独给它一个 key。`;
  }else{
    title.textContent=own?'看不出它自己配没配好':'还没检测过它自己的配置';
    detail.textContent=service.agent?.harness==='dsh'?`${withModel}${dshSetupHint}`:`${withModel}${own?'它没有报告自己的登录方式。':'点「重新检测」看看。'}`;
  }
}
async function checkOwnLogin(){
  if(state.runtime?.capabilities?.agent_own_login!==true||!service.agent)return;
  const agent=service.agent;showOwnLogin(service.view?.own_login,true);
  try{const v=unwrap(await api(`/agents/${agent.agent_id}/service/check`,{method:'POST',body:'{}'}));if(service.agent!==agent)return;service.view.own_login=v.own_login;}
  catch(e){if(service.agent!==agent)return;}
  showOwnLogin(service.view.own_login);
}
function presetAddress(preset){
  if(!preset)return '';
  return preset.addresses?preset.addresses[$('#service-protocol').value]||'':preset.base_url;
}
function servicePresets(){
  const groups=new Map();
  for(const p of service.view?.presets||[]){
    if(!presetAddress(p))continue;
    if(!groups.has(p.group))groups.set(p.group,[]);groups.get(p.group).push(p);
  }
  const none=document.createElement('option');none.value='';none.textContent='自己填地址';
  $('#service-preset').replaceChildren(none,...[...groups].map(([name,items])=>{const g=document.createElement('optgroup');g.label=name;g.append(...items.map(p=>{const o=document.createElement('option');o.value=p.id;o.textContent=`${p.name} · ${p.region}`;return o;}));return g;}));
  const address=$('#service-url').value.trim();
  const current=address&&(service.view?.presets||[]).find(p=>presetAddress(p)===address);$('#service-preset').value=current?.id||'';
  $('#service-preset-label').hidden=$('#service-modes').dataset.mode!=='custom'||groups.size===0;
}
function setKeySource(fromEnv){
  $('#service-key-from-env').checked=fromEnv;
  const mode=$('#service-modes').dataset.mode,needsKey=mode&&mode!=='agent_login';
  $('#service-key-label').hidden=!needsKey||fromEnv;$('#service-key-env-label').hidden=!needsKey||!fromEnv;
  $('#service-key-env').required=needsKey&&fromEnv;
  $('#service-key').required=needsKey&&!fromEnv&&!service.view?.api_key?.set;
}
function hintText(hint,empty){return hint?.set?`已设置${hint.last4?` · 末 4 位 ${hint.last4}`:''}，留空保持不变`:empty;}
const SERVICE_MAX_MODELS=100;
function serviceTakesList(mode){return mode==='custom'&&['opencode','pi','dsh'].includes(service.agent?.harness);}
function serviceTestable(mode){return state.runtime?.capabilities?.agent_service_test===true&&mode!=='agent_login'&&!(mode==='api_key'&&['opencode','pi','acp'].includes(service.agent?.harness));}
function setServiceMode(mode){
  $('#service-modes').dataset.mode=mode;
  for(const b of $('#service-modes').children)b.setAttribute('aria-checked',String(b.dataset.mode===mode));
  $('#service-url-label').hidden=mode!=='custom';
  $('#service-protocol-label').hidden=mode!=='custom'||!service.view?.protocols?.length;
  servicePresets();
  const namedKey=mode==='api_key'&&service.agent.harness==='acp';
  $('#service-key-variable-label').hidden=!namedKey;$('#service-key-variable').required=namedKey;
  // Daemons from before environment keys get no such option.
  const envKeys=state.runtime?.capabilities?.agent_own_login===true;
  if(!envKeys)$('#service-key-from-env').checked=false;
  $('#service-key-source').hidden=mode==='agent_login'||!envKeys;setKeySource($('#service-key-from-env').checked);
  $('#service-url').required=mode==='custom';
  const piKey=mode==='api_key'&&service.agent.harness==='pi';
  $('#service-model').required=piKey;
  $('#service-model').placeholder=piKey?'服务商/模型，例如 deepseek/deepseek-v4-pro':serviceTakesList(mode)?'不填就用下面第一个':mode==='custom'?'这个地址上的模型名':'不填就用 Agent 自己的默认';
  $('#service-model-list-label').hidden=!serviceTakesList(mode);
  $('#service-relay').hidden=!serviceTakesList(mode)||state.runtime?.capabilities?.agent_service_relay!==true;
  $('#service-test-row').hidden=!serviceTestable(mode);
  serviceDatalist(serviceModelSuggestions(mode));
  resetServiceTest();
  if(service.view)showOwnLogin(service.view.own_login,$('#service-own-check').disabled);
}
function serviceModelList(){return [...new Set($('#service-model-list').value.split('\n').map(m=>m.trim()).filter(Boolean))];}
function serviceModelSuggestions(mode){
  if(mode==='custom')return serviceModelList().map(value=>({value}));
  const choices=(service.agent.config_options||[]).find(o=>o.category==='model')?.choices||[];
  if(mode==='api_key'&&service.agent.harness==='dsh'){
    return choices.flatMap(c=>{
      try{
        const route=JSON.parse(c.value);
        if(Array.isArray(route)&&route[0]==='deepseek-official'&&typeof route[1]==='string')return [{value:route[1],name:c.name}];
      }catch{}
      return [];
    });
  }
  if(mode==='api_key'&&service.agent.harness==='pi')return choices.filter(c=>/^(deepseek|openai|anthropic)\//.test(c.value));
  if(mode==='api_key'&&service.agent.harness==='opencode')return choices.filter(c=>c.value.startsWith('opencode/'));
  return choices;
}
function serviceDatalist(names){
  const seen=new Set();
  $('#service-models').replaceChildren(...names.filter(n=>n.value&&!seen.has(n.value)&&seen.add(n.value)).map(n=>{const o=document.createElement('option');o.value=n.value;if(n.name)o.label=n.name;return o;}));
}
function resetServiceTest(){
  service.revision++;
  const run=$('#service-test');run.disabled=false;run.textContent='测试连接';
  showServiceTest(null);
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
function serviceConnection(mode){
  const body={mode};
  if(mode==='agent_login')return body;
  const key=$('#service-key').value.trim();
  if($('#service-key-from-env').checked)body.api_key_env=$('#service-key-env').value.trim();
  else if(key)body.api_key=key;
  if(mode==='custom'){
    body.base_url=$('#service-url').value.trim();
    if(service.view.protocols?.length)body.protocol=$('#service-protocol').value;
  }
  return body;
}
async function testService(){
  if(!$('#service-form').reportValidity())return;
  const mode=$('#service-modes').dataset.mode,revision=++service.revision;
  const body={...serviceConnection(mode),model:$('#service-model').value.trim()||(serviceTakesList(mode)&&serviceModelList()[0])||null};
  const run=$('#service-test');run.disabled=true;run.textContent='测试中…';
  $('#service-test-result').className='muted';$('#service-test-result').textContent='正在连接…';$('#service-fill').hidden=true;
  try{
    const result=unwrap(await api(`/agents/${service.agent.agent_id}/service/test`,{method:'POST',body:JSON.stringify(body)}));
    if(revision===service.revision)showServiceTest(result);
  }catch(e){if(revision===service.revision){$('#service-test-result').className='failed';$('#service-test-result').textContent=`没有测试：${e.message||e.code}`;}}
  finally{if(revision===service.revision){run.disabled=false;run.textContent='测试连接';}}
}
function serviceEnvRow(name='',hint=null){
  const row=el('div','service-env-row'),key=document.createElement('input'),value=document.createElement('input'),remove=button('删除','ghost small',()=>row.remove());
  key.className='env-name';key.value=name;key.placeholder='变量名，如 HTTPS_PROXY';key.maxLength=128;key.setAttribute('aria-label','变量名');
  value.className='env-value';value.type='password';value.autocomplete='off';value.placeholder=hintText(hint,'值');value.setAttribute('aria-label','变量值');
  row.dataset.existing=name;row.append(key,value,remove);return row;
}
async function openService(agent){
  const dialog=Symbol();service.dialog=dialog;
  let v;
  try{v=unwrap(await api(`/agents/${agent.agent_id}/service`));}catch(e){if(service.dialog===dialog)toast(`读取模型服务失败：${e.message||e.code}`);return;}
  if(service.dialog!==dialog)return;
  service.agent=agent;service.view=v;
  $('#service-title').textContent=`${agent.name} 的模型服务`;$('#service-subtitle').textContent=v.modes.length>1?String(agent.harness||''):`${agent.harness||''} · 用它自己的登录，这里可以设默认模型和额外环境变量`;
  // A type with only its own login has nothing to choose between.
  $('#service-modes').hidden=v.modes.length<2;
  $('#service-modes').replaceChildren(...v.modes.map(mode=>{
    const b=el('button','service-mode');b.type='button';b.dataset.mode=mode;b.setAttribute('role','radio');
    const keyTarget={dsh:'用于 DeepSeek 官方服务',pi:'用于所选服务商的官方模型',codex:'用于 OpenAI 官方服务','claude-code':'用于 Anthropic 官方服务',opencode:'用于 OpenCode 官方服务',acp:'通过指定的变量交给这个 Agent'};
    const description=mode==='api_key'?keyTarget[agent.harness]:null;
    b.append(el('strong','',serviceModes[mode]?.[0]||mode),el('span','',description||serviceModes[mode]?.[1]||''));
    b.addEventListener('click',()=>setServiceMode(mode));return b;
  }));
  $('#service-key').value='';$('#service-key').placeholder=hintText(v.api_key,'粘贴你的 key');
  $('#service-key-env').value=v.api_key_env||'';$('#service-key-from-env').checked=Boolean(v.api_key_env);
  $('#service-key-variable').value=v.key_variable||'';
  $('#service-url').value=v.base_url||'';$('#service-model').value=v.default_model||'';
  const protocol=$('#service-protocol');protocol.replaceChildren();
  for(const value of v.protocols||[])option(protocol,value,protocolNames[value]||value);
  if(v.protocol&&v.protocols?.includes(v.protocol))protocol.value=v.protocol;
  protocol.disabled=protocol.options.length<2;
  $('#service-model-list').value=(v.models||[]).join('\n');
  $('#service-env').replaceChildren(...v.env.map(e=>serviceEnvRow(e.name,e.value)));
  $('#service-env-box').open=v.env.length>0;
  $('#service-injected').hidden=!v.injected.length;$('#service-injected').textContent=`启动 ${agent.name} 时会设置：${v.injected.join('、')}`;
  $('#service-message').textContent='';$('#service-save').disabled=false;setServiceMode(v.mode);
  $('#service-dialog').showModal();
  $('#service-dialog .service-fields').scrollTop=0;
  $('#service-modes').querySelector('[aria-checked="true"]')?.focus({preventScroll:true});
  checkOwnLogin();
}
function closeService(){
  // Typed secrets do not stay in the page.
  $('#service-key').value='';for(const input of document.querySelectorAll('#service-env .env-value'))input.value='';
  if($('#service-dialog').open)$('#service-dialog').close();
}
async function saveService(event){
  event.preventDefault();
  const {agent,dialog}=service,mode=$('#service-modes').dataset.mode,env={};
  for(const row of $('#service-env').children){
    const name=row.querySelector('.env-name').value.trim(),value=row.querySelector('.env-value').value;
    if(name)env[name]=value?value:(row.dataset.existing===name?null:'');
  }
  const body={...serviceConnection(mode),default_model:$('#service-model').value.trim()||null,models:serviceTakesList(mode)?serviceModelList():[],env};
  if(mode==='api_key'&&service.agent.harness==='acp')body.key_variable=$('#service-key-variable').value.trim();
  $('#service-save').disabled=true;$('#service-message').textContent='';
  try{
    unwrap(await api(`/agents/${agent.agent_id}/service`,{method:'POST',body:JSON.stringify(body)}));
    if(service.dialog===dialog)closeService();
    toast(`已保存 ${agent.name} 的模型服务`);
    await refresh();loadSessions();
  }catch(e){if(service.dialog===dialog)$('#service-message').textContent=`没有保存：${e.message||e.code}`;}
  finally{if(service.dialog===dialog)$('#service-save').disabled=false;}
}
$('#service-form').addEventListener('submit',saveService);
$('#service-cancel').addEventListener('click',closeService);
$('#close-service').addEventListener('click',closeService);
$('#service-dialog').addEventListener('close',()=>{
  service.dialog=null;
  $('#service-key').value='';for(const input of document.querySelectorAll('#service-env .env-value'))input.value='';
  resetServiceTest();
});
$('#service-test').addEventListener('click',testService);
$('#service-own-check').addEventListener('click',checkOwnLogin);
$('#service-key-from-env').addEventListener('change',e=>{setKeySource(e.target.checked);if(e.target.checked&&!$('#service-key-env').value){const p=(service.view?.presets||[]).find(p=>p.id===$('#service-preset').value);if(p?.env_key)$('#service-key-env').value=p.env_key;}});
$('#service-protocol').addEventListener('change',()=>{
  const p=(service.view?.presets||[]).find(p=>p.id===$('#service-preset').value);
  if(p)$('#service-url').value=presetAddress(p);
  servicePresets();resetServiceTest();
});
$('#service-url').addEventListener('input',servicePresets);
$('#service-form').addEventListener('input',resetServiceTest);
$('#service-preset').addEventListener('change',e=>{
  const p=(service.view?.presets||[]).find(p=>p.id===e.target.value);if(!p)return;
  $('#service-url').value=presetAddress(p);resetServiceTest();
  if($('#service-key-from-env').checked&&!$('#service-key-env').value&&p.env_key)$('#service-key-env').value=p.env_key;
});
$('#service-fill').addEventListener('click',()=>{
  const merged=[...new Set([...serviceModelList(),...service.tested])];
  $('#service-model-list').value=merged.join('\n');$('#service-fill').hidden=true;
  $('#service-test-result').textContent+=`，已填入`;
});
$('#service-env-add').addEventListener('click',()=>{$('#service-env').append(serviceEnvRow());$('#service-env').lastChild.querySelector('input').focus();});

$('#new-session').addEventListener('click',()=>{
  if(!conv.selected&&$('#hero-input').value.trim()){$('#hero-input').focus();return;}
  startNew(undefined,undefined,null);
});
$('#hero-composer').addEventListener('submit',submitNew);
$('#hero-input').addEventListener('input',e=>{
  if(e.isComposing)return;
  if(e.target.value.trimStart().startsWith('/')){
    if($('#hero-error').hidden)submitNew(null,true);
  }else{$('#hero-error').hidden=true;$('#hero-command-status').hidden=true;}
});
$('#hero-input').addEventListener('keydown',e=>{if(e.key==='Enter'&&(e.metaKey||e.ctrlKey))submitNew(e);});
$('#composer').addEventListener('submit',sendMessage);
$('#composer-input').addEventListener('keydown',e=>{
  if(e.key!=='Enter'||!(e.metaKey||e.ctrlKey))return;
  if(conv.record?.status==='idle')sendMessage(e);
  else toast('这一轮还在执行，结束后再发送；内容可以先写在这里');
});
setInterval(()=>{if(!document.hidden)loadSessions();},10000);
