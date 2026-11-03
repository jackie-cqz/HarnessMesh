'use strict';
let onboardingBody=null,onboardingPreview=null,onboardingBusy=false;
function onboardValue(id){return $('#onboard-'+id).value.trim();}
function onboardList(id){return onboardValue(id).split(',').map(s=>s.trim()).filter(Boolean);}
function onboardOptional(id){return onboardValue(id)||null;}
function openOnboarding(){
 if(onboardingBusy)return toast('接入操作正在完成，请稍后再打开。');
 $('#onboard-fields').hidden=false;
 delete $('#onboard-submit').dataset.complete;
 $('#onboard-fields').disabled=false;$('#onboard-review').hidden=true;$('#onboard-back').hidden=true;$('#onboard-message').textContent='';$('#onboard-submit').textContent='检查并预览';$('#onboard-submit').disabled=false;
 onboardingBody=null;onboardingPreview=null;
 const peers=$('#onboard-peer');peers.replaceChildren();option(peers,'','仅本机使用');for(const p of data('peers').filter(p=>p.status==='trusted'))option(peers,p.card.node_id,`${p.card.display_name} · ${short(p.card.node_id)}`);
 $('#onboard-dialog').showModal();
}
$('#new-agent').addEventListener('click',openOnboarding);
$('#close-onboard').addEventListener('click',()=>$('#onboard-dialog').close());
$('#onboard-workspace').addEventListener('change',()=>{$('#onboard-git-label').hidden=onboardValue('workspace')!=='git-worktree';$('#onboard-git').required=onboardValue('workspace')==='git-worktree';});
$('#onboard-back').addEventListener('click',()=>{onboardingPreview=null;$('#onboard-fields').hidden=false;$('#onboard-fields').disabled=false;$('#onboard-review').hidden=true;$('#onboard-back').hidden=true;$('#onboard-submit').textContent='检查并预览';$('#onboard-submit').disabled=false;});
$('#onboard-form').addEventListener('submit',async event=>{
 event.preventDefault();if(onboardingBusy)return;onboardingBusy=true;$('#onboard-submit').disabled=true;$('#onboard-message').textContent='';
 try{
  if(!onboardingPreview){
   onboardingBody={agent_id:crypto.randomUUID(),peer_id:onboardOptional('peer'),config:{agent:{name:onboardValue('name'),harness:onboardValue('harness'),project_id:onboardValue('project'),mode:'managed'},project:{root:onboardValue('root'),allowed_tools:onboardList('tools'),artifact_paths:onboardList('artifacts'),require_remote_task_approval:true,workspace_mode:onboardValue('workspace'),git_executable:onboardValue('workspace')==='git-worktree'?onboardOptional('git'):null},adapter:{executable:onboardValue('executable'),node_executable:onboardOptional('node'),model:onboardValue('model'),provider:onboardValue('provider'),credential_source:onboardOptional('credential'),models_file:onboardOptional('models'),env_allowlist:onboardList('env')}}};
   onboardingPreview=unwrap(await api('/onboarding/preview',{method:'POST',body:JSON.stringify(onboardingBody)}));
   onboardingBody.expected_sha256=onboardingPreview.configuration_sha256;$('#onboard-fields').disabled=true;$('#onboard-fields').hidden=true;$('#onboard-review').hidden=false;$('#onboard-back').hidden=false;
   $('#onboard-summary').replaceChildren(meta([['项目',onboardingPreview.project_id],['目录',onboardingPreview.project_root],['Agent',onboardingPreview.agent_id],['Harness',onboardingPreview.harness],['模型',onboardingPreview.model],['工作目录',onboardingPreview.workspace_mode],['工具',onboardingBody.config.project.allowed_tools.join(' · ')],['工件',onboardingBody.config.project.artifact_paths.join(' · ')||'无'],['凭据文件引用',onboardingBody.config.adapter.credential_source||'未选择'],['模型文件引用',onboardingBody.config.adapter.models_file||'未选择'],['环境变量引用',onboardingBody.config.adapter.env_allowlist.join(' · ')||'未选择'],['本机引用检查','已通过；未调用模型']]));$('#onboard-policy').textContent=onboardingPreview.policy_preview;$('#onboard-submit').textContent='登记此 Agent';
  }else{
   if(!await confirmAction('登记这个 Agent？',`${onboardingPreview.name}\n项目：${onboardingPreview.project_id}\n目录：${onboardingPreview.project_root}\n\n会检查 Harness 版本；不会调用模型或应用授权策略。`))return;
   const result=unwrap(await api('/onboarding/register',{method:'POST',body:JSON.stringify(onboardingBody)}));
   $('#onboard-message').textContent=`已登记 ${result.agent.name}。请通过 CLI 显式验证模型；需要远端访问时，将预览中的精确 grant 合并进现有策略后再应用。`;
   $('#onboard-submit').textContent='已登记';$('#onboard-submit').dataset.complete='true';$('#onboard-back').hidden=true;await refresh();toast('Agent 已登记，尚未调用模型或修改策略。');
  }
 }catch(error){$('#onboard-message').textContent=error.code==='TASK_CONFLICT'?'Agent 名称或登记 ID 已存在，或预览后的配置已变化。请检查已有 Agent 或重新预览。':problem(error);}
 finally{onboardingBusy=false;$('#onboard-submit').disabled=$('#onboard-submit').dataset.complete==='true';}
});
