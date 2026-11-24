// Real browser + two real TLS nodes driving agent sessions. The only agent is
// the ACP test fixture; no model is called.
import {spawn,spawnSync} from 'node:child_process';
import {mkdtempSync,mkdirSync,writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {Mesh,assert,until} from './acceptance/common.mjs';
const {chromium}=await import(process.env.HMESH_PLAYWRIGHT_MODULE||'playwright');
const root=mkdtempSync(join(tmpdir(),'hmesh-sessions-'));
const shots=process.env.HMESH_SCREENSHOT_DIR||root;
const binary=resolve('target/debug',process.platform==='win32'?'hmesh.exe':'hmesh');
const fixture=resolve('target/debug',process.platform==='win32'?'hmesh-fixture.exe':'hmesh-fixture');
const owned=[];let browser;let stage='setup';
function start(args,stdout='ignore'){const child=spawn(binary,args,{stdio:['ignore',stdout,'ignore'],windowsHide:true});owned.push(child);return child;}
async function node(name){
 const dir=join(root,name),init=spawnSync(binary,['--json','--data-dir',dir,'init','--name',name,'--port','0'],{encoding:'utf8'});
 assert(init.status===0,'CLI init failed');const mesh=new Mesh(binary,dir);start(['--data-dir',dir,'daemon','start']);
 const card=await until(()=>{try{mesh.call(['daemon','status']);return mesh.call(['node','show']);}catch{return null;}});
 return{mesh,card,dir};
}
function register(node,name){
 const project=join(root,`${name}-project`);mkdirSync(project);const config=join(root,`${name}.toml`);
 writeFileSync(config,['[agent]',`name="${name}"`,'harness="acp"','project_id="demo"','[project]','root='+JSON.stringify(project),'allowed_tools=["read","write","edit"]','[adapter]','executable='+JSON.stringify(fixture),'env_allowlist=[]',''].join('\n'));
 return node.mesh.call(['agent','register','--config',config]);
}
async function panel(node){
 const child=start(['--json','--data-dir',node.dir,'panel','--port','0'],'pipe');
 const lines=createInterface({input:child.stdout});
 const[line]=await Promise.race([once(lines,'line'),new Promise((_,reject)=>{const t=setTimeout(()=>reject(Error('Panel startup timeout')),20000);t.unref();})]);
 lines.close();return JSON.parse(line);
}
async function openSession(page,nodeId,agentId,message){
 await page.locator('#new-session').click();
 await page.locator('#new-session-node').selectOption(nodeId);
 await page.locator('#new-session-agent').selectOption(agentId);
 await page.locator('#new-session-message').fill(message);
 await page.locator('#new-session-submit').click();
 await page.locator('#new-session-dialog').waitFor({state:'hidden'});
}
const stream=page=>page.locator('#conversation-stream');
try{
 const a=await node('mac-a'),b=await node('mac-b');
 const local=register(a,'helper'),remote=register(b,'worker');
 assert(remote.probe.protocol_probe==='acp_initialize_passed','fixture did not pass the ACP initialize check');
 const pairing=a.mesh.call(['pair','request','--peer',b.card.endpoint,'--expect-fingerprint',b.card.fingerprint]);
 b.mesh.call(['pair','approve',pairing.request_id,'--expect-fingerprint',a.card.fingerprint]);a.mesh.call(['pair','approve',pairing.request_id,'--expect-fingerprint',b.card.fingerprint]);
 const ready=await panel(a);
 browser=await chromium.launch({headless:true});const page=await browser.newPage({viewport:{width:1440,height:960}});
 const problems=[];page.on('pageerror',e=>problems.push(e.message));page.on('console',m=>{if(m.type()==='error')problems.push(m.text());});
 await page.goto(ready.url);await page.locator('#local-name').filter({hasText:'mac-a'}).waitFor();
 stage='open remote session';
 await page.locator('[data-view="sessions"]').click();
 await page.locator('#conversation-empty').waitFor();
 // The agent directory lists this computer's and the paired computer's agents.
 await page.locator('#session-agents .agent-group').filter({hasText:'mac-a（本机）'}).getByText('helper').waitFor();
 await page.locator('#session-agents .agent-group').filter({hasText:'mac-b'}).getByText('worker').waitFor();
 await page.screenshot({path:join(shots,'sessions-empty.png')});
 // One click on an agent opens the dialog with it selected.
 await page.locator('#session-agents .agent-group').filter({hasText:'mac-b'}).getByRole('button',{name:/worker/}).click();
 await page.waitForFunction(([node,agent])=>document.querySelector('#new-session-dialog').open&&document.querySelector('#new-session-node').value===node&&document.querySelector('#new-session-agent').value===agent,[b.card.node_id,remote.agent_id],{timeout:10000});
 await page.locator('#new-session-message').fill('fixture:permission');
 await page.locator('#new-session-submit').click();await page.locator('#new-session-dialog').waitFor({state:'hidden'});
 // The agent on mac-b asks for permission; mac-a opened the session, so mac-a answers.
 stage='answer permission from the opener';
 await page.locator('.permission').getByRole('button',{name:'允许一次'}).waitFor({timeout:30000});
 await page.screenshot({path:join(shots,'sessions-permission.png')});
 await page.locator('.permission').getByRole('button',{name:'允许一次'}).click();
 await stream(page).getByText('Permission granted: notes.txt edited.').waitFor({timeout:30000});
 await stream(page).getByText('已允许（打开者）').waitFor();
 stage='follow-up turn';
 await page.waitForFunction(()=>!document.querySelector('#composer-send').disabled);
 await page.locator('#composer-input').fill('fixture:echo from the panel');
 await page.locator('#composer-send').click();
 await stream(page).getByText('Echo #2: from the panel').waitFor({timeout:30000});
 assert(await page.locator('#conversation-header').getByText('第 2 轮').count()===1,'turn count not shown');
 await page.screenshot({path:join(shots,'sessions-remote.png')});
 stage='reload keeps the conversation';
 await page.reload();await page.locator('#local-name').filter({hasText:'mac-a'}).waitFor();
 await page.locator('[data-view="sessions"]').click();
 await page.locator('.session-row').filter({hasText:'fixture:permission'}).first().click();
 await stream(page).getByText('Echo #2: from the panel').waitFor({timeout:30000});
 await stream(page).getByText('Permission granted: notes.txt edited.').waitFor();
 stage='agent delegates across computers';
 await openSession(page,a.card.node_id,local.agent_id,'fixture:delegate mac-b/worker fixture:reply');
 await stream(page).getByText(/Delegated to mac-b\/worker: Final answer: the build is green\./).waitFor({timeout:60000});
 // The child session the agent opened on mac-b is listed with its caller.
 await page.locator('[data-view="sessions"]').click();
 const child=page.locator('.session-row').filter({hasText:'由 mac-a / helper 调用'});
 await until(async()=>{await page.evaluate(()=>loadSessions());return (await child.count())>0;},30,'child session not listed');
 await page.screenshot({path:join(shots,'sessions-delegation.png')});
 await child.first().click();
 await page.locator('#conversation-header').getByText('工具权限自动允许').waitFor();
 await page.locator('.conversation-chain').getByText(/调用链：mac-a.* → mac-b/).waitFor();
 stage='stop and close';
 await page.locator('.session-row').filter({hasText:'fixture:permission'}).first().click();
 await page.locator('#composer-input').fill('fixture:wait');await page.locator('#composer-send').click();
 await page.getByRole('button',{name:'停止本轮'}).click();
 await stream(page).getByText('本轮已停止').waitFor({timeout:30000});
 await page.getByRole('button',{name:'关闭会话'}).click();await page.getByRole('button',{name:'确认',exact:true}).click();
 await page.locator('#conversation-header .badge').filter({hasText:'已关闭'}).waitFor();
 assert(await page.locator('#composer-send').isDisabled(),'closed session still accepts messages');
 stage='narrow window';
 await page.setViewportSize({width:720,height:900});await page.screenshot({path:join(shots,'sessions-narrow.png'),fullPage:true});
 assert(problems.length===0,`browser errors: ${problems.join(' | ')}`);
 console.log(JSON.stringify({ok:true,screenshots:shots}));
}catch(e){console.error(`sessions panel smoke failed at ${stage}: ${e.stack||e}`);process.exitCode=1;}
finally{await browser?.close();for(const child of owned.reverse()){try{child.kill('SIGTERM');}catch{}}}
