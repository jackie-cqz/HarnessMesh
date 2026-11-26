'use strict';

const onboard=id=>$('#onboard-'+id);
const onboardDialog=onboard('dialog');
// Kind → [name, command, custom], from the agent catalog (agents.js). An
// agent without a command of its own is filled in by hand.
const onboardKinds=Object.fromEntries(agentCatalog.map(a=>[a.kind,[a.custom?'其他 ACP Agent':a.label,a.command,a.custom]]));
const onboardCustom=kind=>!!onboardKinds[kind]?.[2];
const onboardPaths=new Map();
let onboardStep='agent',onboardHarness='',onboardBusy=false,onboardDetecting=false;
let onboardDetected=[],onboardOffers=[],onboardDetectionError=false,onboardVerified=null;
let onboardBody=null,onboardRegistered=null,onboardId=null;

function onboardValue(id){return onboard(id).value.trim();}
function onboardList(id){return onboardValue(id).split(',').map(s=>s.trim()).filter(Boolean);}
function onboardMessage(text='',error=false){
 const message=onboard('message');
 message.textContent=text;
 message.classList.toggle('error',error);
}

for(const [kind,[name]] of Object.entries(onboardKinds)){
 const label=el('label','onboard-choice'),radio=el('input');
 radio.type='radio';radio.name='onboard-kind';radio.value=kind;radio.required=true;
 const title=el('strong','',name),status=el('span','onboard-choice-status');
 title.id='onboard-name-'+kind;status.id='onboard-status-'+kind;
 radio.setAttribute('aria-labelledby',title.id);
 radio.setAttribute('aria-describedby',status.id);
 label.append(radio,title,status);
 onboard('harness').append(label);
}

function renderOnboardAdapter(){
 for(const kind of Object.keys(onboardKinds)){
  const status=onboard('status-'+kind),found=onboardDetected.some(a=>a.harness===kind);
  status.dataset.found=String(found);
  status.textContent=onboardCustom(kind)?'自定义命令':onboardDetecting?'检测中…':found?'本机已找到':
    onboardOffers.some(a=>a.harness===kind)?'可一键安装':onboardDetectionError?'检测不可用':'未找到';
 }
 onboard('launch').hidden=!onboardHarness;
 onboard('executable').disabled=!onboardHarness;
 onboard('args-label').hidden=!onboardCustom(onboardHarness);
 if(!onboardHarness)return;
 const found=onboardDetected.find(a=>a.harness===onboardHarness);
 const offer=onboardOffers.find(a=>a.harness===onboardHarness);
 const input=onboard('executable');
 input.placeholder=onboardKinds[onboardHarness][1]||'ACP 可执行文件';
 const path=onboardPaths.get(onboardHarness)??found?.executable??'';
 if(input.value!==path)input.value=path;
 const usingDetected=found&&input.value===found.executable;
 const canInstall=!!offer&&!found&&!input.value&&!onboardDetecting&&canWrite();
 onboard('install').hidden=!canInstall;
 onboard('retry').hidden=!onboardDetectionError||onboardDetecting||onboardCustom(onboardHarness);
 onboard('install-details').hidden=!canInstall;
 onboard('install-details').textContent=canInstall?installDetails(offer):'';
 onboard('detected').textContent=onboardCustom(onboardHarness)?'提供支持 ACP 的启动命令。':
  usingDetected&&found.executable===onboardVerified?'已通过 ACP 握手':
  usingDetected?`已找到本机命令 ${found.command}`:
  input.value?'将使用手动指定的命令。':
  onboardDetecting?'正在检测本机命令…':
  onboardDetectionError?'检测未完成，可以重试或手动填入路径。':
  canInstall?`可安装 ${onboardKinds[onboardHarness][1]}`:
  `未找到 ${onboardKinds[onboardHarness][1]}。安装后填入路径，或指定已有命令。`;
}
async function detectOnboardAgents(){
 if(onboardDetecting)return;
 onboardDetecting=true;
 renderOnboardAdapter();
 const [detected,offered]=await Promise.allSettled([api('/agents/detect').then(unwrap),api('/agents/install').then(unwrap)]);
 onboardDetected=detected.status==='fulfilled'?detected.value||[]:[];
 onboardOffers=offered.status==='fulfilled'?offered.value||[]:[];
 onboardDetectionError=detected.status==='rejected'||offered.status==='rejected';
 onboardDetecting=false;
 renderOnboardAdapter();
}

