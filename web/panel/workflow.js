'use strict';
const ledgerKey='hmesh.panel.pending.v1';
let pendingDraft=null,composerDraft=null,composerAgents=[],composerGeneration=0,composerBusy=false,composerPreflight=null,composerCheckGeneration=0;
try{pendingDraft=JSON.parse(sessionStorage.getItem(ledgerKey)||'null');}catch{}
function savePending(value){pendingDraft=value;if(value)sessionStorage.setItem(ledgerKey,JSON.stringify(value));else sessionStorage.removeItem(ledgerKey);renderWorkflow();}
function problem(error){const names={PEER_UNREACHABLE:'执行端暂不可达，请检查节点连接并继续观察原任务',PERMISSION_DENIED:'当前任务、项目或工件授权不允许此操作，请由执行端 owner 检查',AGENT_UNAVAILABLE:'Agent 暂不可用或尚未完成验证',TASK_CONFLICT:'任务状态或原请求内容发生冲突',INVALID_INPUT:'请求参数无效或本机没有对应请求记录',INVALID_TASK_REQUEST:'请检查目标、任务长度与执行限额',TASK_PREFLIGHT_FAILED:'执行条件未通过，未提交或批准任务',PANEL_READ_ONLY:'当前面板只读，请查看运行版本与显式升级步骤',FEATURE_UNAVAILABLE:'当前 Daemon 不支持此能力，请查看运行版本',PREFLIGHT_UNAVAILABLE:'执行条件暂不可读取，请检查执行端连接与版本'};return `${names[error.code]||error.message||error.code}${error.trace_id?` · trace ${error.trace_id}`:''}`;}
function renderPreflight(box,check,error){
 box.replaceChildren();box.dataset.ready=check?String(check.ready):'unchecked';
 const composing=box.id==='compose-preflight';
 if(error){box.dataset.ready='error';box.append(el('strong','','暂时无法检查'),el('p','form-note',problem(error)));return;}
 if(!check){
  if(composing)box.append(el('strong','','待检查'));
  box.append(el('p','form-note',composing?'检查项目、Agent、工件和审批要求。':'尚未预检。提交或批准前会重新检查，不调用模型，也不会增加授权。'));
  return;
 }
 box.append(el('strong','',check.ready?'执行条件预检通过':'执行条件未通过'));
 const names={PEER_NOT_TRUSTED:['执行节点信任已确认','执行节点尚未配对信任'],TASK_PERMISSION_DENIED:['当前项目任务权限已确认','没有当前项目的任务权限'],PROJECT_MISMATCH:['项目与 Agent 已匹配','项目与 Agent 不匹配'],AGENT_UNSUPPORTED:['Harness 版本与能力已支持','Harness 版本或能力不兼容'],AGENT_NOT_VERIFIED:['Agent 真实运行验证已通过','Agent 尚未通过真实运行验证'],REFERENCES_INVALID:['本机凭据引用已检查','执行端凭据引用无效'],PROJECT_UNAVAILABLE:['执行端项目可用','执行端项目不可用'],TOOLS_MISMATCH:['工具约定已匹配','工具与约定不匹配'],ARTIFACT_NOT_DECLARED:['工件声明已确认','工件未在执行端声明'],GIT_UNAVAILABLE:['Git 工作区可用','执行端无法读取 Git 工作区'],GIT_COMMIT_MISMATCH:['当前提交匹配','执行端当前提交已变化，请重新选择执行版本'],GIT_DIRTY:['工作区干净','执行端工作区有未提交改动'],GIT_COMMIT_UNAVAILABLE:['指定提交存在','执行端缺少该提交，请先同步指定提交'],PREFLIGHT_UNAVAILABLE:['执行条件可读取','执行条件暂不可读取'],PEER_UNREACHABLE:['执行节点可达','执行节点不可达']};
 const checks=el('ul','preflight-checks');for(const item of check.checks||[]){const name=names[String(item.code).toUpperCase()]?.[item.passed?0:1]||item.code;checks.append(el('li',item.passed?'good':'bad',`${item.passed?'✓':'×'} ${name}${item.field?` · ${item.field}`:''}`));}box.append(checks);
 const note=check.ready?(composing?(check.requires_approval?'提交后由执行端审批。':'提交后可直接开始。'):'这次检查不授权新路径。发送与审批时会再检查当前配置。'):
  (composing?'处理未通过项后重新检查。':'请在执行端保留原配置并处理未通过项，然后重新检查。不要改任务 ID 盲目重试。');
 box.append(el('p','form-note muted',note));
 if(check.configuration_sha256&&!composing)box.append(el('p','mono preflight-digest',`配置摘要 ${check.configuration_sha256}`));
}
function composeArtifacts(){return [...new Set($('#compose-artifacts').value.split(',').map(p=>p.trim()).filter(Boolean))];}
function renderComposerSettings(){
 const count=new Set([...composeArtifacts(),$('#contract-artifact').value.trim()].filter(Boolean)).size;
 $('#compose-artifact-summary').textContent=count?`${count} 个工件`:'可选';
 const seconds=$('#compose-timeout').value;
 $('#compose-limit-summary').textContent=seconds?`${seconds} 秒 · ${$('#compose-output').selectedOptions[0].textContent}`:'';
 $('#compose-git-summary').textContent=$('#git-mode').selectedOptions[0].textContent;
}
function composerBody(){
 const collaboration=readCollaborationDraft();
 const artifacts=composeArtifacts();
 if(collaboration.acceptance_contract&&!artifacts.includes(collaboration.acceptance_contract.artifact))artifacts.push(collaboration.acceptance_contract.artifact);
 return {request_id:composerDraft.request_id,peer_id:$('#compose-peer').value,agent_id:$('#compose-agent').value,project_id:$('#compose-project').value,goal:$('#compose-goal').value,timeout_seconds:Number($('#compose-timeout').value),max_output_bytes:Number($('#compose-output').value),expected_artifacts:artifacts,...collaboration};
}
function invalidateComposerPreflight(){composerCheckGeneration++;composerPreflight=null;renderPreflight($('#compose-preflight'));renderComposerSettings();$('#compose-submit').textContent=pendingDraft?'检查原请求':'预检并审阅';$('#compose-submit').disabled=composerBusy||!canWrite()||(!pendingDraft&&!$('#compose-project').value);}
function lockComposer(locked){
 for(const id of ['compose-timeout','compose-output','compose-goal','compose-artifacts','git-mode','git-commit','contract-artifact','contract-expected'])$('#'+id).disabled=locked;
 for(const id of ['compose-peer','compose-agent','compose-project'])$('#'+id).disabled=locked||!!composerDraft.source_session_id;
}

