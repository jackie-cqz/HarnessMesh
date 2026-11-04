'use strict';
const ledgerKey='hmesh.panel.pending.v1';
let pendingDraft=null,composerDraft=null,composerAgents=[],composerGeneration=0,composerBusy=false;
try{pendingDraft=JSON.parse(sessionStorage.getItem(ledgerKey)||'null');}catch{}
function savePending(value){pendingDraft=value;if(value)sessionStorage.setItem(ledgerKey,JSON.stringify(value));else sessionStorage.removeItem(ledgerKey);renderWorkflow();}
function problem(error){const names={PEER_UNREACHABLE:'执行端暂不可达，请检查节点连接',PERMISSION_DENIED:'当前任务或项目授权不允许此操作',AGENT_UNAVAILABLE:'Agent 暂不可用或尚未完成验证',TASK_CONFLICT:'任务状态或原请求内容发生冲突',INVALID_INPUT:'请求参数无效或本机没有对应请求记录',INVALID_TASK_REQUEST:'请检查目标、任务长度与执行限额'};return `${names[error.code]||error.message}${error.trace_id?` · trace ${error.trace_id}`:''}`;}
function renderWorkflow(){
 const box=$('#pending-submission');if(!pendingDraft){box.replaceChildren();return;}
 const alert=el('div','alert');alert.append(el('strong','','有一个提交尚未确认。'),el('div','mono',`request ${pendingDraft.request_id}`),el('p','','原请求与草稿已保留。请先检查它的状态，再决定下一步。'),button('检查原请求','text-button',()=>recoverOriginal(pendingDraft.request_id)),button('查看原草稿','text-button',openComposer));box.replaceChildren(alert);
}
function option(select,value,text){const o=el('option','',text);o.value=value;select.append(o);}
async function loadComposerAgents(peer,selectedAgent){
 const generation=++composerGeneration,select=$('#compose-agent');select.replaceChildren();option(select,'','读取 Agent…');select.disabled=true;$('#compose-submit').disabled=true;
 try{
  const local=data('node',{}).node?.node_id;const rows=peer===local?data('agents'):unwrap(await api(`/peers/${peer}/agents`));if(generation!==composerGeneration)return;
  composerAgents=rows.filter(a=>a.probe?.managed_run==='passed'&&['idle','busy'].includes(a.availability));select.replaceChildren();
  for(const a of composerAgents)option(select,a.agent_id,`${a.name} · ${a.harness}`);
  if(!composerAgents.length)option(select,'','没有可用且已验证的 Agent');
  if(selectedAgent)select.value=selectedAgent;select.disabled=!!pendingDraft||!composerAgents.length;updateProjects();$('#compose-submit').disabled=!composerAgents.length&&!pendingDraft;
 }catch(e){if(generation===composerGeneration){select.replaceChildren();option(select,'','Agent 不可读取');$('#compose-error').hidden=false;$('#compose-error').textContent=problem(e);}}
}
function updateProjects(){const p=$('#compose-project');p.replaceChildren();const a=composerAgents.find(a=>a.agent_id===$('#compose-agent').value);for(const project of a?.project_ids||[])option(p,project,project);p.disabled=!!pendingDraft;}
async function openComposer(){
 if(!state.snapshot)return;
 const local=data('node',{}).node,select=$('#compose-peer');if(!local)return toast('本机状态暂不可读取。');
 composerDraft=pendingDraft?{...pendingDraft}:{request_id:crypto.randomUUID()};
 select.replaceChildren();option(select,local.node_id,`${local.display_name} · 本机`);for(const p of data('peers').filter(p=>p.status==='trusted'))option(select,p.card.node_id,p.card.display_name+' · '+short(p.card.node_id));
 if(pendingDraft&&!Array.from(select.options).some(o=>o.value===pendingDraft.peer_id))option(select,pendingDraft.peer_id,'原请求节点（当前不可见）');
 select.value=pendingDraft?.peer_id||local.node_id;
 for(const id of ['compose-peer','compose-timeout','compose-output','compose-goal'])$('#'+id).disabled=!!pendingDraft;
 $('#compose-goal').value=pendingDraft?.goal||'';$('#compose-timeout').value=pendingDraft?.timeout_seconds||600;$('#compose-output').value=pendingDraft?.max_output_bytes||16777216;
 $('#compose-binding').textContent=`request ${composerDraft.request_id}`;$('#compose-error').hidden=true;$('#discard-pending').hidden=!pendingDraft;$('#compose-submit').textContent=pendingDraft?'检查原请求':'审阅并提交';
 if(!$('#compose-dialog').open)$('#compose-dialog').showModal();await loadComposerAgents(select.value,pendingDraft?.agent_id);
 if(pendingDraft){const project=$('#compose-project');project.replaceChildren();option(project,pendingDraft.project_id,pendingDraft.project_id);$('#compose-submit').disabled=false;}
}
async function recoverOriginal(id){
 if(state.actionBusy)return;state.actionBusy=true;
 try{
  if(!await confirmAction('恢复原请求？',`request ${id}\n将使用本机已经持久化的原节点、Agent、目标和限额。接收端按同一 ID 去重，既有任务不会重新执行。尚未接收的请求会沿用原审批要求。`))return;
  const task=unwrap(await api(`/requests/${id}/recover`,{method:'POST',body:'{}'}));
  if(pendingDraft?.request_id===id)savePending(null);if($('#compose-dialog').open)$('#compose-dialog').close();await refresh();await openTask(allTasks().find(t=>t.task_id===task.task_id)||{...task,_request_id:id,_direction:'outbound'});toast('已读取原请求的任务状态。');
 }catch(e){toast(`恢复未确认：${problem(e)}。原草稿和请求 ID 保留。`);}finally{state.actionBusy=false;}
}
$('#compose-form').addEventListener('submit',async event=>{
 event.preventDefault();if(composerBusy)return;if(pendingDraft)return recoverOriginal(pendingDraft.request_id);if(!$('#compose-form').reportValidity())return;
 const draft={request_id:composerDraft.request_id,peer_id:$('#compose-peer').value,agent_id:$('#compose-agent').value,project_id:$('#compose-project').value,goal:$('#compose-goal').value,timeout_seconds:Number($('#compose-timeout').value),max_output_bytes:Number($('#compose-output').value)};
 if(new TextEncoder().encode(draft.goal).length>65536)return toast('任务目标最多 65536 UTF-8 字节。');
 composerBusy=true;$('#compose-submit').disabled=true;
 try{
  if(!await confirmAction('提交这一个任务？',`节点：${$('#compose-peer').selectedOptions[0].textContent}\n项目：${draft.project_id}\nAgent：${composerAgents.find(a=>a.agent_id===draft.agent_id)?.name||draft.agent_id}\n执行上限：${draft.timeout_seconds} 秒\n输出额度：${draft.max_output_bytes/1048576} MiB\n\n提交后需要执行端审批。批准后可能调用模型并产生费用。\n\n${draft.goal}`))return;
  // Freeze before dispatch. Failure to persist the draft aborts submission.
  savePending(draft);
  const task=unwrap(await api('/tasks',{method:'POST',body:JSON.stringify(draft)}));savePending(null);$('#compose-dialog').close();await refresh();await openTask(allTasks().find(t=>t.task_id===task.task_id)||{...task,_request_id:draft.request_id,_direction:draft.peer_id===data('node',{}).node?.node_id?'local':'outbound'});
 }catch(e){$('#compose-error').textContent=`提交未确认：${problem(e)}。请检查原请求，不要创建重复任务。`;$('#compose-error').hidden=false;$('#compose-submit').textContent='检查原请求';$('#discard-pending').hidden=false;for(const id of ['compose-peer','compose-agent','compose-project','compose-timeout','compose-output','compose-goal'])$('#'+id).disabled=true;}
 finally{composerBusy=false;$('#compose-submit').disabled=false;}
});
function workflowDetail(task,response){
 const box=el('section','workflow-detail');box.append(meta([['执行节点',task.target_node_id],['原请求 ID',task._request_id||'未绑定本机发起请求']]));const observed=el('p','muted form-note','最近成功观察 '+fullTime(task._observed));observed.id='last-task-observed';box.append(observed);
 if(task._request_id)box.append(button('恢复原请求','button secondary',()=>recoverOriginal(task._request_id)));
 const waiting=data('queue',{}).waiting?.find(w=>w.task_id===task.task_id);if(waiting)box.append(el('p','queue-note',queueReason(waiting.reason)));
 const workspace=response.workspace?.data;
 if(workspace&&workspace.state!=='shared'){
  box.append(el('h3','subheading','独立工作目录'),meta([['状态',workspace.state],['基础 commit',workspace.base_commit],['修改 SHA-256',workspace.patch_sha256]]));
  if(workspace.patch_sha256){
   const diff=el('pre','goal workspace-diff'),progress=el('p','form-note muted');diff.hidden=true;let next=0,complete=false;
   const view=button('查看修改 diff','button secondary',async()=>{view.disabled=true;try{
    const value=unwrap(await api(`/tasks/${task.task_id}/workspace?after=${next}`));
    if(value.patch_sha256!==workspace.patch_sha256||value.patch_offset!==next)throw Error('修改版本或游标不一致，请重新读取任务。');
    diff.textContent+=value.patch||'';diff.hidden=false;complete=value.patch_next_offset===null;next=complete?value.patch_size_bytes:value.patch_next_offset;
    if(complete&&!diff.textContent)diff.textContent='没有文件修改。';progress.textContent=`已读取 ${next} / ${value.patch_size_bytes} 字节${complete?' · 完整 diff':' · 可继续加载；完整内容也可通过 CLI 查看'}`;view.textContent=complete?'diff 已读完':'继续加载 diff';
   }catch(e){toast(problem(e));}finally{view.disabled=complete;}});
   box.append(view,progress,diff,el('p','form-note muted',`合入前停止本机 Daemon，再执行 hmesh --data-dir YOUR_NODE_DIR workspace apply ${task.task_id} --sha256 ${workspace.patch_sha256}。操作会核对基础版本和原目录状态；不会自动提交。`));
  }
 }
 if(task.status!=='SUCCEEDED')return box;
 const history=response.verification?.data||[];box.append(el('h3','subheading','工件核验'));
 for(const v of history.slice(-5).reverse()){
  const card=el('div','verification-result');card.append(badge(v.passed?'SUCCEEDED':'FAILED'),el('strong','',v.passed?'核验通过':'核验未通过'),meta([['来源',v.source==='completion-snapshot'?'完成时快照':'历史任务 · 当前文件'],['工件',v.artifact?.name],['SHA-256',v.artifact?.sha256],['Attempt',v.attempt_id],['配置摘要',v.configuration_sha256]]));
  for(const a of v.assertions||[])card.append(el('div',a.passed?'assertion good':'assertion bad',`${a.passed?'✓':'×'} ${a.field}`));box.append(card);
 }
 const paths=response.artifacts?.data?.artifacts||[];
 if(!paths.length){box.append(el('p','muted form-note',response.artifacts?.error?`工件列表不可读取：${response.artifacts.error.code}`:response.artifacts?.data?.unavailable?.some(e=>e.reason==='purged_by_owner')?'工件快照已由 owner 清理。历史核验与 hash 记录仍保留；当前文件不替代原快照。':response.artifacts?.data?.source==='completion-snapshot'?'当前授权范围内没有可用的完成时工件快照；后补文件不会写入此任务结果。':'执行端尚未授权工件。请由执行端 owner 通过 CLI 授权需要核验的相对路径。'));return box;}
 const form=el('form','verify-form'),label=el('label','form-label','已授权工件'),select=el('select');select.name='artifact';select.setAttribute('aria-label','已授权工件');for(const path of paths)option(select,path,path);label.append(select);
 const expectedLabel=el('label','form-label','预期 JSON 字段'),expected=el('textarea');expected.rows=4;expected.value='{"status":"ok"}';expected.setAttribute('aria-label','预期 JSON');expectedLabel.append(expected);const submit=el('button','button','核验工件');submit.type='submit';const message=el('p','form-note muted');
 form.append(label,expectedLabel,el('p','form-note muted',response.artifacts?.data?.source==='completion-snapshot'?'检查完成时保存的工件快照，不调用模型。执行与核验分别记录。':'历史任务：读取当前授权文件，不是完成时快照。不调用模型。'),message,submit);box.append(form);
 form.addEventListener('submit',async event=>{
  event.preventDefault();let value;try{value=JSON.parse(expected.value);if(!value||Array.isArray(value)||typeof value!=='object'||!Object.keys(value).length||Object.keys(value).length>16||new TextEncoder().encode(expected.value).length>8192)throw Error();}catch{return message.textContent='请输入包含 1–16 个字段、最多 8192 字节的 JSON 对象。';}
  submit.disabled=true;
  try{const result=unwrap(await api(`/tasks/${task.task_id}/verify`,{method:'POST',body:JSON.stringify({artifact:select.value,expected:value})}));toast(result.passed?'工件核验通过。':'工件核验未通过，请查看字段断言。');state.lastDetail='';await readDetail(state.detailVersion);}catch(e){message.textContent=problem(e);}finally{submit.disabled=false;}
 });return box;
}
function workflowFailure(error,task){const box=el('div');box.append(empty('执行端状态暂不可读取',problem(error)));if(task.request){box.append(el('div','alert',`以下为历史缓存；最后成功观察：${fullTime(task._observed)}`),badge(task.status),el('pre','goal',task.request.goal));}if(task._request_id)box.append(button('恢复原请求','button',()=>recoverOriginal(task._request_id)));return box;}
$('#new-task').addEventListener('click',openComposer);$('#close-compose').addEventListener('click',()=>$('#compose-dialog').close());$('#compose-peer').addEventListener('change',()=>loadComposerAgents($('#compose-peer').value));$('#compose-agent').addEventListener('change',updateProjects);
$('#discard-pending').addEventListener('click',async()=>{if(await confirmAction('放弃本地草稿？','这不会取消已经到达执行端的任务。请先检查原请求；放弃后再次新建会得到不同任务 ID。')){savePending(null);$('#compose-dialog').close();}});
$('#stop-panel').addEventListener('click',async()=>{if(await confirmAction('关闭本次面板？','本次面板启动的 Daemon 会一起正常停止；复用的已有 Daemon 将继续运行。')){try{unwrap(await api('/panel/stop',{method:'POST',body:'{}'}));state.stopped=true;$('#sync-state').textContent='面板已关闭';toast('本次面板正在关闭。');}catch(e){toast(problem(e));}}});
renderWorkflow();