function renderOnboard(focus=false){
 const picking=onboardStep==='agent',editing=onboardStep==='project',done=onboardStep==='done';
 onboard('agent').hidden=!picking;
 onboard('agent').disabled=!picking||onboardBusy;
 onboard('project-fields').hidden=!editing;
 onboard('project-fields').disabled=!editing||onboardBusy;
 onboard('review').hidden=picking||editing;
 onboard('back').hidden=picking||done;
 onboard('back').disabled=onboardBusy;
 $('#close-onboard').disabled=onboardBusy;
 onboard('done').hidden=!done;
 onboard('done').disabled=onboardBusy;
 onboard('service').hidden=!done||state.runtime?.capabilities?.agent_settings!==true;
 onboard('service').disabled=onboardBusy;
 onboard('submit').hidden=done;
 onboard('submit').disabled=onboardBusy||(picking&&!onboardHarness);
 const submitLabels={agent:'继续绑定项目',project:'检查并预览',review:'登记此 Agent'};
 onboard('submit').textContent=submitLabels[onboardStep]||'';
 const current=picking?0:editing?1:2;
 [...onboard('steps').children].forEach((item,index)=>{
  if(index===current)item.setAttribute('aria-current','step');
  else item.removeAttribute('aria-current');
  item.classList.toggle('complete',index<current||done);
 });
 if(focus){
  const target=picking?onboard('harness').querySelector('input:checked')||onboard('harness').querySelector('input'):
   editing?onboard('name'):onboard('review-title');
  onboardDialog.querySelector('.onboard-scroll').scrollTop=0;
  target.focus({preventScroll:true});
 }
}

function resetOnboard(){
 onboard('form').reset();
 onboardPaths.clear();
 onboardHarness='';onboardStep='agent';
 onboardBody=null;onboardRegistered=null;onboardId=null;
 onboard('advanced').open=false;
 updateOnboardWorkspace();
 renderOnboardAdapter();
}
function updateOnboardPeers(){
 const peers=onboard('peer'),previous=peers.value;
 peers.replaceChildren();
 option(peers,'','仅本机使用');
 for(const p of data('peers').filter(p=>p.status==='trusted'))option(peers,p.card.node_id,`${p.card.display_name} · ${short(p.card.node_id)}`);
 if([...peers.options].some(o=>o.value===previous))peers.value=previous;
}
function openOnboarding(){
 if(!canWrite())return toast('当前面板只读，请查看运行能力与升级步骤。');
 if(onboardBusy||onboardDialog.open)return;
 if(onboardStep==='done')resetOnboard();
 if(onboardStep!=='review')updateOnboardPeers();
 const local=data('node',{}).node?.display_name;
 onboard('local').textContent=local?`执行电脑 · ${local}（本机）`:'执行电脑 · 本机';
 onboardMessage();
 renderOnboard();
 onboardDialog.showModal();
 renderOnboard(true);
 detectOnboardAgents();
}

