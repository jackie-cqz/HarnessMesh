'use strict';
// Agent sessions: list on the left, the live conversation on the right.
// Every agent and peer string is rendered as text, never as HTML.
const sessionLabels={pending_approval:'等待批准',idle:'空闲',running:'进行中',waiting_permission:'等待授权',closed:'已关闭'};
const sessionTone={pending_approval:'warn',idle:'good',running:'live',waiting_permission:'warn',closed:''};
const conv={list:null,nodes:[],selected:null,cursor:0,events:[],record:null,remote:false,generation:0,busy:false,listLoading:false};

// Daemon notes are English; show them in the panel's language.
const sessionNotes=[
  [/^opened /,null],
  [/^approved on this computer/,'已批准，开始执行'],
  [/^rejected on this computer/,'已拒绝'],
  [/^closed by its owner/,'会话已关闭'],
  [/^closed by the computer that opened it/,'打开它的电脑关闭了会话'],
  [/^pairing was revoked/,'配对已撤销，会话关闭'],
  [/^the request was withdrawn/,'请求在批准前撤回'],
  [/^the agent reloaded this conversation/,'Agent 重新载入了之前的对话'],
  [/^the agent could not reload/,'Agent 没能载入之前的对话，接着对话但不带之前的上下文'],
  [/^this agent cannot reload/,'这个 Agent 不支持载入之前的对话，接着对话但不带之前的上下文'],
  [/^cancel requested/,'已请求停止'],
  [/^the turn was interrupted because HarnessMesh restarted/,'HarnessMesh 重启，这一轮被中断'],
];
function noteText(message){for(const [pattern,text]of sessionNotes)if(pattern.test(message||''))return text;return message;}
function sessionBadge(status){return el('span',`badge ${sessionTone[status]||''}`,sessionLabels[status]||status||'未知');}
function localNodeId(){return data('node',{}).node?.node_id;}
function nodeLabel(id){const n=conv.nodes.find(n=>n.node_id===id);if(n)return n.local?`${n.name}（本机）`:n.name;return nodeName(id);}
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

async function loadSessions(){
  if(conv.listLoading)return;conv.listLoading=true;
  try{
    const reply=await api('/sessions');
    if(reply.sessions?.error)throw Object.assign(Error(reply.sessions.error.code),reply.sessions.error);
    conv.list=reply.sessions.data;conv.nodes=reply.nodes?.data||conv.nodes;
    renderSessionList();
  }catch(e){$('#session-list').replaceChildren(empty('会话暂不可读取',e.message));}
  finally{conv.listLoading=false;}
}
function renderSessions(){if(!conv.list)loadSessions();else renderSessionList();}
// Called when the sessions view becomes visible: resume following the open conversation.
function sessionsShown(){loadSessions();if(conv.selected){conv.generation++;follow(conv.generation);}}
// Keep the list row of the open conversation as current as the conversation.
function syncListRow(record){
  if(!conv.list)return;
  const local=(conv.list.sessions||[]).find(s=>s.session_id===record.session_id);
  const remote=(conv.list.remote||[]).find(r=>r.session.session_id===record.session_id);
  const target=local||remote?.session;if(!target)return;
  if(target.status===record.status&&target.turns===record.turns&&target.title===record.title)return;
  Object.assign(target,{status:record.status,turns:record.turns,title:record.title,updated_at:record.updated_at});renderSessionList();
}
function needsOwner(s){return (s.status==='pending_approval'&&!s._remote)||(s.status==='waiting_permission'&&mayAnswer(s));}
// Every agent on this and paired computers, grouped by computer; one click opens a session.
function renderAgentDirectory(){
  const box=$('#session-agents');if(!box)return;
  const groups=conv.nodes.map(n=>{
    const group=el('div','agent-group');group.append(el('div','agent-group-name',n.local?`${n.name}（本机）`:`${n.name}${n.reachable?'':' · 不可达'}`));
    const agents=(n.agents||[]).filter(a=>a.availability!=='disabled');
    if(!agents.length)group.append(el('div','agent-row-empty','没有可用 Agent'));
    for(const a of agents){
      const ready=a.probe?.version_supported&&n.reachable,row=el('button','agent-row');row.type='button';row.disabled=!ready||!canWrite();
      row.append(el('span','agent-row-name',a.name),el('span','agent-row-kind',String(a.harness||'')));
      row.title=ready?'用这个 Agent 开新会话':'这个 Agent 现在不能用';
      row.addEventListener('click',()=>openNewSession(n.node_id,a.agent_id));group.append(row);
    }
    return group;
  });
  box.replaceChildren(...(groups.length?groups:[empty('还没有 Agent','在 Agents 页接入一个已安装的 ACP agent。')]));
}
function renderSessionList(){
  renderAgentDirectory();
  const rows=allSessions(),waiting=rows.filter(needsOwner).length;
  $('#nav-sessions').hidden=!waiting;$('#nav-sessions').textContent=waiting;
  $('#session-list').replaceChildren(...(rows.length?rows.map(s=>{
    const row=el('button','session-row');row.type='button';row.classList.toggle('selected',s.session_id===conv.selected);
    const top=el('div','session-row-top');top.append(el('strong','',s.agent_name),sessionBadge(s.status));
    row.append(top,el('div','session-row-title',s.title||'（还没有消息）'),el('div','session-row-meta',`${nodeLabel(s.node_id)} · ${originText(s)} · ${time(s.updated_at)}`));
    row.addEventListener('click',()=>selectSession(s));return row;
  }):[empty('还没有会话','点「新会话」选一个 Agent 开始对话。')]));
}

