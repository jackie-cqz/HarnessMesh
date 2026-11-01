// Optional real-browser, real-TLS UI acceptance. Only the explicit fixture can execute.
import {spawn,spawnSync} from 'node:child_process';
import {mkdtempSync,mkdirSync,writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {Mesh,assert,until} from './acceptance/common.mjs';
const {chromium}=await import(process.env.HMESH_PLAYWRIGHT_MODULE||'playwright');
const root=mkdtempSync(join(tmpdir(),'hmesh-panel-')),binary=resolve('target/debug',process.platform==='win32'?'hmesh.exe':'hmesh'),fixture=resolve('target/debug',process.platform==='win32'?'hmesh-fixture.exe':'hmesh-fixture');
const owned=[];let browser;
function start(args,stdout='ignore'){const child=spawn(binary,args,{stdio:['ignore',stdout,'ignore'],windowsHide:true});owned.push(child);return child;}
async function node(name){const dir=join(root,name);const init=spawnSync(binary,['--json','--data-dir',dir,'init','--name',name,'--port','0'],{encoding:'utf8'});assert(init.status===0,'CLI init failed');const mesh=new Mesh(binary,dir);start(['--data-dir',dir,'daemon','start']);const card=await until(()=>{try{mesh.call(['daemon','status']);return mesh.call(['node','show']);}catch{return null;}});return{mesh,card,dir};}
try {
 const a=await node('panel-origin'),b=await node('panel-worker'),project=join(root,'project');mkdirSync(project);
 const config=join(root,'agent.toml');writeFileSync(config,`[agent]\nname="panel-fixture"\nharness="pi"\nproject_id="panel-test"\nmode="managed"\n[project]\nroot=${JSON.stringify(project)}\nallowed_tools=["read","write","edit"]\nartifact_paths=[]\nrequire_remote_task_approval=true\n[adapter]\nexecutable=${JSON.stringify(fixture)}\nprovider="fixture"\nmodel="fixture"\nenv_allowlist=[]\n`);
 const agent=b.mesh.call(['agent','register','--config',config]);
 const pair=a.mesh.call(['pair','request','--peer',b.card.endpoint,'--expect-fingerprint',b.card.fingerprint]);b.mesh.call(['pair','approve',pair.request_id,'--expect-fingerprint',a.card.fingerprint]);a.mesh.call(['pair','approve',pair.request_id,'--expect-fingerprint',b.card.fingerprint]);
 const policy=join(root,'policy.toml');writeFileSync(policy,`[default]\naction="deny"\n[[grant]]\npeer="${a.card.node_id}"\nagent="${agent.agent_id}"\nproject="panel-test"\nactions=["agent.read","task.submit"]\nrequire_approval=true\n`);b.mesh.call(['policy','--file',policy]);
 // Mark the explicit test double verified. No model, user credentials or external executable.
 const verified=b.mesh.call(['agent','verify',agent.agent_id,'--allow-model','--prompt','fixture:create']);await until(()=>{const t=b.mesh.call(['task','show',verified.task_id]);if(['FAILED','UNKNOWN','CANCELLED'].includes(t.status))throw Error('Fixture verification ended '+t.status);return t.status==='SUCCEEDED';});
 const goal='<img src=x onerror="window.panelInjected=true"> synthetic pending request';
 const pending=a.mesh.call(['task','send',agent.agent_id,'--peer',b.card.node_id,'--project','panel-test','--prompt',goal]);assert(pending.status==='PENDING_APPROVAL','Expected independent approval');
 const panel=start(['--json','--data-dir',b.dir,'panel','--port','0'],'pipe');const lines=createInterface({input:panel.stdout});const [line]=await Promise.race([once(lines,'line'),new Promise((_,reject)=>{const t=setTimeout(()=>reject(Error('Panel startup timeout')),20000);t.unref();})]);const ready=JSON.parse(line);lines.close();
 browser=await chromium.launch({headless:true,...(process.env.HMESH_CHROME?{executablePath:process.env.HMESH_CHROME}:{})});
 const page=await browser.newPage({viewport:{width:1440,height:1000}}),errors=[];page.on('pageerror',e=>errors.push(e.message));
 await page.goto(ready.url);await page.waitForLoadState('networkidle');await page.getByText('panel-worker',{exact:true}).first().waitFor();
 assert(!await page.evaluate(()=>!!window.panelInjected),'Task text executed as HTML');
 await page.locator('#review-pending').click();await page.locator('#task-list .task-title').first().click();await page.locator('#task-detail .detail-id').waitFor();
 assert((await page.locator('#task-detail .goal').innerText())===goal,'Full request not shown safely');
 await page.getByRole('button',{name:'拒绝任务',exact:true}).click();await page.getByRole('button',{name:'确认',exact:true}).click();await until(()=>b.mesh.call(['task','show',pending.task_id]).status==='REJECTED');
 await page.locator('#task-detail .badge').filter({hasText:'已拒绝'}).waitFor();await page.getByRole('button',{name:'归档事件',exact:true}).click();await page.getByRole('button',{name:'确认',exact:true}).click();await page.locator('#toast').filter({hasText:'操作已提交'}).waitFor();
 await page.locator('[data-close="task-dialog"]').click();await page.locator('[data-view="agents"]').click();await page.getByRole('button',{name:'退役 Agent',exact:true}).click();await page.getByRole('button',{name:'确认',exact:true}).click();await until(()=>b.mesh.call(['agent','retired']).length===1);
 await page.locator('[data-agent-filter="retired"]').click();await page.getByRole('button',{name:'恢复 Agent',exact:true}).click();await page.getByRole('button',{name:'确认',exact:true}).click();await until(()=>b.mesh.call(['agents','list']).some(a=>a.agent_id===agent.agent_id&&a.probe.managed_run==='not_checked'));
 for(const name of ['overview','tasks','agents','services','nodes']){await page.locator(`[data-view="${name}"]`).click();assert(await page.locator(`#${name}-view`).isVisible(),'Navigation failed');}
 await page.setViewportSize({width:390,height:844});await page.locator('[data-view="overview"]').click();assert(!await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),'Mobile layout overflow');
 const privatePage=await browser.newPage();await privatePage.goto(new URL(ready.url).origin);await privatePage.locator('#access-gate').waitFor();
 assert(!await page.evaluate(()=>!!window.panelInjected)&&errors.length===0,'Browser script error');
 console.log(JSON.stringify({passed:true,model_calls:0,scope:'isolated two-node TLS and headless browser',checks:['navigation','pending-approval','request-text-XSS','reject','archive','retire','restore-unverified','mobile','access-gate']}));
}catch(e){console.error(e.message+' diagnostics='+root);process.exitCode=1;}
finally{await browser?.close();for(const p of owned.reverse()){if(p.exitCode===null){p.kill();await Promise.race([once(p,'exit'),new Promise(r=>setTimeout(r,3000))]);}}}
