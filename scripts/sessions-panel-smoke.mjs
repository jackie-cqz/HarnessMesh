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
// Ready for the next message: while a turn runs the send button is enabled
// too, as 停止本轮 (stop), and a click then stops the turn instead.
async function readyToSend(page){await page.waitForFunction(()=>{const send=document.querySelector('#composer-send');return !send.disabled&&!send.classList.contains('stop');},null,{timeout:30000});}
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
 const server=createServer((req,res)=>{seen.push({method:req.method,url:req.url,authorization:req.headers.authorization||'',apiKey:req.headers['x-api-key']||''});
  if(req.method==='GET'&&req.url.split('?')[0]==='/v1/models'){res.writeHead(200,{'content-type':'application/json'});res.end(JSON.stringify({object:'list',data:[{id:'m-one'},{id:'m-two'}]}));}
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
 const local=register(a,'helper'),remote=register(b,'worker');register(a,'coder','opencode');register(b,'pico','pi');
 register(a,'deepseek','dsh');
 // Fixture agents answering ACP auth/status the way claude-agent-acp does.
 register(a,'claude','claude-code',['--auth-status','subscription']);register(a,'stranger','claude-code',['--auth-status','none']);
 const models=await modelService();
 assert(remote.probe.protocol_probe==='acp_initialize_passed','fixture did not pass the ACP initialize check');
 const pairing=a.mesh.call(['pair','request','--peer',b.card.endpoint,'--expect-fingerprint',b.card.fingerprint]);
 b.mesh.call(['pair','approve',pairing.request_id,'--expect-fingerprint',a.card.fingerprint]);a.mesh.call(['pair','approve',pairing.request_id,'--expect-fingerprint',b.card.fingerprint]);
 const ready=await panel(a);
 browser=await chromium.launch({headless:true,...(process.env.HMESH_CHROME?{executablePath:process.env.HMESH_CHROME}:{})});const page=await browser.newPage({viewport:{width:1440,height:960}});
 const problems=[];page.on('pageerror',e=>problems.push(e.message));page.on('console',m=>{if(m.type()==='error')problems.push(m.text());});
 await page.goto(ready.url);await page.locator('#local-name').filter({hasText:'mac-a'}).waitFor({state:'attached'});
 stage='sidebar lists both computers\' agents';
 await page.locator('#session-agents .session-group').filter({hasText:'mac-a（本机）'}).getByText('helper').waitFor();
 await page.locator('#session-agents .session-group').filter({hasText:'mac-b'}).getByText('worker').waitFor();
 await settle(page);await page.screenshot({path:join(shots,'overview-light.png')});
 await page.locator('#new-session').click();await page.locator('#conversation-empty').waitFor();
 assert(await page.locator('#hero-agents .agent-chip').count()===7,'new-session screen does not offer every agent');
 await settle(page);await page.screenshot({path:join(shots,'sessions-empty.png')});
 stage='slash suggestions on an approved remote Pi session';
 b.mesh.call(['peers','access',a.card.node_id,'confirm']);
 const sent=[];
 page.on('request',r=>{
   const path=new URL(r.url()).pathname;
   if(r.method()==='POST'&&/^\/api\/sessions(?:$|\/[^/]+\/(?:prompt|config|command)$)/.test(path))sent.push({path,body:r.postDataJSON()});
 });
 await page.locator('#hero-agents .agent-chip').filter({hasText:'pico'}).click();
 await page.locator('#hero-input').fill('/');
 await page.locator('#command-help').filter({hasText:'等待执行端批准'}).waitFor();
 const pendingId=await page.evaluate(()=>conv.selected);
 assert(!!pendingId&&!sent.some(r=>r.body?.message),'discovering commands sent a first prompt');
 assert(await page.locator('#command-options [role=option]').count()===0,'unapproved commands visible');
 b.mesh.call(['session','approve',pendingId]);
 const input=page.locator('#composer-input'),options=page.locator('#command-options');
 const row=name=>options.getByRole('option').filter({has:page.getByText('/'+name,{exact:true})});
 await row('compact').getByText('instructions',{exact:true}).waitFor({timeout:45000});
 await row('model').waitFor();
 await page.screenshot({path:join(shots,'session-commands.png')});
 await page.setViewportSize({width:390,height:844});
 assert(await page.locator('#command-options').evaluate(list=>list.clientWidth>0&&list.scrollWidth<=list.clientWidth),'command list overflows on mobile');
 await page.locator('#composer').scrollIntoViewIfNeeded();await settle(page);
 await page.screenshot({path:join(shots,'session-commands-mobile.png')});
 await page.setViewportSize({width:1440,height:960});
 assert(await row('models').count()===0,'Pi offered the OpenCode model spelling');
 await row('model').click();
 await options.getByRole('option').filter({hasText:'fixture-large'}).click();
 await stream(page).getByText('已切换模型：fixture-large').waitFor();
 assert(sent.some(r=>r.path.endsWith('/config')&&r.body.value==='fixture-large'),'model picker did not set the Pi session model');
 assert(!sent.some(r=>r.path.endsWith('/prompt')||r.path.endsWith('/command')),'model selection sent a prompt');
 await input.fill('/comp');await row('compact').waitFor();
 await input.press('ArrowDown');await input.press('Tab');
 assert(await input.inputValue()==='/compact ','Tab did not complete the native command');
 await input.press('End');await input.pressSequentially('keep tests');
 await input.press('Control+Enter');
 await stream(page).getByText('Fixture compacted: keep tests',{exact:true}).waitFor();
 await page.waitForFunction(()=>conv.record?.status==='idle');
 assert(sent.some(r=>r.path.endsWith('/command')&&r.body.arguments==='keep tests'),'native command arguments missing');
 const before=sent.length;
 await input.fill('/omitted');await page.locator('#command-help').filter({hasText:'没有可用的 /omitted'}).waitFor();
 await page.locator('#composer-send').click();assert(sent.length===before,'unknown slash command reached a model or command endpoint');
 await input.fill('fixture:clear-commands');await page.locator('#composer-send').click();
 await stream(page).getByText('Commands cleared',{exact:true}).waitFor();
 await page.waitForFunction(()=>conv.record?.status==='idle');await input.fill('/');await row('model').waitFor();
 assert(await row('compact').count()===0,'old commands survived an empty ACP announcement');
 b.mesh.call(['peers','access',a.card.node_id,'auto']);
 stage='OpenCode slash catalog and model alias';
 await page.locator('#new-session').click();await page.locator('#hero-agents .agent-chip').filter({hasText:'coder'}).click();
 await page.locator('#hero-input').fill('/');await row('models').waitFor();
 assert(await row('model').count()===0,'OpenCode offered the Pi model spelling');
 await input.fill('/models fixture-large');await page.locator('#composer-send').click();
 await stream(page).getByText('已切换模型：fixture-large').waitFor();
 await page.waitForFunction(()=>document.querySelector('#composer-input').value==='');
 await input.fill('/');await row('compact').waitFor();await input.press('Escape');
 assert(await page.locator('#command-menu').isHidden(),'Escape did not dismiss the list');
 stage='DSH model shortcut without native commands';
 await page.locator('#new-session').click();await page.locator('#hero-agents .agent-chip').filter({hasText:'deepseek'}).click();
 await page.locator('#hero-input').fill('/');
 await row('model').getByText(/面板快捷方式/).waitFor();
 assert(await options.getByRole('option').count()===1,'DSH offered an unannounced command');
 await row('model').click();
 await options.getByRole('option').filter({hasText:'fixture-large'}).click();
 await stream(page).getByText('已切换模型：fixture-large').waitFor();
 const beforeUnknown=sent.length;
 await input.fill('/compact');await page.locator('#command-help').filter({hasText:'没有可用的 /compact'}).waitFor();
 await page.locator('#composer-send').click();
 assert(sent.length===beforeUnknown,'unavailable DSH command was sent to the agent');
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
 await readyToSend(page);
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
 await readyToSend(page);
 await page.locator('#composer-input').fill('fixture:config');await page.locator('#composer-send').click();
 await stream(page).getByText('model=fixture-large mode=ask effort=low').waitFor({timeout:30000});
 await settle(page);await page.screenshot({path:join(shots,'settings-composer.png')});
 stage='reload keeps the conversation';
 await page.reload();await page.locator('#local-name').filter({hasText:'mac-a'}).waitFor({state:'attached'});
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
 await readyToSend(page);
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
 assert((await page.locator('#service-relay').innerText()).includes('key 不交给 OpenCode'),'relay note missing for OpenCode with a custom address');
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
 await openFor('deepseek');
 const custom=page.locator('#service-modes').getByRole('radio',{name:/自定义地址/});
 await custom.click();
 // Check actual geometry before any locator action scrolls the dialog.
 for(const width of [1280,390]){
   await page.setViewportSize({width,height:720});
   const geometry=await page.locator('#service-dialog').evaluate(dialog=>{
     const bounds=dialog.getBoundingClientRect();
     const close=dialog.querySelector('#close-service').getBoundingClientRect();
     const save=dialog.querySelector('#service-save').getBoundingClientRect();
     const fields=dialog.querySelector('.service-fields');
     const footerBefore=save.bottom;
     const originalScroll=fields.scrollTop;
     fields.scrollTop=fields.scrollHeight;
     const footerAfter=dialog.querySelector('#service-save').getBoundingClientRect().bottom;
     fields.scrollTop=originalScroll;
     return {visible:bounds.left>=0&&bounds.right<=innerWidth&&close.top>=bounds.top&&save.bottom<=bounds.bottom&&save.bottom<=innerHeight,
       footerFixed:footerBefore===footerAfter,scrolls:fields.scrollHeight>fields.clientHeight,
       horizontalOverflow:fields.scrollWidth>fields.clientWidth};
   });
   assert(geometry.visible&&geometry.footerFixed&&!geometry.horizontalOverflow,'model service actions or fields overflow at '+width);
   if(width===390)assert(geometry.scrolls,'expected long mobile form to scroll inside the modal');
 }
 await page.setViewportSize({width:1440,height:960});
 const format=page.locator('#service-protocol'),preset=page.locator('#service-preset');
 assert(await format.locator('option').count()===3,'missing DSH custom protocols');
 await format.selectOption('openai-chat');await preset.selectOption('deepseek');
 assert(await page.locator('#service-url').inputValue()==='https://api.deepseek.com/v1','wrong OpenAI preset address');
 await format.selectOption('anthropic-messages');
 assert(await page.locator('#service-url').inputValue()==='https://api.deepseek.com/anthropic','protocol change kept the old preset URL');
 await format.selectOption('openai-responses');
 assert(await preset.inputValue()===''&&await page.locator('#service-url').inputValue()==='','unsupported preset left a stale address');
 await page.locator('#service-url').fill(models.base);
 await format.selectOption('anthropic-messages');
 assert(await page.locator('#service-url').inputValue()===models.base,'manual address erased on a protocol change');
 const dshKey='fixture-deepseek-1997';
 await page.locator('#service-key').fill(dshKey);await page.locator('#service-model').fill('m-one');
 await page.locator('#service-test').click();
 await page.locator('#service-test-result.ok').waitFor();
 assert(models.seen.at(-1).apiKey===dshKey&&!models.seen.at(-1).authorization,'wrong header or agent key in Messages request');
 await page.locator('#service-save').click();await page.locator('#service-dialog').waitFor({state:'hidden'});
 await openFor('deepseek');
 assert(await format.inputValue()==='anthropic-messages','saved DSH protocol did not round-trip');
 assert(await page.locator('#service-key').inputValue()===''&&!((await page.content()).includes(dshKey)),'DSH key returned to browser');
 await page.locator('#service-cancel').click();
 await openFor('helper');
 await page.locator('#service-modes').getByRole('radio',{name:/单独给它一个 key/}).click();
 await page.getByLabel('Agent 读取 Key 的变量名').fill('FIXTURE_MODEL_KEY');
 await page.locator('#service-key').fill('generic-fixture-key');
 await page.locator('#service-save').click();await page.locator('#service-dialog').waitFor({state:'hidden'});
 await openFor('helper');
 assert(await page.getByLabel('Agent 读取 Key 的变量名').inputValue()==='FIXTURE_MODEL_KEY','generic ACP key binding missing');
 await page.locator('#service-cancel').click();
 await openFor('claude');
 await page.locator('#service-own.configured').filter({hasText:'它自己已经配好了'}).waitFor({timeout:30000});
 const own=await page.locator('#service-own').innerText();
 assert(own.includes('fixture@example.com')&&own.includes('Claude 订阅（max 套餐）'),'own login details missing: '+own);
 await settle(page);await page.screenshot({path:join(shots,'service-own.png')});
 await page.locator('#service-modes').getByRole('radio',{name:/自定义地址/}).click();
 assert(await format.isDisabled()&&await format.inputValue()==='anthropic-messages','Claude offered unsupported protocols');
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
 stage='session usage on the settings page';
 await openSession(page,'mac-a（本机）','helper','fixture:usage');
 await page.locator('[data-view="settings"]').click();
 const usageBox=page.locator('#session-usage-list');
 await usageBox.getByText('$0.0125').first().waitFor({timeout:30000});
 const usageText=await usageBox.innerText();
 assert(usageText.includes('1.2k / 128.0k')&&usageText.includes('总 40 · 输入 30 / 输出 10')&&usageText.includes('累计费用合计 $0.0125'),'session usage row incomplete: '+usageText);
 await settle(page);await usageBox.screenshot({path:join(shots,'session-usage.png')});
 stage='compact navigation on mobile';
 await page.setViewportSize({width:390,height:720});
 const directory=page.locator('#sidebar-directory'),directoryToggle=page.locator('#sidebar-toggle');
 assert(await directory.isHidden(),'mobile directory takes up space before being opened');
 await directoryToggle.click();await directory.getByText('可用 Agent',{exact:true}).waitFor();
 assert(await directoryToggle.getAttribute('aria-expanded')==='true','mobile directory state not announced');
 await directoryToggle.click();assert(await directory.isHidden(),'mobile directory did not collapse');
 await page.setViewportSize({width:1440,height:960});
 assert(await directory.isVisible()&&await directoryToggle.isHidden(),'desktop directory was affected by the mobile toggle');
 stage='dark theme';
 await page.emulateMedia({colorScheme:'dark'});
 await page.locator('.session-row').filter({hasText:'fixture:reply'}).first().click();
 await stream(page).getByText(/Final answer/).first().waitFor();
 const shade=await page.evaluate(()=>{const lum=c=>{const m=c.match(/[\d.]+/g).map(Number);return (0.2126*m[0]+0.7152*m[1]+0.0722*m[2])/255;};return {page:lum(getComputedStyle(document.body).backgroundColor),sidebar:lum(getComputedStyle(document.querySelector('.sidebar')).backgroundColor),text:lum(getComputedStyle(document.querySelector('.msg.agent .msg-text')).color)};});
 assert(shade.page<0.1&&shade.sidebar<0.15&&shade.sidebar>=shade.page&&shade.text>0.8,`dark theme not applied: ${JSON.stringify(shade)}`);
 await settle(page);await page.screenshot({path:join(shots,'sessions-dark.png')});
 await page.locator('#new-session').click();await settle(page);await page.screenshot({path:join(shots,'hero-dark.png')});
 await page.locator('[data-view="inbox"]').click();await settle(page);await page.screenshot({path:join(shots,'inbox-dark.png')});
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
 await page.setViewportSize({width:390,height:844});
 await page.locator('[data-view="settings"]').click();await usageBox.getByText('$0.0125').first().waitFor({timeout:30000});
 assert(!await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth+1),'settings page overflows a phone-sized window');
 await usageBox.scrollIntoViewIfNeeded();await settle(page);await page.screenshot({path:join(shots,'session-usage-phone.png')});
 assert(problems.length===0,`browser errors: ${problems.join(' | ')}`);
 console.log(JSON.stringify({ok:true,screenshots:shots}));
}catch(e){console.error(`sessions panel smoke failed at ${stage}: ${e.stack||e}`);process.exitCode=1;}
finally{await browser?.close();for(const child of owned.reverse()){try{child.kill('SIGTERM');}catch{}}}