async function installOnboardAdapter(){
 if(onboardBusy||onboard('install').hidden||!canWrite())return;
 const kind=onboardHarness,offer=onboardOffers.find(a=>a.harness===kind);
 if(!offer)return;
 onboardMessage();
 onboardBusy=true;
 renderOnboard();
 let installed=false,ticking=null;
 try{
  const large=offer.download_mb>=500?'\n\n'+`安装后约占 ${(offer.download_mb/1000).toFixed(1)} GB，下载可能需要几分钟。`:'';
  if(!await confirmAction('安装 ACP 适配器？',`${offer.package}@${offer.version}\n\n将下载到当前节点的数据目录，并检查 ACP 握手。${large}`))return;
  onboard('install').textContent='安装并检查中…';
  const started=Date.now(),detected=onboard('detected');
  detected.textContent='正在下载适配器并检查 ACP。';
  // A large adapter takes minutes; show that it is still going.
  ticking=setInterval(()=>{detected.textContent=`正在下载适配器并检查 ACP，已用 ${Math.round((Date.now()-started)/1000)} 秒。`;},1000);
  const result=unwrap(await api('/agents/install',{method:'POST',body:JSON.stringify({harness:kind}),signal:AbortSignal.timeout(((offer.timeout_s||120)+50)*1000)}));
  if(result.protocol_probe!=='acp_initialize_passed')throw Error('适配器未通过 ACP 握手。');
  onboardVerified=result.agent.executable;
  onboardDetected=[...onboardDetected.filter(a=>a.harness!==kind),result.agent];
  onboardPaths.delete(kind);
  installed=true;
 }catch(error){
  onboardMessage(`安装或握手失败。${error.message||error.code}`,true);
 }finally{
  clearInterval(ticking);
  onboardBusy=false;
  onboard('install').textContent='安装推荐版本';
  renderOnboardAdapter();
  renderOnboard();
  (installed?onboard('submit'):onboard('install')).focus();
 }
}

function updateOnboardWorkspace(){
 const worktree=onboardValue('workspace')==='git-worktree';
 onboard('git-label').hidden=!worktree;
 onboard('git').required=worktree;
}
// The package, the Node.js it needs, a command it starts and its size.
function installDetails(offer){
 const parts=[`${offer.package}@${offer.version}`,`Node.js ≥ ${offer.node}`];
 if(offer.needs)parts.push(`需要已安装 ${offer.needs.command} ${offer.needs.version}+`);
 if(offer.download_mb)parts.push(`约 ${offer.download_mb>=1000?(offer.download_mb/1000).toFixed(1)+' GB':offer.download_mb+' MB'}`);
 return parts.join(' · ');
}
// The detected agent the form points at, with the Node.js it runs with.
function onboardDetectedChoice(){return onboardDetected.find(a=>a.harness===onboardHarness&&a.executable===onboardValue('executable'));}
function buildOnboardBody(){
 const workspace=onboardValue('workspace');
 return {
  agent_id:onboardId??=crypto.randomUUID(),
  peer_id:onboardValue('peer')||null,
  config:{
   agent:{name:onboardValue('name'),harness:onboardHarness,project_id:onboardValue('project'),mode:'managed'},
   project:{
    root:onboardValue('root'),allowed_tools:onboardList('tools'),artifact_paths:onboardList('artifacts'),
    require_remote_task_approval:true,workspace_mode:workspace,
    git_executable:workspace==='git-worktree'?onboardValue('git'):null
   },
   adapter:{
    executable:onboardValue('executable'),
    node_executable:onboardDetectedChoice()?.node||null,
    args:onboardCustom(onboardHarness)?onboardValue('args').split(/\s+/).filter(Boolean):[],
    env_allowlist:onboardList('env')
   }
  }
 };
}
function renderOnboardPreview(preview,body){
 const {adapter,project}=body.config;
 onboard('review-title').textContent=preview.name;
 onboard('review-note').textContent='本机路径与引用检查已通过。登记会检查 ACP，模型服务可在登记后配置。';
 const peer=body.peer_id?onboard('peer').selectedOptions[0]?.textContent:null;
 const summary=[
  ['Agent 类型',onboardKinds[onboardHarness][0]],['项目 ID',preview.project_id],['项目目录',preview.project_root],
  ['工作目录',preview.workspace_mode==='git-worktree'?'独立 Git worktree':'共享目录'],
  ['允许工具',project.allowed_tools.join(' · ')],
  ['启动命令',[adapter.executable,...(preview.launch_args||adapter.args)].join(' ')]
 ];
 if(adapter.node_executable)summary.push(['Node.js',adapter.node_executable]);
 if(project.git_executable)summary.push(['Git 路径',project.git_executable]);
 if(project.artifact_paths.length)summary.push(['工件',project.artifact_paths.join(' · ')]);
 if(adapter.env_allowlist.length)summary.push(['环境变量名',adapter.env_allowlist.join(' · ')]);
 summary.push(['远端授权',peer?`${peer} · 仅预览，远端提交需审批`:'仅本机配置']);
 onboard('summary').replaceChildren(meta(summary));
 onboard('grant').hidden=!peer;
 onboard('grant').open=false;
 onboard('policy').textContent=peer?preview.policy_preview:'';
}
function onboardError(error){
 const hints={
  'local project directory is unavailable':'找不到这个项目目录。请填写执行电脑上已存在的绝对路径。',
  'project root must be a directory':'项目路径指向的是文件，请选择文件夹。',
  'local executable/model/credential reference check failed; no model was called':'本机引用检查失败。请检查启动命令、Git 路径和环境变量名。',
  'select a currently trusted peer':'该电脑已不在受信任列表中，请重新选择。'
 };
 if(error.code==='TASK_CONFLICT')return 'Agent 名称已被占用，或配置与预览时不同。请检查已登记的 Agent，再返回修改。';
 return hints[error.message]||error.message||problem(error);
}

