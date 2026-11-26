// Retakes the README's conversation screenshots (docs/images/session.png,
// permission.png, delegated.png): two loopback nodes, "studio" and "laptop",
// whose agents are the ACP test fixture playing scripts/readme-screenshots.json.
// No model is called. Build first (cargo build --workspace), then:
//   HMESH_SCREENSHOT_DIR=docs/images node scripts/readme-screenshots.mjs
import {spawn,spawnSync} from 'node:child_process';
import {mkdtempSync,mkdirSync,readFileSync,writeFileSync} from 'node:fs';
import {hostname,tmpdir,userInfo} from 'node:os';
import {join,resolve} from 'node:path';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {Mesh,assert,until} from './acceptance/common.mjs';
const {chromium}=await import(process.env.HMESH_PLAYWRIGHT_MODULE||'playwright');
const root=mkdtempSync(join(tmpdir(),'hmesh-readme-'));
const out=resolve(process.env.HMESH_SCREENSHOT_DIR||root);mkdirSync(out,{recursive:true});
const exe=name=>resolve('target/debug',process.platform==='win32'?`${name}.exe`:name);
const binary=exe('hmesh'),fixture=exe('hmesh-fixture'),script=resolve('scripts/readme-screenshots.json');
const owned=[],problems=[];let browser;let stage='setup';
function start(args,stdout='ignore'){const child=spawn(binary,args,{stdio:['ignore',stdout,'ignore'],windowsHide:true});owned.push(child);return child;}
// One computer: its own data directory and agents, each with a project folder.
async function computer(name,agents){
 const dir=join(root,name),init=spawnSync(binary,['--json','--data-dir',dir,'init','--name',name,'--port','0'],{encoding:'utf8'});
 assert(init.status===0,`init ${name}: ${init.stderr}`);
 // A host name of the demo's own, not this machine's.
 const config=join(dir,'node.toml'),text=readFileSync(config,'utf8');
 assert(text.includes('[node]\n'),`no [node] table in ${config}`);
 writeFileSync(config,text.replace('[node]\n',`[node]\nhost_names = ["${name}.local"]\n`));
 const mesh=new Mesh(binary,dir);start(['--data-dir',dir,'daemon','start']);
 const card=await until(()=>{try{mesh.call(['daemon','status']);return mesh.call(['node','show']);}catch{return null;}});
 const ids={};
 for(const [agent,project] of agents){
  const folder=join(root,'work',name,'code',project),state=join(root,'state',name,agent);
  mkdirSync(folder,{recursive:true});mkdirSync(state,{recursive:true});
  const file=join(root,`${name}-${agent}.toml`);
  writeFileSync(file,['[agent]',`name="${agent}"`,'harness="acp"',`project_id="${project}"`,'[project]','root='+JSON.stringify(folder),'allowed_tools=["read","write","edit"]','[adapter]','executable='+JSON.stringify(fixture),'args='+JSON.stringify(['--script',script,'--models','standard,deep','--mode','code','--state-dir',state,'--no-list']),'env_allowlist=[]',''].join('\n'));
  ids[agent]=mesh.call(['agent','register','--config',file]).agent_id;
 }
 return{name,mesh,card,dir,ids};
}
async function panel(node){
 const child=start(['--json','--data-dir',node.dir,'panel','--port','0'],'pipe');
 const lines=createInterface({input:child.stdout});
 const[line]=await Promise.race([once(lines,'line'),new Promise((_,reject)=>{const t=setTimeout(()=>reject(Error('panel startup timeout')),20000);t.unref();})]);
 lines.close();return JSON.parse(line).url;
}
const stream=page=>page.locator('#conversation-stream');
const settle=page=>page.waitForTimeout(700);
const idle=(page,turns=1)=>page.waitForFunction(n=>conv.record?.status==='idle'&&conv.record?.turns>=n,turns,{timeout:60000});
async function open(url,name){
 const page=await browser.newPage({viewport:{width:1280,height:800},deviceScaleFactor:2});
 page.on('pageerror',e=>problems.push(`${name}: ${e.message}`));
 page.on('console',m=>{if(m.type()==='error')problems.push(`${name}: ${m.text()}`);});
 await page.goto(url);await page.locator('#local-name').filter({hasText:name}).waitFor({state:'attached'});
 return page;
}
async function pick(page,title){await page.locator('.session-row').filter({hasText:title}).first().click();await page.locator('#conversation').waitFor();}
// What a reader of the README could learn about the computer it was taken on.
const personal=[hostname().replace(/\.local$/,''),userInfo().username].filter(s=>s&&s.length>2);
async function shot(page,file){
 await settle(page);
 const[shown,tips]=await page.evaluate(()=>[document.body.innerText,[...document.querySelectorAll('[title]')].map(e=>e.title).join('\n')]);
 // Folder paths only appear in tooltips, which a screenshot does not show.
 for(const s of [...personal,root])assert(!shown.includes(s),`${file} shows ${JSON.stringify(s)}`);
 for(const s of personal)assert(!tips.includes(s),`a tooltip in ${file} has ${JSON.stringify(s)}`);
 await page.screenshot({path:join(out,file)});
}
try{
 const studio=await computer('studio',[['backend','shop-api'],['ops','infra']]);
 const laptop=await computer('laptop',[['frontend','shop-web'],['reviewer','shop-api']]);
 stage='pairing';
 const pairing=studio.mesh.call(['pair','request','--peer',laptop.card.endpoint,'--expect-fingerprint',laptop.card.fingerprint]);
 laptop.mesh.call(['pair','approve',pairing.request_id,'--expect-fingerprint',studio.card.fingerprint]);
 studio.mesh.call(['pair','approve',pairing.request_id,'--expect-fingerprint',laptop.card.fingerprint]);
 const studioUrl=await panel(studio),laptopUrl=await panel(laptop);
 browser=await chromium.launch({headless:true,...(process.env.HMESH_CHROME?{executablePath:process.env.HMESH_CHROME}:{})});

 stage='earlier sessions';
 // Opened from studio, so the sidebar looks used.
 const sopen=(agent,peer,message)=>studio.mesh.call(['session','open','--agent',agent,...(peer?['--peer',peer]:[]),'--message',message]);
 sopen(studio.ids.ops,null,'给 /health 加上版本号和构建时间');
 sopen(laptop.ids.frontend,laptop.card.node_id,'把设置页的保存按钮改成主色');
 await new Promise(r=>setTimeout(r,1500));

 stage='session.png';
 const ps=await open(studioUrl,'studio');
 await ps.locator('#new-session').click();
 await ps.locator('#session-agents .session-group').filter({has:ps.locator('.session-group-name',{hasText:/^studio（本机）$/})}).getByRole('button',{name:/backend/}).click();
 await ps.locator('#hero-input').fill('登录接口偶尔返回 500，帮我查一下原因。修好后让 laptop 上的 reviewer 看一眼。');
 await ps.locator('#hero-send').click();
 await ps.locator('#conversation').waitFor();
 await idle(ps);
 await stream(ps).getByText('13 个测试全部通过').waitFor();
 // Open the finished turn's process and the call to the other computer.
 const turn=stream(ps).locator('.turn').last();
 await turn.locator('details.work > summary').first().click();
 const call=stream(ps).locator('.step.delegate[data-tool="t5"]');
 await call.locator(':scope > summary').click();
 await call.locator('.delegate-reply').waitFor();
 // Scrolled to the end: from the process's summary to the answer.
 await stream(ps).evaluate(e=>{e.scrollTop=e.scrollHeight;});
 await shot(ps,'session.png');

 stage='permission.png';
 sopen(laptop.ids.frontend,laptop.card.node_id,'把首页的加载动画换成骨架屏');
 await ps.reload();await ps.locator('#local-name').filter({hasText:'studio'}).waitFor({state:'attached'});
 await pick(ps,'把首页的加载动画换成骨架屏');
 await ps.locator('#permission-dock .permission').waitFor({timeout:30000});
 await shot(ps,'permission.png');

 stage='delegated.png';
 const pl=await open(laptopUrl,'laptop');
 await pick(pl,'请审查 src/auth/session.ts');
 await stream(pl).getByText('超过就返回 409').waitFor();
 // Its process opened, with the diff it read.
 await stream(pl).locator('.turn').last().locator('details.work > summary').first().click();
 await stream(pl).locator('.step[data-tool="r1"] > summary').click();
 await stream(pl).locator('.step[data-tool="r1"] .term-exit').waitFor();
 await shot(pl,'delegated.png');
 assert(problems.length===0,`browser errors: ${problems.join(' | ')}`);
 console.log(JSON.stringify({ok:true,screenshots:out}));
}catch(e){console.error(`README screenshots failed at ${stage}: ${e.stack||e}${problems.length?`\nbrowser errors: ${problems.join(' | ')}`:''}`);process.exitCode=1;}
finally{await browser?.close();for(const child of owned.reverse()){try{child.kill('SIGTERM');}catch{}}}
