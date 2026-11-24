// Real browser + two real TLS nodes driving agent sessions. The only agent is
// the ACP test fixture; no model is called.
import {spawn,spawnSync} from 'node:child_process';
import {mkdtempSync,mkdirSync,writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {createServer} from 'node:http';
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
function register(node,name,harness='acp',args=[]){
 const project=join(root,`${name}-project`);mkdirSync(project);const config=join(root,`${name}.toml`);
 writeFileSync(config,['[agent]',`name="${name}"`,`harness="${harness}"`,`project_id="${harness==='acp'?'demo':'demo-'+name}"`,'[project]','root='+JSON.stringify(project),'allowed_tools=["read","write","edit"]','[adapter]','executable='+JSON.stringify(fixture),'args='+JSON.stringify(args),'env_allowlist=[]',''].join('\n'));
 return node.mesh.call(['agent','register','--config',config]);
}
async function panel(node){
 const child=start(['--json','--data-dir',node.dir,'panel','--port','0'],'pipe');
 const lines=createInterface({input:child.stdout});
 const[line]=await Promise.race([once(lines,'line'),new Promise((_,reject)=>{const t=setTimeout(()=>reject(Error('Panel startup timeout')),20000);t.unref();})]);
 lines.close();return JSON.parse(line);
}
// Pick an agent in the sidebar, then send the first message from the new-session screen.
async function openSession(page,group,agentName,message){
 await page.locator('#session-agents .session-group').filter({has:page.locator('.session-group-name',{hasText:new RegExp('^'+group+'$')})}).getByRole('button',{name:new RegExp(agentName)}).click();
 await page.locator('#conversation-empty').waitFor();
 await page.locator('#hero-target').filter({hasText:agentName}).waitFor();
 await page.locator('#hero-input').fill(message);
 await page.locator('#hero-send').click();
 await page.locator('#conversation').waitFor();
}
// A model service on loopback that lists two models and records what it receives.
async function modelService(){
 const seen=[];
 const server=createServer((req,res)=>{seen.push({method:req.method,url:req.url,authorization:req.headers.authorization||''});
  if(req.method==='GET'&&req.url==='/v1/models'){res.writeHead(200,{'content-type':'application/json'});res.end(JSON.stringify({object:'list',data:[{id:'m-one'},{id:'m-two'}]}));}
  else if(req.method==='GET'&&req.url==='/many/v1/models'){res.writeHead(200,{'content-type':'application/json'});res.end(JSON.stringify({object:'list',data:Array.from({length:120},(_,i)=>({id:'bulk-'+i}))}));}
  else{res.writeHead(404);res.end('not here');}
 });
 server.listen(0,'127.0.0.1');await once(server,'listening');server.unref();
 return{base:`http://127.0.0.1:${server.address().port}/v1`,seen,server};
}
const stream=page=>page.locator('#conversation-stream');
const settle=page=>page.waitForTimeout(400);
try{
 const a=await node('mac-a'),b=await node('mac-b');
 const local=register(a,'helper'),remote=register(b,'worker');register(a,'coder','opencode');
 // Fixture agents answering ACP auth/status the way claude-agent-acp does.
 register(a,'claude','claude-code',['--auth-status','subscription']);register(a,'stranger','claude-code',['--auth-status','none']);
 const models=await modelService();
 assert(remote.probe.protocol_probe==='acp_initialize_passed','fixture did not pass the ACP initialize check');
 const pairing=a.mesh.call(['pair','request','--peer',b.card.endpoint,'--expect-fingerprint',b.card.fingerprint]);
 b.mesh.call(['pair','approve',pairing.request_id,'--expect-fingerprint',a.card.fingerprint]);a.mesh.call(['pair','approve',pairing.request_id,'--expect-fingerprint',b.card.fingerprint]);
 const ready=await panel(a);
 browser=await chromium.launch({headless:true});const page=await browser.newPage({viewport:{width:1440,height:960}});
 const problems=[];page.on('pageerror',e=>problems.push(e.message));page.on('console',m=>{if(m.type()==='error')problems.push(m.text());});
 await page.goto(ready.url);await page.locator('#local-name').filter({hasText:'mac-a'}).waitFor();
 stage='sidebar lists both computers\' agents';
 await page.locator('#session-agents .session-group').filter({hasText:'mac-a（本机）'}).getByText('helper').waitFor();
 await page.locator('#session-agents .session-group').filter({hasText:'mac-b'}).getByText('worker').waitFor();
 await settle(page);await page.screenshot({path:join(shots,'overview-light.png')});
 await page.locator('#new-session').click();await page.locator('#conversation-empty').waitFor();
 assert(await page.locator('#hero-agents .agent-chip').count()===5,'new-session screen does not offer every agent');
 await settle(page);await page.screenshot({path:join(shots,'sessions-empty.png')});
 stage='open remote session';
 await openSession(page,'mac-b','worker','fixture:permission');
 // The agent on mac-b asks for permission; mac-a opened the session, so mac-a answers.
 stage='answer permission from the opener';
 await page.locator('#permission-dock .permission').getByRole('button',{name:'允许一次'}).waitFor({timeout:30000});
 await page.screenshot({path:join(shots,'sessions-permission.png')});
 await page.locator('#permission-dock .permission').getByRole('button',{name:'允许一次'}).click();
 await stream(page).getByText('Permission granted: notes.txt edited.').waitFor({timeout:30000});
 await stream(page).getByText('已允许（打开者）').waitFor();
 assert(await page.locator('#permission-dock .permission').count()===0,'answered request still docked');
 stage='follow-up turn';
 await page.waitForFunction(()=>!document.querySelector('#composer-send').disabled);
 await page.locator('#composer-input').fill('fixture:echo from the panel');
 await page.locator('#composer-send').click();
 await stream(page).getByText('Echo #2: from the panel').waitFor({timeout:30000});
 await page.locator('#conversation-meta').getByText('第 2 轮').waitFor();
 // Tool calls are collapsed rows that open to show their details.
 const tool=stream(page).locator('details.tool').first();
 await tool.locator('summary').click();assert(await tool.evaluate(d=>d.open),'tool row did not expand');
 await page.screenshot({path:join(shots,'sessions-remote.png')});
 stage='switch the model between turns';
 // The agent offers its settings; the opener switches them between turns.
 await page.locator('#composer-settings select[aria-label="模型"]').selectOption('fixture-large');
 await stream(page).getByText('已切换模型：fixture-large').waitFor({timeout:30000});
 // The large model adds a reasoning setting.
 await page.locator('#composer-settings select[aria-label="思考强度"]').waitFor({timeout:30000});
 await page.waitForFunction(()=>!document.querySelector('#composer-send').disabled);
 await page.locator('#composer-input').fill('fixture:config');await page.locator('#composer-send').click();
 await stream(page).getByText('model=fixture-large mode=ask effort=low').waitFor({timeout:30000});
 await settle(page);await page.screenshot({path:join(shots,'settings-composer.png')});
 stage='reload keeps the conversation';
 await page.reload();await page.locator('#local-name').filter({hasText:'mac-a'}).waitFor();
 await page.locator('.session-row').filter({hasText:'fixture:permission'}).first().click();
 await stream(page).getByText('Echo #2: from the panel').waitFor({timeout:30000});
 await stream(page).getByText('Permission granted: notes.txt edited.').waitFor();
 stage='new session with chosen settings';
 // Settings the agent offered last time can be chosen before the first message.
 await page.locator('#new-session').click();await page.locator('#conversation-empty').waitFor();
 await page.locator('#hero-agents .agent-chip').filter({hasText:'worker'}).click();
 const heroModel=page.locator('#hero-settings select[aria-label="模型"]');await heroModel.waitFor({timeout:30000});
 await heroModel.selectOption('fixture-large');await page.locator('#hero-settings select[aria-label="模式"]').selectOption('code');
 await settle(page);await page.screenshot({path:join(shots,'settings-hero.png')});
 await page.locator('#hero-input').fill('fixture:config');await page.locator('#hero-send').click();
 await page.locator('#conversation').waitFor();
 await stream(page).getByText('model=fixture-large mode=code effort=low').waitFor({timeout:30000});
 stage='agent delegates across computers';
 await openSession(page,'mac-a（本机）','helper','fixture:delegate mac-b/worker fixture:reply');
 await stream(page).getByText(/Delegated to mac-b\/worker: Final answer: the build is green\./).waitFor({timeout:60000});
 // The session the agent opened on mac-b is listed with its caller.
 const child=page.locator('.session-row[title*="由 mac-a / helper 调用"]');
 await until(async()=>{await page.evaluate(()=>loadSessions());return (await child.count())>0;},30,'child session not listed');
 await page.screenshot({path:join(shots,'sessions-delegation.png')});
 await child.first().click();
 await page.locator('#conversation-meta').getByText('工具权限自动允许').waitFor();
 await page.locator('.conversation-chain').getByText(/调用链：mac-a → mac-b/).waitFor();
 stage='stop and close';
 await page.locator('.session-row').filter({hasText:'fixture:permission'}).first().click();
 await page.waitForFunction(()=>!document.querySelector('#composer-send').disabled);
 await page.locator('#composer-input').fill('fixture:wait');await page.locator('#composer-send').click();
 await page.getByRole('button',{name:'停止本轮'}).click();
 await stream(page).getByText('本轮已停止').waitFor({timeout:30000});
 await page.getByRole('button',{name:'关闭会话'}).click();await page.getByRole('button',{name:'确认',exact:true}).click();
 await page.locator('#conversation-header .badge').filter({hasText:'已关闭'}).waitFor();
 assert(await page.locator('#composer-send').isDisabled(),'closed session still accepts messages');
 stage='model service on the agents page';
 const secret='fixture-secret-0123456789';
 await page.locator('[data-view="agents"]').click();
 const helperCard=page.locator('#agent-list .entity-card').filter({hasText:'helper'});
 await helperCard.getByRole('button',{name:'模型服务'}).click();
 await page.locator('#service-dialog[open]').waitFor();
 await page.locator('#service-env-box summary').click();await page.locator('#service-env-add').click();
 const envRow=page.locator('#service-env .service-env-row').last();
 await envRow.locator('.env-name').fill('FIXTURE_SERVICE_TOKEN');await envRow.locator('.env-value').fill(secret);
 await page.locator('#service-model').fill('fixture-large');
 await settle(page);await page.screenshot({path:join(shots,'service-dialog.png')});
 await page.locator('#service-save').click();
 await helperCard.getByText('单独配置').waitFor({timeout:30000});
 // Reopened, the value is gone; only its last characters are shown.
 await helperCard.getByRole('button',{name:'模型服务'}).click();
 await page.locator('#service-dialog[open]').waitFor();
 const hint=await page.locator('#service-env .env-value').first().getAttribute('placeholder');
 assert(hint.includes(secret.slice(-4))&&await page.locator('#service-env .env-value').first().inputValue()==='','stored value shown again: '+hint);
 assert(!(await page.content()).includes(secret),'secret visible in the page');
 await page.locator('#service-cancel').click();
 // The agent process receives it; the conversation shows it redacted.
 await openSession(page,'mac-a（本机）','helper','fixture:env FIXTURE_SERVICE_TOKEN');
 await stream(page).getByText(/FIXTURE_SERVICE_TOKEN len=25 fnv=[0-9a-f]{16} value=\[REDACTED\]/).waitFor({timeout:30000});
 assert(!(await page.content()).includes(secret),'secret visible in the conversation');
 stage='model list and connection test';
 const coderCard=page.locator('#agent-list .entity-card').filter({hasText:'coder'});
 await page.locator('[data-view="agents"]').click();
 await coderCard.getByRole('button',{name:'模型服务'}).click();
 await page.locator('#service-dialog[open]').waitFor();
 await page.locator('#service-modes').getByRole('radio',{name:/自定义地址/}).click();
 assert(await page.locator('#service-model-list').isVisible()&&await page.locator('#service-test').isVisible(),'model list or test button missing for OpenCode with a custom address');
 const serviceKey='sk-panel-0123456789';
 await page.locator('#service-url').fill(models.base);await page.locator('#service-key').fill(serviceKey);
 await page.locator('#service-test').click();
 await page.locator('#service-test-result.ok').filter({hasText:'这个服务列出了 2 个模型'}).waitFor({timeout:30000});
 assert(models.seen.length===1&&models.seen[0].url==='/v1/models'&&models.seen[0].authorization===`Bearer ${serviceKey}`,'connection test did not ask for the model list once: '+JSON.stringify(models.seen));
 await page.locator('#service-fill').click();
 assert(await page.locator('#service-model-list').inputValue()==='m-one\nm-two','models not filled in');
 await settle(page);await page.screenshot({path:join(shots,'service-test.png')});
 await page.locator('#service-save').click();
 await coderCard.getByText('hmesh/m-one').waitFor({timeout:30000});
 await coderCard.getByRole('button',{name:'模型服务'}).click();
 await page.locator('#service-dialog[open]').waitFor();
 assert(await page.locator('#service-model-list').inputValue()==='m-one\nm-two','saved model list not shown again');
 assert(!(await page.content()).includes(serviceKey),'service key visible in the page');
 // A service listing more models than a list may hold offers no one-click fill.
 await page.locator('#service-url').fill(models.base.replace('/v1','/many/v1'));
 await page.locator('#service-test').click();
 await page.locator('#service-test-result.ok').filter({hasText:'超过 100 个'}).waitFor({timeout:30000});
 assert(await page.locator('#service-fill').isHidden(),'fill offered beyond the model limit');
 // A wrong address is reported in the dialog, not as a page error.
 await page.locator('#service-url').fill(models.base.replace('/v1','/missing'));
 await page.locator('#service-test').click();
 await page.locator('#service-test-result.failed').waitFor({timeout:30000});
 await page.locator('#service-cancel').click();
 stage='its own login and presets';
 const openFor=async name=>{await page.locator('#agent-list .entity-card').filter({has:page.locator('.entity-name',{hasText:new RegExp('^'+name+'$')})}).getByRole('button',{name:'模型服务'}).click();await page.locator('#service-dialog[open]').waitFor();};
 await openFor('claude');
 await page.locator('#service-own.configured').filter({hasText:'它自己已经配好了'}).waitFor({timeout:30000});
 const own=await page.locator('#service-own').innerText();
 assert(own.includes('fixture@example.com')&&own.includes('Claude 订阅（max 套餐）'),'own login details missing: '+own);
 await settle(page);await page.screenshot({path:join(shots,'service-own.png')});
 await page.locator('#service-modes').getByRole('radio',{name:/自定义地址/}).click();
 assert(await page.locator('#service-preset').isVisible(),'no presets for Claude Code');
 const presetUrls=await page.locator('#service-preset option').evaluateAll(o=>o.map(x=>x.value).filter(Boolean));
 assert(presetUrls.includes('deepseek')&&!presetUrls.includes('openai'),'Claude Code presets not limited to Anthropic endpoints: '+presetUrls);
 await page.locator('#service-preset').selectOption('deepseek');
 assert(await page.locator('#service-url').inputValue()==='https://api.deepseek.com/anthropic','preset address not filled');
 await page.locator('#service-key-from-env').check();
 assert(await page.locator('#service-key-env').inputValue()==='DEEPSEEK_API_KEY'&&await page.locator('#service-key').isHidden(),'environment key name not offered');
 await page.locator('#service-model').fill('deepseek-v4-pro');
 await settle(page);await page.screenshot({path:join(shots,'service-preset.png')});
 await page.locator('#service-save').click();
 await page.locator('#service-dialog').waitFor({state:'hidden'});
 await openFor('claude');
 await page.locator('#service-own.configured').filter({hasText:'现在用的是这里单独配的设置'}).waitFor({timeout:30000});
 assert(await page.locator('#service-key-env').inputValue()==='DEEPSEEK_API_KEY','saved environment key name not shown');
 await page.locator('#service-cancel').click();
 await openFor('stranger');
 await page.locator('#service-own.not_configured').filter({hasText:'在终端运行 claude 并登录'}).waitFor({timeout:30000});
 await page.locator('#service-cancel').click();
 stage='dark theme';
 await page.emulateMedia({colorScheme:'dark'});
 await page.locator('.session-row').filter({hasText:'fixture:reply'}).first().click();
 await stream(page).getByText(/Final answer/).first().waitFor();
 const shade=await page.evaluate(()=>{const lum=c=>{const m=c.match(/[\d.]+/g).map(Number);return (0.2126*m[0]+0.7152*m[1]+0.0722*m[2])/255;};return {page:lum(getComputedStyle(document.body).backgroundColor),sidebar:lum(getComputedStyle(document.querySelector('.sidebar')).backgroundColor),text:lum(getComputedStyle(document.querySelector('.msg.agent .msg-text')).color)};});
 assert(shade.page<0.1&&shade.sidebar<0.15&&shade.sidebar>=shade.page&&shade.text>0.8,`dark theme not applied: ${JSON.stringify(shade)}`);
 await settle(page);await page.screenshot({path:join(shots,'sessions-dark.png')});
 await page.locator('#new-session').click();await settle(page);await page.screenshot({path:join(shots,'hero-dark.png')});
 await page.locator('[data-view="overview"]').click();await settle(page);await page.screenshot({path:join(shots,'overview-dark.png')});
 await page.locator('[data-view="agents"]').click();await settle(page);await page.screenshot({path:join(shots,'agents-dark.png')});
 await page.locator('#agent-list .entity-card').filter({hasText:'helper'}).getByRole('button',{name:'模型服务'}).click();
 await page.locator('#service-dialog[open]').waitFor();await settle(page);await page.screenshot({path:join(shots,'service-dialog-dark.png')});
 await page.locator('#service-cancel').click();
 await page.locator('#agent-list .entity-card').filter({hasText:'coder'}).getByRole('button',{name:'模型服务'}).click();
 await page.locator('#service-dialog[open]').waitFor();await page.locator('#service-test').click();
 await page.locator('#service-test-result.ok').waitFor({timeout:30000});await settle(page);await page.screenshot({path:join(shots,'service-test-dark.png')});
 await page.locator('#service-cancel').click();
 await openFor('claude');await page.locator('#service-own.configured').waitFor({timeout:30000});await settle(page);await page.screenshot({path:join(shots,'service-own-dark.png')});
 await page.locator('#service-cancel').click();
 await page.emulateMedia({colorScheme:'light'});
 stage='narrow window';
 await page.locator('.session-row').filter({hasText:'fixture:permission'}).first().click();
 await page.setViewportSize({width:720,height:900});await settle(page);await page.screenshot({path:join(shots,'sessions-narrow.png'),fullPage:true});
 await page.locator('[data-view="agents"]').click();
 await page.locator('#agent-list .entity-card').filter({hasText:'coder'}).getByRole('button',{name:'模型服务'}).click();
 await page.locator('#service-dialog[open]').waitFor();await page.locator('#service-test').click();
 await page.locator('#service-test-result.ok').waitFor({timeout:30000});await settle(page);await page.screenshot({path:join(shots,'service-test-narrow.png')});
 await page.locator('#service-cancel').click();
 await openFor('stranger');
 await page.locator('#service-own.not_configured').waitFor({timeout:30000});await settle(page);await page.screenshot({path:join(shots,'service-own-narrow.png')});
 await page.locator('#service-cancel').click();
 assert(problems.length===0,`browser errors: ${problems.join(' | ')}`);
 console.log(JSON.stringify({ok:true,screenshots:shots}));
}catch(e){console.error(`sessions panel smoke failed at ${stage}: ${e.stack||e}`);process.exitCode=1;}
finally{await browser?.close();for(const child of owned.reverse()){try{child.kill('SIGTERM');}catch{}}}