$('#new-agent').addEventListener('click',openOnboarding);
$('#close-onboard').addEventListener('click',()=>onboardDialog.close());
onboardDialog.addEventListener('cancel',event=>{if(onboardBusy)event.preventDefault();});
onboard('harness').addEventListener('change',event=>{
 onboardHarness=event.target.value;
 onboardMessage();
 renderOnboardAdapter();
 renderOnboard();
});
onboard('executable').addEventListener('input',()=>{
 onboardPaths.set(onboardHarness,onboard('executable').value);
 onboardMessage();
 renderOnboardAdapter();
});
onboard('install').addEventListener('click',installOnboardAdapter);
onboard('retry').addEventListener('click',detectOnboardAgents);
onboard('workspace').addEventListener('change',updateOnboardWorkspace);
onboard('advanced').addEventListener('invalid',()=>{onboard('advanced').open=true;},true);
onboard('back').addEventListener('click',()=>{
 if(onboardBusy)return;
 onboardStep=onboardStep==='review'?'project':'agent';
 if(onboardStep==='project')updateOnboardPeers();
 onboardMessage();
 renderOnboard(true);
});
onboard('done').addEventListener('click',()=>onboardDialog.close());
onboard('service').addEventListener('click',()=>{
 onboardDialog.close();
 openService(onboardRegistered);
});
onboard('form').addEventListener('submit',async event=>{
 event.preventDefault();
 if(onboardBusy||onboardStep==='done')return;
 onboardMessage();
 if(onboardStep==='agent'){
  onboard('project-agent').textContent=`Agent 类型 · ${onboardKinds[onboardHarness][0]}`;
  onboardStep='project';
  renderOnboard(true);
  return;
 }
 const reviewing=onboardStep==='review';
 onboardBusy=true;
 renderOnboard();
 onboard('submit').textContent=reviewing?'正在登记…':'正在检查…';
 let moved=false;
 try{
  if(reviewing){
   const {agent}=unwrap(await api('/onboarding/register',{method:'POST',body:JSON.stringify(onboardBody)}));
   onboardRegistered=agent;
   onboardStep='done';
   onboard('review-title').textContent=`已登记 ${agent.name}`;
   const ready=agent.probe?.protocol_probe==='acp_initialize_passed';
   onboard('review-note').textContent=ready?'ACP 握手已通过。接下来可在模型服务里检查登录或设置 API。':
    'ACP 握手未通过。请在 Agents 列表查看检查状态。';
   state.agentFilter='active';
   renderAgents();
   await refresh();
  }else{
   onboardBody=buildOnboardBody();
   const preview=unwrap(await api('/onboarding/preview',{method:'POST',body:JSON.stringify(onboardBody)}));
   onboardBody.expected_sha256=preview.configuration_sha256;
   renderOnboardPreview(preview,onboardBody);
   onboardStep='review';
  }
  moved=true;
 }catch(error){
  onboardMessage(onboardError(error),true);
 }finally{
  onboardBusy=false;
  renderOnboard(moved);
 }
});
