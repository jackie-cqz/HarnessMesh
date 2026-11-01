// Real browser + two real TLS Nodes; the only executable Agent is the test fixture.
import {spawn,spawnSync} from 'node:child_process';
import {mkdtempSync,mkdirSync,writeFileSync} from 'node:fs';
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
async function create(page,agent,goal,peer,lost=false){
 await page.locator('#new-task').click();
 if(peer)await page.locator('#compose-peer').selectOption(peer);
 await page.waitForFunction(id=>document.querySelector('#compose-agent').value===id&&!document.querySelector('#compose-submit').disabled,agent);
 await page.locator('#compose-goal').fill(goal);
 const id=(await page.locator('#compose-binding').innerText()).split(' ').at(-1);
 if(lost)await page.route('**/api/tasks',async route=>{const response=await route.fetch();assert(response.ok(),'Submission before lost acknowledgement failed');await route.abort('failed');});
 await page.getByRole('button',{name:'审阅并提交',exact:true}).click();await confirm(page);
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
 writeFileSync(policy,['[default]','action="deny"','[[grant]]','peer="'+a.card.node_id+'"','agent="'+agent.agent_id+'"','project="panel-test"','actions=["agent.read","task.submit"]','require_approval=true',''].join('\n'));b.mesh.call(['policy','--file',policy]);
 // Explicit test double only, with an empty env allowlist and no credential reference.
 const probe=b.mesh.call(['agent','verify',agent.agent_id,'--allow-model','--prompt','fixture:create']);await succeeded(b.mesh,probe.task_id);
 const goal='<img src=x onerror="window.panelInjected=true"> synthetic pending request';
 const pending=a.mesh.call(['task','send',agent.agent_id,'--peer',b.card.node_id,'--project','panel-test','--prompt',goal]);assert(pending.status==='PENDING_APPROVAL','Expected owner approval');
 const ready=await panel(b),originReady=await panel(a);
 browser=await chromium.launch({headless:true,...(process.env.HMESH_CHROME?{executablePath:process.env.HMESH_CHROME}:{})});
 const page=await browser.newPage({viewport:{width:1440,height:1000},reducedMotion:'reduce'}),errors=[];page.on('pageerror',e=>errors.push(e.message));
 await page.goto(ready.url);await page.waitForLoadState('networkidle');await page.getByText('panel-worker',{exact:true}).first().waitFor();
 assert(!await page.evaluate(()=>!!window.panelInjected),'Task text executed');
 stage='reject-and-archive';
 await page.locator('#review-pending').click();await page.locator('#task-list .task-title').first().click();await page.locator('#task-detail .detail-id').waitFor();
 assert(await page.locator('#task-detail .goal').innerText()===goal,'Full request missing');
 await page.getByRole('button',{name:'拒绝任务',exact:true}).click();await confirm(page);
 await until(()=>b.mesh.call(['task','show',pending.task_id]).status==='REJECTED');
 await page.locator('#task-detail > .badge').filter({hasText:'已拒绝'}).waitFor();
 await page.getByRole('button',{name:'归档事件',exact:true}).click();
 const archiveResponse=page.waitForResponse(r=>r.url().endsWith('/archive')&&r.request().method()==='POST');await confirm(page);
 assert((await(await archiveResponse).json()).data.archived_events>0,'Archive did not move events');
 await page.locator('[data-close="task-dialog"]').click();
 stage='local-create-approve-verify';
 const localId=await create(page,agent.agent_id,'fixture:create');
 assert(b.mesh.call(['task','show',localId]).status==='PENDING_APPROVAL','New local task bypassed approval');
 assert(!b.mesh.call(['task','show',localId]).last_attempt_id,'Attempt before approval');
 await page.getByRole('button',{name:'批准执行',exact:true}).click();await confirm(page);await succeeded(b.mesh,localId);
 await page.locator('#task-detail > .badge').filter({hasText:'已完成'}).waitFor();await verify(page,{status:'ok'},'核验通过');
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
 await origin.locator('#task-detail > .badge').filter({hasText:'已完成'}).waitFor();
 stage='remote-artifact-verification';
 await verify(origin,{status:'wrong'},'核验未通过');await verify(origin,{status:'ok'},'核验通过');
 assert(b.mesh.call(['task','show',remoteId]).last_attempt_id===remote.last_attempt_id,'Verification reran task');
 assert(a.mesh.call(['task','list']).submitted.find(t=>t.task_id===remoteId).last_observed_at,'No persisted observation');
 await origin.locator('[data-close="task-dialog"]').click();
 stage='agent-lifecycle';
 await page.locator('[data-view="agents"]').click();await page.getByRole('button',{name:'退役 Agent',exact:true}).click();await confirm(page);
 await until(()=>b.mesh.call(['agent','retired']).length===1);await page.locator('[data-agent-filter="retired"]').click();
 await page.getByRole('button',{name:'恢复 Agent',exact:true}).click();await confirm(page);
 await until(()=>b.mesh.call(['agents','list']).some(a=>a.agent_id===agent.agent_id&&a.probe.managed_run==='not_checked'));
 for(const name of ['overview','tasks','agents','services','nodes']){await page.locator('[data-view="'+name+'"]').click();assert(await page.locator('#'+name+'-view').isVisible(),'Navigation failed');}
 await page.setViewportSize({width:390,height:844});await page.locator('[data-view="overview"]').click();assert(!await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),'Mobile overflow');
 const privatePage=await browser.newPage();await privatePage.goto(new URL(ready.url).origin);await privatePage.locator('#access-gate').waitFor();
 assert(!await page.evaluate(()=>!!window.panelInjected)&&!errors.length,'Browser error or script execution');
 stage='stop-panel-preserves-existing-daemon';
 await page.locator('#stop-panel').click();await confirm(page);
 await until(()=>owned.find(p=>p.pid===ready.pid)?.exitCode===0,20);
 assert(b.mesh.call(['daemon','status']).running===true,'Stopping a reused panel stopped its Daemon');
 console.log(JSON.stringify({passed:true,model_calls:0,fixture_runs:3,scope:'isolated two-node mTLS and headless browser',checks:['navigation','pending-approval','request-text-XSS','reject','archive','local-create','local-approval','local-verify','remote-create','lost-acknowledgement','original-request-recovery','no-duplicate-attempt','remote-verification-fail','remote-verification-pass','observation-time','retire','restore-unverified','mobile','access-gate','stop-preserves-existing-daemon']}));
}catch(e){console.error(stage+': '+e.message+' diagnostics='+root);process.exitCode=1;}
finally{await browser?.close();for(const p of owned.reverse()){if(p.exitCode===null){p.kill();await Promise.race([once(p,'exit'),new Promise(r=>setTimeout(r,3000))]);}}}