async function checkComposer(draft){
 const generation=++composerCheckGeneration,binding=JSON.stringify(draft),box=$('#compose-preflight');
 composerPreflight=null;box.dataset.ready='checking';box.replaceChildren(el('strong','','正在检查…'));
 const path=hasProjectWork()?'/tasks/preflight?observe_git=true':'/tasks/preflight';
 const check=unwrap(await api(path,{method:'POST',body:binding}));
 if(generation!==composerCheckGeneration||JSON.stringify(composerBody())!==binding)throw Object.assign(Error('输入已变化，请重新检查当前任务。'),{code:'PREFLIGHT_STALE'});
 projectWork.executor=check.git_version?.current||null;
 if($('#git-mode').value==='current'&&!draft.git_version&&projectWork.executor?.commit)projectWork.currentCommit=projectWork.executor.commit;
 renderGitComparison();
 composerPreflight={binding,check};renderPreflight(box,check);return check;
}
function renderWorkflow(){
 const box=$('#pending-submission');if(!pendingDraft){box.replaceChildren();return;}
 const alert=el('div','alert');alert.append(el('strong','','有一个提交尚未确认。'),el('div','mono',`request ${pendingDraft.request_id}`),el('p','','原请求与草稿已保留。请先检查它的状态，再决定下一步。'),button('检查原请求','text-button',()=>recoverOriginal(pendingDraft.request_id)),button('查看原草稿','text-button',()=>openComposer()));for(const b of alert.querySelectorAll('button'))b.disabled=!canWrite();box.replaceChildren(alert);
}
function option(select,value,text){const o=el('option','',text);o.value=value;select.append(o);}
async function loadComposerAgents(peer,selectedAgent,selectedProject){
 composerAgents=[];
 invalidateComposerPreflight();
 const generation=++composerGeneration,select=$('#compose-agent'),projects=$('#compose-project');
 select.replaceChildren();option(select,'','读取 Agent…');select.disabled=true;$('#compose-submit').disabled=true;$('#compose-check').disabled=true;
 projects.replaceChildren();option(projects,'','—');projects.disabled=true;
 $('#compose-error').hidden=true;
 try{
  const local=data('node',{}).node?.node_id;const rows=peer===local?data('agents'):unwrap(await api(`/peers/${peer}/agents`));if(generation!==composerGeneration)return;
  composerAgents=rows.filter(a=>(a.probe?.managed_run==='passed'||(hasProjectWork()&&peer===local&&a.probe?.protocol_probe==='acp_initialize_passed'))&&['idle','busy','unverified'].includes(a.availability));select.replaceChildren();
  for(const a of composerAgents)option(select,a.agent_id,`${a.name} · ${a.harness}`);
  if(!composerAgents.length)option(select,'','没有可用且已验证的 Agent');
  if(selectedAgent)select.value=selectedAgent;select.disabled=!!pendingDraft||!!composerDraft.source_session_id||!composerAgents.length;updateProjects(selectedProject);
 }catch(e){if(generation===composerGeneration){select.replaceChildren();option(select,'','Agent 不可读取');$('#compose-error').hidden=false;$('#compose-error').textContent=problem(e);}}
}
function updateProjects(selected){
 const p=$('#compose-project');p.replaceChildren();
 const agent=composerAgents.find(a=>a.agent_id===$('#compose-agent').value);
 for(const project of agent?.project_ids||[])option(p,project,project);
 if(typeof selected==='string')p.value=selected;
 if(!p.options.length)option(p,'','—');
 p.disabled=!!pendingDraft||!!composerDraft.source_session_id||!agent?.project_ids?.length;
 changedProjectTarget();
 invalidateComposerPreflight();
 $('#compose-check').disabled=composerBusy||!p.value;
}
async function openComposer(source){
 if(!canWrite())return toast('当前面板只读，请查看运行能力。');
 const dialog=$('#compose-dialog');
 if(!state.snapshot||composerBusy||dialog.open)return;
 if(source&&pendingDraft)return toast('请先在待办中处理尚未确认的请求。');
 const local=data('node',{}).node,select=$('#compose-peer');if(!local)return toast('本机状态暂不可读取。');
 composerDraft=pendingDraft?{...pendingDraft}:{request_id:crypto.randomUUID(),...source};
 select.replaceChildren();option(select,local.node_id,`${local.display_name} · 本机`);for(const p of data('peers').filter(p=>p.status==='trusted'))option(select,p.card.node_id,p.card.display_name+' · '+short(p.card.node_id));
 if(composerDraft.peer_id&&!Array.from(select.options).some(o=>o.value===composerDraft.peer_id))option(select,composerDraft.peer_id,'原执行节点（当前不可见）');
 select.value=composerDraft.peer_id||local.node_id;
 lockComposer(!!pendingDraft);
 $('#compose-goal').value=composerDraft.goal||'';$('#compose-timeout').value=composerDraft.timeout_seconds||600;$('#compose-output').value=composerDraft.max_output_bytes||16777216;
 $('#compose-artifacts').value=(composerDraft.expected_artifacts||[]).join(', ');
 $('#compose-title').textContent=source?'将会话作为任务执行':'新建委派任务';
 $('#compose-binding').textContent=`request ${composerDraft.request_id}`;$('#compose-error').hidden=true;$('#discard-pending').hidden=!pendingDraft;$('#compose-submit').textContent=pendingDraft?'检查原请求':'预检并审阅';$('#compose-check').hidden=!!pendingDraft;invalidateComposerPreflight();
 $('#compose-discovery').open=false;
 $('#compose-limits').open=false;
 $('#compose-git').open=!!composerDraft.git_version;
 $('#compose-artifact-settings').open=!!(composerDraft.expected_artifacts?.length||composerDraft.acceptance_contract);
 $('.delegation-request').open=!!pendingDraft;
 resetProjectWork(composerDraft);
 renderComposerSettings();
 dialog.showModal();
 dialog.querySelector('.delegation-scroll').scrollTop=0;
 (source?$('#compose-goal'):pendingDraft?$('#compose-submit'):select).focus({preventScroll:true});
 await loadComposerAgents(select.value,composerDraft.agent_id,composerDraft.project_id);
 if(pendingDraft){const project=$('#compose-project');project.replaceChildren();option(project,pendingDraft.project_id,pendingDraft.project_id);$('#compose-submit').disabled=false;}
 if(source&&!$('#compose-agent').value){$('#compose-error').hidden=false;$('#compose-error').textContent='会话 Agent 暂不能接收任务。请在执行端检查其验证状态。';}
}
function composerReview(draft,check){
 const box=el('div','delegation-review');
 const status=el('div',`delegation-review-status ${check.requires_approval?'approval':'ready'}`);
 status.append(el('strong','',check.requires_approval?'等待执行端审批':'提交后可直接开始'),
  el('p','',check.requires_approval?'执行端批准后，Agent 才开始处理。':'Agent 可能立即调用模型并产生费用。'));
 const rows=[
  ['执行电脑',$('#compose-peer').selectedOptions[0].textContent],
  ['Agent',composerAgents.find(a=>a.agent_id===draft.agent_id)?.name||draft.agent_id],
  ['项目',draft.project_id],['执行限额',`${draft.timeout_seconds} 秒 · ${draft.max_output_bytes/1048576} MiB 输出`],
  ['执行版本',draft.git_version?.commit||draft.git_version?.expected_commit||'沿用项目运行方式'],
  ['工件验收',draft.expected_artifacts.join(' · ')||'未设置']
 ];
 if(draft.acceptance_contract)rows.push(['预期字段',JSON.stringify(draft.acceptance_contract.expected)]);
 box.append(status,meta(rows),el('h3','','任务目标'),el('pre','delegation-review-text',draft.goal));
 return box;
}
function setComposerBusy(value){
 composerBusy=value;
 $('#close-compose').disabled=value;
 $('#compose-check').disabled=value||!$('#compose-project').value;
 if(value)$('#compose-submit').disabled=true;
}
function revealComposerCheck(){
 $('#compose-preflight').scrollIntoView({block:'nearest'});
}
async function recoverOriginal(id){
 if(!canWrite())return toast('当前面板只读，原请求仍保留。请查看运行能力。');
 const release=await claimAction();
 try{
  if(!await confirmAction('恢复原请求？',`request ${id}\n将使用本机已经持久化的原节点、Agent、目标和限额。接收端按同一 ID 去重，既有任务不会重新执行。尚未接收的请求会沿用原审批要求。`))return;
  const task=unwrap(await api(`/requests/${id}/recover`,{method:'POST',body:'{}'}));
  if(pendingDraft?.request_id===id)savePending(null);if($('#compose-dialog').open)$('#compose-dialog').close();await refresh();await openTask(allTasks().find(t=>t.task_id===task.task_id)||{...task,_request_id:id,_direction:'outbound'});toast('已读取原请求的任务状态。');
 }catch(e){toast(`恢复未确认：${problem(e)}。原草稿和请求 ID 保留。`);}finally{release();}
}
$('#compose-form').addEventListener('submit',async event=>{
 event.preventDefault();if(composerBusy)return;if(pendingDraft)return recoverOriginal(pendingDraft.request_id);if(!$('#compose-form').reportValidity())return;
 let draft;
 setComposerBusy(true);$('#compose-error').hidden=true;
 let dispatched=false;
 try{
  await settleProjectWork();
  draft=composerBody();
  if(new TextEncoder().encode(draft.goal).length>65536)throw Error('任务目标最多 65536 UTF-8 字节。');
  let check=await checkComposer(draft);
  if(!check.ready){revealComposerCheck();return;}
  if($('#git-mode').value==='current'&&!draft.git_version){
    if(!projectWork.currentCommit)throw Error('未读取到执行端当前提交。');
    draft=composerBody();check=await checkComposer(draft);if(!check.ready){revealComposerCheck();return;}
  }
  if(projectWork.origin?.commit&&projectWork.executor?.commit&&projectWork.origin.commit!==projectWork.executor.commit&&!draft.git_version)throw Error('两端提交不同，请先选择执行版本。');
  if(!await confirmAction('确认委派任务',composerReview(draft,check)))return;
  if(!(await checkComposer(draft)).ready){revealComposerCheck();return;}
  // Freeze before dispatch. Failure to persist the draft aborts submission.
  savePending(draft);
  dispatched=true;
  const task=unwrap(await api('/tasks',{method:'POST',body:JSON.stringify(draft)}));dispatched=false;savePending(null);$('#compose-dialog').close();await refresh();renderSessionWork();await openTask(allTasks().find(t=>t.task_id===task.task_id)||{...task,_request_id:draft.request_id,_direction:draft.peer_id===data('node',{}).node?.node_id?'local':'outbound'});
 }catch(e){
  const unknown=dispatched&&e.submission_attempted!==false;
  if(!unknown&&pendingDraft?.request_id===draft?.request_id)savePending(null);
  $('#compose-error').textContent=unknown?`提交未确认：${problem(e)}。请检查原请求，不要创建重复任务。`:`尚未提交：${problem(e)}。处理后可检查同一份草稿。`;$('#compose-error').hidden=false;
  if(e.preflight){composerPreflight={binding:JSON.stringify(draft),check:e.preflight};renderPreflight($('#compose-preflight'),e.preflight);}
  else if(!unknown)renderPreflight($('#compose-preflight'),null,{code:e.code||'PREFLIGHT_UNAVAILABLE',message:e.message});
  $('#compose-submit').textContent=unknown?'检查原请求':'预检并审阅';$('#discard-pending').hidden=!unknown;$('#compose-check').hidden=unknown;lockComposer(unknown);
 }finally{setComposerBusy(false);$('#compose-submit').disabled=!pendingDraft&&composerPreflight?.check.ready===false;}
});
$('#compose-check').addEventListener('click',async()=>{
 if(composerBusy||pendingDraft||!$('#compose-form').reportValidity())return;
 setComposerBusy(true);$('#compose-error').hidden=true;
 try{await settleProjectWork();await checkComposer(composerBody());}
 catch(e){renderPreflight($('#compose-preflight'),null,{code:e.code||'PREFLIGHT_UNAVAILABLE',message:e.message});}
 finally{setComposerBusy(false);$('#compose-submit').disabled=composerPreflight?.check.ready!==true;revealComposerCheck();}
});
$('#compose-dialog').addEventListener('cancel',event=>{if(composerBusy)event.preventDefault();});
$('#compose-form').addEventListener('invalid',event=>{const disclosure=event.target.closest('details');if(disclosure)disclosure.open=true;},true);
for(const input of $('#compose-form').querySelectorAll('input,select,textarea')){input.addEventListener('input',invalidateComposerPreflight);input.addEventListener('change',invalidateComposerPreflight);}
function workflowDetail(task,response){
 const box=el('section','workflow-detail');box.append(meta([['执行节点',task.target_node_id],['原请求 ID',task._request_id||'未绑定本机发起请求']]));const observed=el('p','muted form-note','最近成功观察 '+fullTime(task._observed));observed.id='last-task-observed';box.append(observed);
 if(task._request_id&&canWrite())box.append(button('恢复原请求','button secondary',()=>recoverOriginal(task._request_id)));
 const waiting=data('queue',{}).waiting?.find(w=>w.task_id===task.task_id);if(waiting)box.append(el('p','queue-note',queueReason(waiting.reason)));
 if(task.status==='PENDING_APPROVAL'){
  const local=task._direction==='local',handoff=el('section','handoff');handoff.id='approval-handoff';
  handoff.append(el('h3','',local?'本机审批待办':`等待 ${nodeName(task.target_node_id)} 审批`),el('p','form-note',local?`任务来自 ${nodeName(task.origin_node_id)}。先核对完整请求与预检，再由本机 owner 决定。`:'任务尚未开始。请执行端 owner 在自己的电脑上审阅、预检并批准；本机不能代为批准。'));
  const text=[`HarnessMesh 固定任务交接`,`执行端：${nodeName(task.target_node_id)} (${task.target_node_id})`,`任务：${task.task_id}`,`Agent：${task.agent_id}`,`项目：${task.project_id}`,`工件：${expectedArtifacts(task).join(', ')||'未设置工件验收'}`,`限额：${task.request?.limits?.timeout_seconds} 秒 / ${task.request?.limits?.max_output_bytes} 输出字节`,`在执行端本机终端，将 YOUR_NODE_DIR 换为该节点目录，先审阅完整请求及执行条件，再决定是否批准：`,`hmesh --data-dir YOUR_NODE_DIR task show ${task.task_id}`,`hmesh --data-dir YOUR_NODE_DIR task approve ${task.task_id}`].join('\n');
  const pre=el('pre','goal',text);pre.id='task-handoff-text';handoff.append(pre);const copy=button('复制固定任务交接说明','button secondary',async()=>{try{await navigator.clipboard.writeText(text);toast('已复制任务说明与执行端本机命令，不含访问凭据。');}catch{toast('复制不可用，可选中上方说明手动复制。');}});copy.id='copy-task-handoff';handoff.append(copy);box.append(handoff);
 }
 const contract=task.request?.acceptance_contract;
 const workspace=response.workspace?.data;
 if(task.request?.origin_git||workspace?.actual_git)box.append(el('h3','subheading','代码版本'),meta([
  ['发起端提交',task.request?.origin_git?.commit],['执行端实际提交',workspace?.actual_git?.commit||workspace?.base_commit],
  ['执行端分支',workspace?.actual_git?.branch],['执行端未提交改动',workspace?.actual_git?.dirty===true?'有':workspace?.actual_git?.dirty===false?'无':null]
 ]));
 if(terminal.has(task.status)&&hasProjectWork()){
  const reply=el('pre','goal');reply.hidden=true;
  const readReply=button('查看 Agent 执行报告','button secondary',async()=>{
   readReply.disabled=true;
   try{const result=unwrap(await api(`/tasks/${task.task_id}/result`));reply.textContent=result.final_text||'没有执行报告，请查看事件流。';if(result.truncated)reply.textContent+='\n（报告已截断，可查看事件流。）';reply.hidden=false;}
   catch(e){toast(problem(e));}finally{readReply.disabled=false;}
  });
  box.append(readReply,reply);
 }
 if(contract)box.append(el('h3','subheading','约定的验收条件'),el('p','form-note',contract.artifact),el('pre','goal',JSON.stringify(contract.expected,null,2)));
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
   box.append(view,progress,diff,el('p','form-note muted',`审阅后在执行端停止 Daemon，再执行 hmesh --data-dir YOUR_NODE_DIR workspace apply ${task.task_id} --sha256 ${workspace.patch_sha256}。操作会核对基础版本和原目录状态；不会自动提交。`));
  }
 }
 if(task.status!=='SUCCEEDED')return box;
 const history=response.verification?.data||[];box.append(el('h3','subheading','工件核验'));
 for(const v of history.slice(-5).reverse()){
  const card=el('div','verification-result');card.append(el('span',`badge ${v.passed?'good':'bad'}`,v.passed?'断言通过':'断言失败'),el('strong','',v.passed?'核验通过':'核验未通过'),meta([['来源',v.source==='completion-snapshot'?'完成时快照':'历史任务 · 当前文件'],['工件',v.artifact?.name],['SHA-256',v.artifact?.sha256],['Attempt',v.attempt_id],['配置摘要',v.configuration_sha256]]));
  for(const a of v.assertions||[])card.append(el('div',a.passed?'assertion good':'assertion bad',`${a.passed?'✓':'×'} ${a.field}`));box.append(card);
 }
 const paths=response.artifacts?.data?.artifacts||[];
 if(!paths.length){box.append(el('p','muted form-note',response.artifacts?.error?`工件列表不可读取：${response.artifacts.error.code}`:response.artifacts?.data?.unavailable?.some(e=>e.reason==='purged_by_owner')?'工件快照已由 owner 清理。历史核验与 hash 记录仍保留；当前文件不替代原快照。':response.artifacts?.data?.source==='completion-snapshot'?'当前授权范围内没有可用的完成时工件快照；后补文件不会写入此任务结果。':'执行端尚未授权工件。请由执行端 owner 通过 CLI 授权需要核验的相对路径。'));return box;}
 if(!canWrite()){box.append(el('p','form-note muted','当前面板只读。已有核验证据仍可查看；升级前不发送新的核验操作。'));return box;}
 const form=el('form','verify-form'),label=el('label','form-label','已授权工件'),select=el('select');select.name='artifact';select.setAttribute('aria-label','已授权工件');for(const path of paths)option(select,path,path);label.append(select);
 const expectedLabel=el('label','form-label','预期 JSON 字段'),expected=el('textarea');expected.rows=4;expected.value=contract?JSON.stringify(contract.expected,null,2):'{"status":"ok"}';expected.setAttribute('aria-label','预期 JSON');expectedLabel.append(expected);const submit=el('button','button','核验工件');submit.type='submit';const message=el('p','form-note muted');
 if(contract&&paths.includes(contract.artifact)){select.value=contract.artifact;expected.readOnly=true;select.addEventListener('change',()=>{expected.readOnly=select.value===contract.artifact;if(expected.readOnly)expected.value=JSON.stringify(contract.expected,null,2);});}
 form.append(label,expectedLabel,el('p','form-note muted',response.artifacts?.data?.source==='completion-snapshot'?'检查完成时保存的工件快照，不调用模型。执行与核验分别记录。':'历史任务：读取当前授权文件，不是完成时快照。不调用模型。'),message,submit);box.append(form);
 form.addEventListener('submit',async event=>{
  event.preventDefault();let value;try{value=JSON.parse(expected.value);if(!value||Array.isArray(value)||typeof value!=='object'||!Object.keys(value).length||Object.keys(value).length>16||new TextEncoder().encode(expected.value).length>8192)throw Error();}catch{return message.textContent='请输入包含 1–16 个字段、最多 8192 字节的 JSON 对象。';}
  submit.disabled=true;state.verificationBusy=true;
  try{const result=unwrap(await api(`/tasks/${task.task_id}/verify`,{method:'POST',body:JSON.stringify({artifact:select.value,expected:value})}));toast(result.passed?'工件核验通过。':'工件核验未通过，请查看字段断言。');state.lastDetail='';state.verificationBusy=false;await readDetail(state.detailVersion,true);}catch(e){message.textContent=problem(e);}finally{state.verificationBusy=false;submit.disabled=false;}
 });return box;
}
function workflowFailure(error,task){const box=el('div');box.append(empty('执行端状态暂不可读取',problem(error)));if(task.request){box.append(el('div','alert',`以下为历史缓存；最后成功观察：${fullTime(task._observed)}`),badge(task.status),el('pre','goal',task.request.goal));}if(task._request_id&&canWrite())box.append(button('恢复原请求','button',()=>recoverOriginal(task._request_id)));return box;}
$('#new-task').addEventListener('click',()=>openComposer());$('#close-compose').addEventListener('click',()=>$('#compose-dialog').close());$('#compose-peer').addEventListener('change',()=>loadComposerAgents($('#compose-peer').value));$('#compose-agent').addEventListener('change',()=>updateProjects());$('#compose-project').addEventListener('change',()=>changedProjectTarget());
$('#discard-pending').addEventListener('click',async()=>{if(await confirmAction('放弃本地草稿？','这不会取消已经到达执行端的任务。请先检查原请求；放弃后再次新建会得到不同任务 ID。')){savePending(null);$('#compose-dialog').close();}});
$('#stop-panel').addEventListener('click',async()=>{if(await confirmAction('关闭本次面板？','本次面板启动的 Daemon 会一起正常停止；复用的已有 Daemon 将继续运行。')){try{unwrap(await api('/panel/stop',{method:'POST',body:'{}'}));state.stopped=true;$('#sync-state').textContent='面板已关闭';toast('本次面板正在关闭。');}catch(e){toast(problem(e));}}});
renderWorkflow();