function selectSession(s){
  conv.selected=s.session_id;conv.remote=!!s._remote;conv.record=s;conv.cursor=0;conv.events=[];conv.generation++;
  renderSessionList();renderConversation();follow(conv.generation);
}
async function follow(generation){
  // One long-poll at a time per selected session; switching sessions ends it.
  let failures=0;
  while(generation===conv.generation&&state.view==='sessions'&&!state.stopped){
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

function permissionState(){
  // A request stays open until its key is resolved or the turn ends.
  const open=new Map();
  for(const e of conv.events){
    if(e.kind==='permission_request')open.set(e.payload.key,e);
    else if(e.kind==='permission_resolved')open.delete(e.payload.key);
    else if(e.kind==='turn_end'||e.kind==='error')open.clear();
  }
  return open;
}
function mayAnswer(s){
  if(s.approver==='auto')return false;
  if(s._remote)return s.approver==='origin';
  return true;
}
function renderConversation(){
  const s=conv.record,pane=$('#conversation');
  if(!s){pane.hidden=true;$('#conversation-empty').hidden=false;return;}
  pane.hidden=false;$('#conversation-empty').hidden=true;$('#conversation-note').hidden=true;
  const header=$('#conversation-header');
  const title=el('div','conversation-title');title.append(el('h2','',`${s.agent_name}`),sessionBadge(s.status));
  const sub=el('div','conversation-sub',`${nodeLabel(s.node_id)} · ${originText(s)} · ${approverText(s)} · 第 ${s.turns||0} 轮`);
  const actions=el('div','conversation-actions');
  const busy=s.status==='running'||s.status==='waiting_permission';
  if(busy)actions.append(button('停止本轮','button secondary',()=>sessionAction('cancel')));
  if(s.status==='pending_approval'&&!conv.remote){actions.append(button('拒绝','button secondary',()=>sessionAction('reject')),button('批准','button',()=>sessionAction('approve')));}
  if(s.status!=='closed')actions.append(button('关闭会话','button secondary',async()=>{if(await confirmAction('关闭这个会话？',`${s.agent_name}\n关闭后会停止它的 Agent 进程，不能再继续对话。`))sessionAction('close');}));
  header.replaceChildren(title,sub,actions);
  if(s.origin?.chain?.length){const chain=s.origin.chain.map(l=>nodeLabel(l.node_id)).join(' → ');header.append(el('div','conversation-chain',`调用链：${chain} → ${nodeLabel(s.node_id)}`));}

  const stream=$('#conversation-stream'),atBottom=stream.scrollHeight-stream.scrollTop-stream.clientHeight<80;
  const open=permissionState(),items=[];let text=null,thought=null;
  const flush=()=>{text=null;thought=null;};
  for(const e of conv.events){
    const p=e.payload||{};
    if(e.kind==='agent_message_chunk'){if(!text){text=el('div','msg agent');text.append(el('div','msg-who',s.agent_name),el('div','msg-text',''));items.push(text);}thought=null;text.lastChild.textContent+=p.text||'';continue;}
    if(e.kind==='agent_thought_chunk'){if(!thought){thought=el('details','msg thought');thought.append(el('summary','','思考'),el('div','msg-text',''));items.push(thought);}thought.lastChild.textContent+=p.text||'';continue;}
    flush();
    if(e.kind==='user_message'){const m=el('div','msg user'),from=p.from?.label||(p.from?.node_id&&p.from.node_id!==localNodeId()?nodeLabel(p.from.node_id):'你');m.append(el('div','msg-who',from),el('div','msg-text',p.text||''));items.push(m);}
    else if(e.kind==='tool_call'||e.kind==='tool_call_update'){
      const existing=items.find(i=>i.dataset?.tool===String(p.id));
      const row=existing||el('div','tool-row');row.dataset.tool=String(p.id);
      const status=p.status||row.dataset.status||'pending';row.dataset.status=status;
      const name=p.title||row.dataset.title||p.kind||'工具';row.dataset.title=name;
      row.replaceChildren(el('span',`tool-state ${status}`,({pending:'等待',in_progress:'进行中',completed:'完成',failed:'失败'})[status]||status),el('span','tool-name',name));
      if(p.output)row.title=String(p.output);
      if(!existing)items.push(row);
    }
    else if(e.kind==='plan'){const box=el('div','plan');box.append(el('div','msg-who','计划'));for(const entry of p.entries||[]){box.append(el('div',`plan-entry ${entry.status||''}`,`${entry.status==='completed'?'✓':'·'} ${entry.content}`));}items.push(box);}
    else if(e.kind==='permission_request'){
      const card=el('div','permission');card.append(el('div','msg-who','需要授权'),el('div','permission-title',p.title||p.kind||'工具调用'));
      if(open.has(p.key)){
        if(mayAnswer({...s,_remote:conv.remote})){const row=el('div','permission-actions');row.append(button('拒绝','button secondary',()=>answerPermission(p.key,false)),button('允许一次','button',()=>answerPermission(p.key,true)));card.append(row);}
        else card.append(el('div','muted',approverText({...s,_remote:conv.remote})));
      }
      items.push(card);
    }
    else if(e.kind==='permission_resolved'){items.push(el('div','note',`${p.allow?'已允许':'已拒绝'}${p.by==='auto'?'（自动）':p.by==='origin'?'（打开者）':''}`));}
    else if(e.kind==='turn_end'){items.push(el('div','turn-end',({end_turn:'本轮结束',cancelled:'本轮已停止',max_tokens:'达到输出上限',max_turn_requests:'达到步数上限',refusal:'Agent 拒绝了这一轮'})[p.stop_reason]||`本轮结束（${p.stop_reason}）`));}
    else if(e.kind==='error'){items.push(el('div','note bad',noteText(p.message)||'出错了'));}
    else if(e.kind==='status'){const text=noteText(p.message);if(text!==null)items.push(el('div','note',text||sessionLabels[p.status]||p.status));}
  }
  if(s.status==='running'&&!open.size)items.push(el('div','working','正在工作…'));
  stream.replaceChildren(...(items.length?items:[empty('还没有消息','在下面输入第一条消息。')]));
  if(atBottom)stream.scrollTop=stream.scrollHeight;

  const input=$('#composer-input'),send=$('#composer-send');
  const writable=canWrite()&&s.status==='idle';
  send.disabled=!writable||conv.busy;input.disabled=s.status==='closed'||!canWrite();
  input.placeholder=s.status==='closed'?'会话已关闭':s.status==='pending_approval'?'等待批准后才能继续':busy?'这一轮还在进行，等它结束或先停止':'输入消息，Ctrl/⌘ + Enter 发送';
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
  const input=$('#composer-input'),message=input.value.trim();
  if(!message||!conv.selected||conv.busy)return;
  conv.busy=true;renderConversation();
  try{const record=unwrap(await api(`/sessions/${conv.selected}/prompt`,{method:'POST',body:JSON.stringify({message})}));input.value='';conv.record={...conv.record,...record};}
  catch(e){toast(`发送失败：${e.message||e.code}`);}
  finally{conv.busy=false;renderConversation();loadSessions();}
}

function fillAgents(){
  const node=conv.nodes.find(n=>n.node_id===$('#new-session-node').value),select=$('#new-session-agent');
  const agents=(node?.agents||[]).filter(a=>a.availability!=='disabled');
  select.replaceChildren(...(agents.length?agents.map(a=>{const o=el('option','',`${a.name}${a.probe?.version_supported?'':'（未通过 ACP 检查）'}`);o.value=a.agent_id;o.disabled=!a.probe?.version_supported;return o;}):[Object.assign(el('option','','这个节点没有可用 Agent'),{value:''})]));
  $('#new-session-hint').textContent=node&&!node.reachable?'这个节点暂时连不上。':node&&!node.local?`对话在 ${node.name} 上运行；工具权限由你在这里回答。`:'对话在本机运行；工具权限由你在这里回答。';
}
async function openNewSession(nodeId,agentId){
  if(!canWrite())return toast('面板处于只读模式。');
  await loadSessions();
  const select=$('#new-session-node');
  select.replaceChildren(...conv.nodes.map(n=>{const o=el('option','',n.local?`${n.name}（本机）`:`${n.name}${n.reachable?'':'（不可达）'}`);o.value=n.node_id;return o;}));
  if(typeof nodeId==='string')select.value=nodeId;
  fillAgents();
  if(typeof agentId==='string')$('#new-session-agent').value=agentId;$('#new-session-error').hidden=true;$('#new-session-dialog').showModal();$('#new-session-message').focus();
}
async function submitNewSession(event){
  event.preventDefault();
  const node=conv.nodes.find(n=>n.node_id===$('#new-session-node').value),agent=$('#new-session-agent').value,message=$('#new-session-message').value.trim();
  if(!node||!agent)return;
  const submit=$('#new-session-submit');submit.disabled=true;
  try{
    const record=unwrap(await api('/sessions',{method:'POST',body:JSON.stringify({agent_id:agent,peer_id:node.local?null:node.node_id,message:message||null})}));
    $('#new-session-dialog').close();$('#new-session-message').value='';
    await loadSessions();
    selectSession({...record,_remote:!node.local});
  }catch(e){$('#new-session-error').textContent=`没有打开：${e.message||e.code}`;$('#new-session-error').hidden=false;}
  finally{submit.disabled=false;}
}

$('#new-session').addEventListener('click',()=>openNewSession());
$('#new-session-node').addEventListener('change',fillAgents);
$('#new-session-form').addEventListener('submit',submitNewSession);
$('#composer').addEventListener('submit',sendMessage);
$('#composer-input').addEventListener('keydown',e=>{if(e.key==='Enter'&&(e.metaKey||e.ctrlKey))sendMessage(e);});
setInterval(()=>{if(!document.hidden&&state.view==='sessions')loadSessions();},10000);
