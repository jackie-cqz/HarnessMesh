// Real browser + two real TLS Nodes; the only executable Agent is the test fixture.
import {spawn,spawnSync} from 'node:child_process';
import {mkdtempSync,mkdirSync,writeFileSync,readFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {Mesh,assert,until} from './acceptance/common.mjs';
const {chromium}=await import(process.env.HMESH_PLAYWRIGHT_MODULE||'playwright');
const root=mkdtempSync(join(tmpdir(),'hmesh-panel-'));
const binary=resolve('target/debug',process.platform==='win32'?'hmesh.exe':'hmesh');
const fixture=resolve('target/debug',process.platform==='win32'?'hmesh-fixture.exe':'hmesh-fixture');
const owned=[];let browser;let stage='setup';
function start(args,stdout='ignore'){
 const child=spawn(binary,args,{stdio:['ignore',stdout,'ignore'],windowsHide:true});owned.push(child);return child;
}
async function node(name){
 const dir=join(root,name),init=spawnSync(binary,['--json','--data-dir',dir,'init','--name',name,'--port','0'],{encoding:'utf8'});
 assert(init.status===0,'CLI init failed');const mesh=new Mesh(binary,dir);start(['--data-dir',dir,'daemon','start']);
 const card=await until(()=>{try{mesh.call(['daemon','status']);return mesh.call(['node','show']);}catch{return null;}});
 return{mesh,card,dir};
}
async function panel(node){
 const child=start(['--json','--data-dir',node.dir,'panel','--port','0'],'pipe');
 const lines=createInterface({input:child.stdout});
 const[line]=await Promise.race([once(lines,'line'),new Promise((_,reject)=>{const t=setTimeout(()=>reject(Error('Panel startup timeout')),20000);t.unref();})]);
 const ready=JSON.parse(line);lines.close();return ready;
}
async function succeeded(mesh,id){
 return until(()=>{const task=mesh.call(['task','show',id]);if(['FAILED','UNKNOWN','CANCELLED'].includes(task.status))throw Error('Fixture task ended '+task.status);return task.status==='SUCCEEDED'?task:null;});
}
async function confirm(page){await page.getByRole('button',{name:'确认',exact:true}).click();}
async function checkProductArtwork(page, theme){
 const choices=page.locator('#onboard-harness');
 // A <dialog> that is not open renders nothing, and the catalog can still be
 // one animation frame away on a slow runner: make sure the dialog is open
 // and its icons are on screen before counting.
 if(await page.locator('#onboard-dialog[open]').count()===0)await page.locator('#new-agent').click();
 await page.locator('#onboard-dialog[open]').waitFor({timeout:20000});
 await choices.locator('.product-icon').first().waitFor({state:'visible',timeout:20000});
 for(const kind of ['pi','opencode','dsh','codex','claude-code']){
  const image=choices.locator(`[data-product="${kind}"] img:visible`);
  // The dialog animates open; on a slow runner the first image is not laid
  // out yet when this runs, so give it a moment before counting.
  await image.first().waitFor({state:'visible',timeout:20000}).catch(()=>{});
  const count=await image.count();
  assert(count===1,`Expected one visible product icon for ${kind}, got ${count}`);
  await image.evaluate(img=>img.decode());
  if(kind==='opencode'||kind==='codex'){
   assert((await image.getAttribute('class'))==='product-'+theme,'Wrong theme asset for '+kind);
  }
 }
 assert(await choices.locator('[data-product="acp"] svg use[href="#i-terminal"]').count()===1,'Missing generic ACP icon');
}
async function checkTabIcon(page){
 const primary=page.locator('link[rel="icon"]');
 assert(await primary.count()===1&&await primary.getAttribute('href')==='/assets/brand.svg','Expected the theme-aware tab icon');
 const icons=await primary.evaluate(async link=>{
  return Promise.all([link.href,'/favicon.ico'].map(async path=>{
   const reply=await fetch(path);
   const image=new Image();image.src=path;await image.decode();
   return {path:new URL(path,location.origin).pathname,type:reply.headers.get('content-type'),width:image.naturalWidth};
  }));
 });
 assert(icons.some(i=>i.path==='/assets/brand.svg'&&i.type==='image/svg+xml'&&i.width>0),'Tab brand SVG did not load');
 assert(icons.some(i=>i.path==='/favicon.ico'&&i.type==='image/x-icon'&&i.width>=16),'Tab icon fallback did not load');
}
async function create(page,agent,goal,peer,lost=false){
 await page.locator('[data-view="inbox"]').click();
 await page.locator('[data-work-area="tasks"]').click();await page.locator('#new-task').click();
 if(peer)await page.locator('#compose-peer').selectOption(peer);
 await page.waitForFunction(id=>document.querySelector('#compose-agent').value===id&&!document.querySelector('#compose-submit').disabled,agent);
 await page.locator('#compose-goal').fill(goal);
 const id=(await page.locator('#compose-binding').textContent()).split(' ').at(-1);
 if(lost)await page.route('**/api/tasks',async route=>{const response=await route.fetch();assert(response.ok(),'Submission before lost acknowledgement failed');await route.abort('failed');});
 await page.locator('#compose-submit').click();await confirm(page);
 if(lost){await page.locator('#compose-error').waitFor();await page.unroute('**/api/tasks');}
 else await page.locator('#task-detail .detail-id').filter({hasText:id}).waitFor();
 return id;
}
async function verify(page,value,label){
 await page.getByLabel('预期 JSON',{exact:true}).fill(JSON.stringify(value));
 await page.getByRole('button',{name:'核验工件',exact:true}).click();
 await page.locator('.verification-result > strong').filter({hasText:label}).first().waitFor();
}
try{
 const a=await node('panel-origin'),b=await node('panel-worker'),project=join(root,'project');mkdirSync(project);
 const config=join(root,'agent.toml');
 writeFileSync(config,['[agent]','name="panel-fixture"','harness="pi"','project_id="panel-test"','mode="managed"','[project]','root='+JSON.stringify(project),'allowed_tools=["read","write","edit"]','artifact_paths=["out/health.json"]','require_remote_task_approval=true','[adapter]','executable='+JSON.stringify(fixture),'provider="fixture"','model="fixture"','env_allowlist=[]',''].join('\n'));
 const agent=b.mesh.call(['agent','register','--config',config]);
 const pairing=a.mesh.call(['pair','request','--peer',b.card.endpoint,'--expect-fingerprint',b.card.fingerprint]);
 b.mesh.call(['pair','approve',pairing.request_id,'--expect-fingerprint',a.card.fingerprint]);a.mesh.call(['pair','approve',pairing.request_id,'--expect-fingerprint',b.card.fingerprint]);
 const policy=join(root,'policy.toml');
 writeFileSync(policy,['[default]','action="deny"','[peers]','default="confirm"','[[grant]]','peer="'+a.card.node_id+'"','agent="'+agent.agent_id+'"','project="panel-test"','actions=["agent.read","task.submit"]','require_approval=true',''].join('\n'));b.mesh.call(['policy','--file',policy]);
 // Explicit test double only, with an empty env allowlist and no credential reference.
 const probe=b.mesh.call(['agent','verify',agent.agent_id,'--allow-model','--prompt','fixture:create']);await succeeded(b.mesh,probe.task_id);
 const goal='<img src=x onerror="window.panelInjected=true"> synthetic pending request';
 const pending=a.mesh.call(['task','send',agent.agent_id,'--peer',b.card.node_id,'--project','panel-test','--prompt',goal]);assert(pending.status==='PENDING_APPROVAL','Expected owner approval');
 const ready=await panel(b),originReady=await panel(a);
 browser=await chromium.launch({headless:true,...(process.env.HMESH_CHROME?{executablePath:process.env.HMESH_CHROME}:{})});
 const page=await browser.newPage({viewport:{width:1440,height:1000},reducedMotion:'reduce'}),errors=[];page.on('pageerror',e=>errors.push(e.message));
 await page.goto(ready.url);await page.waitForLoadState('networkidle');await page.locator('#local-name').filter({hasText:'panel-worker'}).waitFor({state:'attached'});
 assert(!await page.evaluate(()=>!!window.panelInjected),'Task text executed');
 stage='reject-and-archive';
 await page.locator('[data-view="inbox"]').click();
 await page.locator('[data-work-area="tasks"]').click();await page.locator('[data-filter="pending"]').click();await page.locator('#task-list .task-title').first().click();await page.locator('#task-detail .detail-id').waitFor();
 assert(await page.locator('#task-detail > pre.goal').innerText()===goal,'Full request missing');
 await page.getByRole('button',{name:'拒绝任务',exact:true}).click();await confirm(page);
 await until(()=>b.mesh.call(['task','show',pending.task_id]).status==='REJECTED');
 await page.locator('#task-detail .task-outcome > .badge').filter({hasText:'已拒绝'}).waitFor();
 await page.getByRole('button',{name:'归档事件',exact:true}).click();
 const archiveResponse=page.waitForResponse(r=>r.url().endsWith('/archive')&&r.request().method()==='POST');await confirm(page);
 assert((await(await archiveResponse).json()).data.archived_events>0,'Archive did not move events');
 await page.locator('[data-close="task-dialog"]').click();
 stage='local-create-run-verify';
 const localId=await create(page,agent.agent_id,'fixture:create');
 // The owner's own task starts without a self-approval step.
 assert(b.mesh.call(['task','show',localId]).status!=='PENDING_APPROVAL','Owner task waited for self-approval');
 await succeeded(b.mesh,localId);
 await page.locator('#task-detail .task-outcome > .badge').filter({hasText:'执行成功'}).waitFor();await verify(page,{status:'ok'},'核验通过');
 writeFileSync(join(project,'out/health.json'),JSON.stringify({status:'later-edit'}));
 await verify(page,{status:'ok'},'核验通过');assert(await page.locator('#task-detail').innerText().then(t=>t.includes('完成时快照')),'Snapshot source missing');
 await page.locator('[data-close="task-dialog"]').click();
 stage='remote-lost-ack-recover';
 const origin=await browser.newPage({viewport:{width:1440,height:1000},reducedMotion:'reduce'});origin.on('pageerror',e=>errors.push(e.message));
 await origin.goto(originReady.url);await origin.waitForLoadState('networkidle');
 const remoteId=await create(origin,agent.agent_id,'fixture:create',b.card.node_id,true);
 const admitted=b.mesh.call(['task','show',remoteId]);assert(admitted.status==='PENDING_APPROVAL'&&!admitted.last_attempt_id,'Lost acknowledgement created an attempt');
 await origin.locator('#compose-submit').click();await confirm(origin);
 await origin.locator('#task-detail .detail-id').filter({hasText:remoteId}).waitFor();
 assert(b.mesh.call(['task','list']).executions.filter(t=>t.task_id===remoteId).length===1,'Recovery duplicated task');
 assert(!b.mesh.call(['task','show',remoteId]).last_attempt_id,'Recovery bypassed approval');
 b.mesh.call(['task','approve',remoteId]);const remote=await succeeded(b.mesh,remoteId);
 await origin.locator('#task-detail .task-outcome > .badge').filter({hasText:'执行成功'}).waitFor();
 stage='remote-artifact-verification';
 await verify(origin,{status:'wrong'},'核验未通过');await verify(origin,{status:'ok'},'核验通过');
 assert(b.mesh.call(['task','show',remoteId]).last_attempt_id===remote.last_attempt_id,'Verification reran task');
 assert(a.mesh.call(['task','list']).submitted.find(t=>t.task_id===remoteId).last_observed_at,'No persisted observation');
 await origin.locator('[data-close="task-dialog"]').click();
 stage='queue-wait-and-resume';
 const blockingId=await create(page,agent.agent_id,'fixture:wait');await until(()=>b.mesh.call(['task','show',blockingId]).status==='RUNNING');await page.locator('[data-close="task-dialog"]').click();
 const queuedId=await create(page,agent.agent_id,'fixture:create');
 await page.locator('#task-detail .queue-note').filter({hasText:'等待全局执行名额'}).waitFor();assert(!b.mesh.call(['task','show',queuedId]).last_attempt_id,'Waiting task started above concurrency cap');
 b.mesh.call(['task','cancel',blockingId]);await until(()=>b.mesh.call(['task','show',blockingId]).status==='CANCELLED');await succeeded(b.mesh,queuedId);await page.locator('#task-detail .task-outcome > .badge').filter({hasText:'执行成功'}).waitFor();await page.locator('[data-close="task-dialog"]').click();
 stage='agent-lifecycle';
 await page.locator('[data-view="agents"]').click();await page.getByRole('button',{name:'退役 Agent',exact:true}).click();await confirm(page);
 await until(()=>b.mesh.call(['agent','retired']).length===1);await page.locator('[data-agent-filter="retired"]').click();
 await page.getByRole('button',{name:'恢复 Agent',exact:true}).click();await confirm(page);
 await until(()=>b.mesh.call(['agents','list']).some(a=>a.agent_id===agent.agent_id&&a.probe.managed_run==='not_checked'));
 stage='onboarding-preview-register-recover';
 const beforePolicy=readFileSync(join(b.dir,'policy.toml'),'utf8');
 await page.locator('.theme-options label[title="深色"]').click();
 await page.reload();
 await checkTabIcon(page);
 await page.locator('[data-view="agents"]').click();await page.locator('#new-agent').click();
 assert(await page.locator('html').getAttribute('data-theme')==='dark','Appearance lost on reload');
 await checkProductArtwork(page,'dark');
 await page.locator('#close-onboard').click();
 await page.locator('.theme-options label[title="浅色"]').click();
 await page.locator('#new-agent').click();
 await checkProductArtwork(page,'light');
 assert(await page.locator('#onboard-submit').isDisabled(),'Wizard allowed an unselected Agent');
 assert(await page.locator('#onboard-harness label').count()===await page.locator('#onboard-harness .product-icon').count(),'Missing product icon in the Agent catalog');
 await page.locator('.onboard-choice').filter({hasText:'其他 ACP Agent'}).click();
 await page.locator('#onboard-executable').fill(fixture);
 await page.locator('.onboard-choice').filter({hasText:'Codex'}).click();
 assert(await page.locator('#onboard-executable').inputValue()!==fixture,'Manual path leaked to another Agent type');
 await page.locator('.onboard-choice').filter({hasText:'其他 ACP Agent'}).click();
 assert(await page.locator('#onboard-executable').inputValue()===fixture,'Returning to a type lost its manual path');
 await page.locator('#onboard-submit').click();
 await page.waitForFunction(()=>document.querySelector('#onboard-project').value==='panel-test');
 await page.locator('#onboard-name').fill('wizard-fixture');
 assert(await page.locator('#onboard-legacy-root').isHidden(),'Asked twice for the project folder');
 const setProjectHidden=hidden=>page.evaluate(hidden=>api('/projects/panel-test',{method:'POST',body:JSON.stringify({hidden})}).then(unwrap),hidden);
 await setProjectHidden(true);await page.locator('#close-onboard').click();
 await page.route('**/api/projects',route=>route.abort('failed'));await page.locator('#new-agent').click();
 await page.locator('#onboard-retry-projects').waitFor();
 assert(await page.locator('#onboard-submit').isDisabled()&&await page.locator('#onboard-add-project').isHidden(),'Failed read treated as an empty project list');
 await page.unroute('**/api/projects');await page.locator('#onboard-retry-projects').click();
 await page.locator('#onboard-add-project').waitFor();
 assert(await page.locator('#onboard-submit').isDisabled(),'Hidden project still selectable');
 await page.locator('#onboard-add-project').click();
 const secondFolder=join(root,'second-project');mkdirSync(secondFolder);
 await page.locator('#project-path').fill(secondFolder);await page.locator('#project-look').click();await page.locator('#project-add').click();
 await page.locator('#onboard-dialog[open]').waitFor();
 await page.waitForFunction(()=>document.querySelector('#onboard-project').selectedOptions[0]?.textContent==='second-project');
 await setProjectHidden(false);
 await page.locator('#onboard-back').click();
 await page.locator('#onboard-submit').click();
 await page.locator('#onboard-project').selectOption('panel-test');
 assert(await page.locator('#onboard-name').inputValue()==='wizard-fixture','Back navigation lost the Agent name');
 assert(await page.locator('#onboard-peer').isHidden(),'Advanced options are expanded by default');
 await page.locator('#onboard-advanced summary').click();
 await page.locator('#onboard-peer').selectOption(a.card.node_id);
 const previewResponse=page.waitForResponse(r=>r.url().endsWith('/onboarding/preview'));
 await page.getByRole('button',{name:'预览登记',exact:true}).click();const preview=(await(await previewResponse).json()).data;
 assert(preview.project_id==='panel-test'&&preview.project_root.endsWith('project'),'Project ID or folder changed in the preview');
 assert(preview.model_called===false&&preview.policy_applied===false&&preview.policy_preview.includes(preview.agent_id)&&preview.policy_preview.includes(a.card.node_id),'Exact preview failed');
 await page.route('**/api/onboarding/register',async route=>{const response=await route.fetch();assert(response.ok(),'Onboarding registration failed');await route.abort('failed');});
 await page.locator('#onboard-grant summary').click();
 assert((await page.locator('#onboard-policy').textContent()).includes(preview.agent_id),'Exact policy is missing from review');
 await page.getByRole('button',{name:'登记此 Agent',exact:true}).click();await page.waitForFunction(()=>document.querySelector('#onboard-message').textContent.length>0&&!document.querySelector('#onboard-submit').disabled);await page.unroute('**/api/onboarding/register');
 const firstRegistration=b.mesh.call(['agents','list']).filter(a=>a.agent_id===preview.agent_id);assert(firstRegistration.length===1,'First registration missing');
 await page.getByRole('button',{name:'登记此 Agent',exact:true}).click();await page.getByRole('heading',{name:'已登记 wizard-fixture',exact:true}).waitFor();
 const registered=b.mesh.call(['agents','list']).filter(a=>a.agent_id===preview.agent_id);assert(registered.length===1&&registered[0].probe.managed_run!=='passed','Registration duplicated or called a model');
 assert(readFileSync(join(b.dir,'policy.toml'),'utf8')===beforePolicy,'Wizard applied policy');
 await page.locator('#onboard-service').click();
 await page.getByRole('heading',{name:'wizard-fixture 的模型服务',exact:true}).waitFor();
 await page.locator('#close-service').click();
 stage='onboarding-detected-node';
 // An adapter installed with a Node.js other than PATH's first one: the preview carries that Node.js, a hand-edited path does not.
 if(await page.locator('#onboard-dialog[open]').count())await page.locator('#close-onboard').click();
 await page.route('**/api/agents/detect',route=>route.fulfill({json:{data:[{harness:'dsh',command:'dsh',executable:fixture,node:process.execPath}]}}));
 await page.locator('[data-view="agents"]').click();await page.locator('#new-agent').click();
 await page.locator('#onboard-status-dsh').filter({hasText:'本机已找到'}).waitFor();
 await page.locator('.onboard-choice').filter({hasText:'DeepSeek Harness'}).click();
 assert(await page.locator('#onboard-executable').inputValue()===fixture,'Detected DSH path missing');
 await page.locator('#onboard-submit').click();
 await page.waitForFunction(()=>document.querySelector('#onboard-project').options.length===3);
 assert(await page.locator('#onboard-project').inputValue()===''&&await page.locator('#onboard-submit').isDisabled(),'Multiple projects require an explicit choice');
 await page.locator('#onboard-project').selectOption('panel-test');await page.locator('#onboard-name').fill('node-fixture');
 const nodePreview=page.waitForRequest(r=>r.url().endsWith('/onboarding/preview'));await page.locator('#onboard-submit').click();
 assert((await nodePreview).postDataJSON().config.adapter.node_executable===process.execPath,'Detected Node.js missing from the registration');
 await page.locator('#onboard-summary').filter({hasText:process.execPath}).waitFor();
 await page.locator('#onboard-back').click();await page.locator('#onboard-back').click();
 await page.locator('#onboard-executable').fill(process.execPath);await page.locator('#onboard-submit').click();
 const manualPreview=page.waitForResponse(r=>r.url().endsWith('/onboarding/preview'));await page.locator('#onboard-submit').click();
 assert((await manualPreview).request().postDataJSON().config.adapter.node_executable===null,'Node.js kept for a hand-edited path');
 // Leave the wizard on its first step, as the later checks expect.
 await page.locator('#onboard-back:not([disabled])').click();await page.locator('#onboard-back').click();await page.locator('#onboard-harness').waitFor();
 await page.unroute('**/api/agents/detect');await page.locator('#close-onboard').click();
 stage='worktree-diff-review';
 const locate=spawnSync(process.platform==='win32'?'where.exe':'which',['git'],{encoding:'utf8'});assert(locate.status===0,'Git is required for worktree browser regression');const git=locate.stdout.trim().split(/\r?\n/)[0];
 mkdirSync(join(root,'disabled-test-hooks'));const gitCall=args=>assert(spawnSync(git,['-c','core.hooksPath='+join(root,'disabled-test-hooks'),'-c','core.fsmonitor=false','-C',project,...args],{encoding:'utf8',timeout:30000}).status===0,'Git browser fixture setup failed');
 gitCall(['init','-q']);writeFileSync(join(project,'out/health.json'),JSON.stringify({status:'base'}));gitCall(['add','.']);gitCall(['-c','user.name=fixture','-c','user.email=fixture@invalid','-c','commit.gpgsign=false','commit','-qm','fixture base']);
 const gitConfig=join(root,'git-agent.toml');writeFileSync(gitConfig,readFileSync(config,'utf8').replace('name="panel-fixture"','name="worktree-fixture"').replace('[adapter]','workspace_mode="git-worktree"\ngit_executable='+JSON.stringify(git)+'\n[adapter]'));
 const gitAgent=b.mesh.call(['agent','register','--config',gitConfig]);const gitTask=b.mesh.call(['agent','verify',gitAgent.agent_id,'--allow-model','--prompt','fixture:create']);await succeeded(b.mesh,gitTask.task_id);
 await page.locator('[data-view="inbox"]').click();
 await page.locator('[data-work-area="tasks"]').click();await page.locator('[data-filter="all"]').click();await page.locator('#task-list tr').filter({hasText:gitTask.task_id.slice(0,8)}).locator('.task-title').click();await page.getByRole('button',{name:'查看修改 diff',exact:true}).click();await page.locator('.workspace-diff').filter({hasText:'out/health.json'}).waitFor();await page.getByRole('button',{name:'diff 已读完',exact:true}).waitFor();
 assert(JSON.parse(readFileSync(join(project,'out/health.json'),'utf8')).status==='base','Worktree changed original project');await page.locator('[data-close="task-dialog"]').click();
 for(const [name,section] of [['sessions','sessions'],['inbox','inbox'],['agents','agents'],['nodes','nodes'],['settings','operations'],['settings','services']]){
  await page.locator('[data-view="'+name+'"]').click();
  assert(await page.locator('#'+section+'-view').isVisible(),'Navigation failed: '+name+' / '+section);
 }
 await page.locator('[data-view="inbox"]').click();
 await page.locator('[data-work-area="tasks"]').click();
 assert(await page.locator('#tasks-view').isVisible()&&await page.locator('#development-view').isVisible(),'Task history or development workflows missing from the work inbox');
 assert(await page.locator('[data-view="inbox"]').getAttribute('aria-current')==='page','Task history lost its work inbox navigation');
 assert(await page.locator('[data-view="overview"],[data-view="tasks"],[data-view="development"],[data-view="operations"],[data-view="services"]').count()===0,'Old navigation entries are still shown');
 // The version box lives on the settings page; elsewhere it shows only when something needs attention.
 await page.locator('[data-view="inbox"]').click();
 await page.locator('[data-work-area="tasks"]').click();assert(await page.locator('#runtime-summary').isHidden(),'Version box shown outside settings');
 await page.locator('[data-view="settings"]').click();assert(await page.locator('#runtime-summary').isVisible(),'Version box missing from settings');
 await page.setViewportSize({width:390,height:844});await page.locator('[data-view="inbox"]').click();await page.locator('[data-work-area="tasks"]').click();assert(!await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),'Mobile overflow');
 await page.setViewportSize({width:360,height:540});
  await page.locator('#new-task').click();
  assert(await page.evaluate(()=>{
   const scroll=document.querySelector('.delegation-scroll'),footer=document.querySelector('.delegation-footer');
   return scroll.scrollWidth<=scroll.clientWidth&&scroll.scrollHeight>scroll.clientHeight&&
    footer.getBoundingClientRect().bottom<=innerHeight;
  }),'Mobile delegation overflows or hides its actions');
  await page.locator('#close-compose').click();
 await page.locator('[data-view="agents"]').click();await page.locator('#new-agent').click();
 await page.locator('.onboard-choice').filter({hasText:'其他 ACP Agent'}).click();
 await page.locator('#onboard-executable').fill(fixture);await page.locator('#onboard-submit').click();
 await page.locator('#onboard-advanced summary').click();
 assert(await page.evaluate(()=>{
  const scroll=document.querySelector('.onboard-scroll'),footer=document.querySelector('.onboard-footer');
  return document.documentElement.scrollWidth<=innerWidth&&scroll.scrollWidth<=scroll.clientWidth&&
   scroll.scrollHeight>scroll.clientHeight&&footer.getBoundingClientRect().bottom<=innerHeight;
 }),'Mobile onboarding overflows or hides its actions');
 await page.locator('#close-onboard').click();
 const privatePage=await browser.newPage();await privatePage.goto(new URL(ready.url).origin);await privatePage.locator('#access-gate').waitFor();
 assert(!await page.evaluate(()=>!!window.panelInjected)&&!errors.length,'Browser error or script execution');
 stage='stop-panel-preserves-existing-daemon';
 await page.locator('#stop-panel').click();await confirm(page);
 await until(()=>owned.find(p=>p.pid===ready.pid)?.exitCode===0,20);
 assert(b.mesh.call(['daemon','status']).running===true,'Stopping a reused panel stopped its Daemon');
 console.log(JSON.stringify({passed:true,model_calls:0,fixture_runs:6,scope:'isolated two-node mTLS and headless browser',checks:['worktree-diff-review','queue-wait-reason','queue-resumes-after-cancel','completion-snapshot-after-file-edit','onboarding-existing-project-selection','onboarding-add-project-return','onboarding-reference-preview','onboarding-idempotent-register','onboarding-no-policy-or-model','onboarding-detected-node','mobile-onboarding','navigation','pending-approval','request-text-XSS','reject','archive','local-create','local-runs-without-self-approval','local-verify','remote-create','lost-acknowledgement','original-request-recovery','no-duplicate-attempt','remote-verification-fail','remote-verification-pass','observation-time','retire','restore-unverified','mobile','access-gate','stop-preserves-existing-daemon']}));
}catch(e){console.error(stage+': '+e.message+' diagnostics='+root);process.exitCode=1;}
finally{await browser?.close();for(const p of owned.reverse()){if(p.exitCode===null){p.kill();await Promise.race([once(p,'exit'),new Promise(r=>setTimeout(r,3000))]);}}}
