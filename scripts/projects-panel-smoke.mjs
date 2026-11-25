// Real browser + two real TLS nodes: projects, the sessions agents already
// keep in them, session names/pins/hiding, and a project shared with a paired
// computer. The only agent is the ACP test fixture; no model is called.
import {spawn,spawnSync} from 'node:child_process';
import {mkdtempSync,mkdirSync,writeFileSync,realpathSync,utimesSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {Mesh,assert,until} from './acceptance/common.mjs';
const {chromium}=await import(process.env.HMESH_PLAYWRIGHT_MODULE||'playwright');
// Folders are compared the way the daemon stores them: with links resolved.
const root=realpathSync(mkdtempSync(join(tmpdir(),'hmesh-projects-')));
const shots=resolve(process.env.SMOKE_SHOTS_DIR||join(root,'shots'));mkdirSync(shots,{recursive:true});
const binary=resolve('target/debug',process.platform==='win32'?'hmesh.exe':'hmesh');
const fixture=resolve('target/debug',process.platform==='win32'?'hmesh-fixture.exe':'hmesh-fixture');
const owned=[],taken=[];let browser;let stage='setup';
function start(args,stdout='ignore'){const child=spawn(binary,args,{stdio:['ignore',stdout,'ignore'],windowsHide:true});owned.push(child);return child;}
async function node(name){
 const dir=join(root,name),init=spawnSync(binary,['--json','--data-dir',dir,'init','--name',name,'--port','0'],{encoding:'utf8'});
 assert(init.status===0,'CLI init failed');const mesh=new Mesh(binary,dir);start(['--data-dir',dir,'daemon','start']);
 const card=await until(()=>{try{mesh.call(['daemon','status']);return mesh.call(['node','show']);}catch{return null;}});
 return{mesh,card,dir,name};
}
function register(node,name,projectId,folder,args){
 const config=join(root,`${node.name}-${name}.toml`);
 writeFileSync(config,['[agent]',`name="${name}"`,'harness="acp"',`project_id="${projectId}"`,'[project]','root='+JSON.stringify(folder),'allowed_tools=["read","write","edit"]','[adapter]','executable='+JSON.stringify(fixture),'args='+JSON.stringify(args),'env_allowlist=[]',''].join('\n'));
 return node.mesh.call(['agent','register','--config',config]);
}
// A session the agent kept before HarnessMesh saw it, in the fixture's own store.
function seed(state,id,cwd,history,ageSeconds){
 const file=join(state,`${id}.json`);writeFileSync(file,JSON.stringify({cwd,history}));
 const at=Date.now()/1000-ageSeconds;utimesSync(file,at,at);
}
async function panel(node){
 const child=start(['--json','--data-dir',node.dir,'panel','--port','0'],'pipe');
 const lines=createInterface({input:child.stdout});
 const[line]=await Promise.race([once(lines,'line'),new Promise((_,reject)=>{const t=setTimeout(()=>reject(Error('Panel startup timeout')),20000);t.unref();})]);
 lines.close();return JSON.parse(line).url;
}
const settle=page=>page.waitForTimeout(400);
const stream=page=>page.locator('#conversation-stream');
const projectBlock=(page,name)=>page.locator('#session-list .project').filter({has:page.locator('.project-name',{hasText:new RegExp('^'+name+'$')})});
const rowsIn=block=>block.locator('.project-body > .session-item .session-row');
const projectNames=page=>page.locator('#session-list .project:not(.other) .project-name').allTextContents();
async function shot(page,name,options={}){const path=join(shots,name);await settle(page);await page.evaluate(()=>{document.querySelector('#toast').hidden=true;});await page.screenshot({path,...options});taken.push(path);}
// Nothing internal may reach the page; the agent's own replies may name folders.
async function noInternals(page,where){
 const text=await page.evaluate(()=>{
  let text=document.body.innerText;
  for(const bubble of document.querySelectorAll('.msg.agent .msg-text'))text=text.split(bubble.innerText).join('');
  return text;
 });
 for(const word of ['{"','session_id','agent_session_id','project_id','cwd='])assert(!text.includes(word),`${where}: the page shows "${word}"`);
 const id=/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i.exec(text);
 assert(!id,`${where}: the page shows an ID ${id}`);
}
async function sessionMenu(page,item,choice){
 await item.hover();await item.locator('.row-menu').click();
 await page.locator('.side-menu').getByRole('menuitem',{name:choice,exact:true}).click();
}
async function projectMenu(page,name){
 const head=projectBlock(page,name).locator('.project-head');
 await head.hover();await head.getByRole('button',{name:'项目操作'}).click();
 await page.locator('.side-menu').waitFor();
}
async function waitIdle(page){await page.waitForFunction(()=>conv.record?.status==='idle',null,{timeout:30000});}
async function send(page,message){
 await page.waitForFunction(()=>!document.querySelector('#composer-send').disabled);
 await page.locator('#composer-input').fill(message);await page.locator('#composer-send').click();
}
try{
 const a=await node('mac-a'),b=await node('mac-b');
 const state=join(root,'fixture-state');mkdirSync(state);
 const folder=name=>{const dir=join(root,name);mkdirSync(dir);return realpathSync(dir);};
 const shop=folder('shop-app'),blog=folder('blog'),notes=folder('notes'),site=folder('site');
 seed(state,'shop-build',shop,[['user','检查一下构建脚本'],['agent','构建脚本没问题，可以直接发布。']],600);
 seed(state,'shop-deps',shop,[['user','升级一下依赖版本'],['agent','依赖都升级到最新的小版本了。']],2*86400);
 seed(state,'blog-outline',blog,[['user','整理博客的目录结构'],['agent','好的，目录已经按年份分好了。']],26*3600);
 seed(state,'blog-tags',blog,[['user','给文章加上标签'],['agent','每篇文章都加了两三个标签。']],3*86400);
 seed(state,'notes-topics',notes,[['user','把读书笔记按主题归类'],['agent','已经分成五个主题。']],5*3600);
 seed(state,'notes-tags',notes,[['user','给笔记加上标签'],['agent','标签都加好了。']],30*3600);
 const helper=register(a,'helper','shop-app',shop,['--state-dir',state]);
 assert(helper.probe?.protocol_probe==='acp_initialize_passed','fixture did not pass the ACP initialize check');
 // A second agent in the same folder that cannot list its sessions.
 register(a,'reviewer','shop-app',shop,['--state-dir',join(root,'reviewer-state'),'--no-list']);
 const pairing=a.mesh.call(['pair','request','--peer',b.card.endpoint,'--expect-fingerprint',b.card.fingerprint]);
 b.mesh.call(['pair','approve',pairing.request_id,'--expect-fingerprint',a.card.fingerprint]);a.mesh.call(['pair','approve',pairing.request_id,'--expect-fingerprint',b.card.fingerprint]);
 const urlA=await panel(a),urlB=await panel(b);
 browser=await chromium.launch({headless:true,...(process.env.HMESH_CHROME?{executablePath:process.env.HMESH_CHROME}:{})});
 const problems=[];let allowRefusal=false;
 async function open(url,name){
  const page=await browser.newPage({viewport:{width:1440,height:960}});
  page.on('pageerror',e=>problems.push(`${name}: ${e.message}`));
  page.on('console',m=>{if(m.type()==='error'&&!(allowRefusal&&/status of 502/.test(m.text())))problems.push(`${name}: ${m.text()}`);});
  await page.goto(url);await page.locator('#local-name').filter({hasText:name}).waitFor({state:'attached'});
  return page;
 }
 const page=await open(urlA,'mac-a');
 await page.waitForFunction(()=>state.runtime?.capabilities?.projects===true);

 stage='a session in the agent\'s own project';
 await page.locator('#new-session').click();await page.locator('#conversation-empty').waitFor();
 await page.locator('#hero-agents .agent-chip').filter({hasText:'helper'}).click();
 const picker=page.locator('#hero-project select');await picker.waitFor();
 assert(await picker.locator('option:checked').textContent()==='shop-app','the new session does not start in the agent\'s own project');
 await page.locator('#hero-input').fill('fixture:echo 第一次打招呼');await page.locator('#hero-send').click();
 await stream(page).getByText(/Echo #\d+: 第一次打招呼/).waitFor({timeout:30000});
 await waitIdle(page);

 stage='add projects from the agents\' history and by path';
 await page.locator('#session-list .add-project').click();
 const dialog=page.locator('#project-dialog');await dialog.waitFor();
 const blogRow=dialog.locator('.folder-row').filter({hasText:blog}),shopRow=dialog.locator('.folder-row').filter({hasText:shop});
 await blogRow.waitFor({timeout:30000});
 assert((await blogRow.innerText()).includes('2 个会话')&&(await blogRow.innerText()).includes('helper'),'the folder from the agent\'s history lacks its sessions or agent: '+await blogRow.innerText());
 assert((await dialog.locator('.folder-row').filter({hasText:notes}).innerText()).includes('2 个会话'),'the second folder is not counted');
 assert((await shopRow.innerText()).includes('已添加')&&await shopRow.locator('input').isDisabled(),'the agent\'s own folder is offered again');
 await noInternals(page,'add-project dialog');
 allowRefusal=true;
 await page.locator('#project-path').fill(shop);await page.locator('#project-add').click();
 await page.locator('#project-message.error').filter({hasText:'这个文件夹已经是项目「shop-app」了'}).waitFor();
 await page.locator('#project-path').fill('relative/folder');await page.locator('#project-add').click();
 await page.locator('#project-message.error').filter({hasText:'请填写文件夹的完整路径'}).waitFor();
 allowRefusal=false;
 await blogRow.locator('input').check();await page.locator('#project-path').fill(site);
 await page.locator('#project-add').click();await dialog.waitFor({state:'hidden'});
 await projectBlock(page,'blog').waitFor();await projectBlock(page,'site').waitFor();

 stage='a session in an added project';
 await page.locator('#new-session').click();await page.locator('#conversation-empty').waitFor();
 assert((await picker.locator('option').allTextContents()).join('|')==='shop-app|blog|site','the project picker does not offer every project');
 await picker.selectOption({label:'site'});
 await page.locator('#hero-input').fill('fixture:cwd');await page.locator('#hero-send').click();
 await stream(page).getByText(`cwd=${site} `).waitFor({timeout:30000});
 await waitIdle(page);

 stage='the sidebar lists this computer → project → sessions';
 const localGroup=page.locator('#session-list .session-group').first();
 assert((await localGroup.locator('.session-group-name').innerText()).includes('mac-a（本机）'),'this computer does not head the project list');
 await rowsIn(projectBlock(page,'shop-app')).filter({hasText:'fixture:echo 第一次打招呼'}).waitFor();
 await rowsIn(projectBlock(page,'site')).filter({hasText:'fixture:cwd'}).waitFor();
 assert(await rowsIn(projectBlock(page,'shop-app')).filter({hasText:'fixture:cwd'}).count()===0,'a session is listed under the wrong project');
 assert(await rowsIn(projectBlock(page,'site')).filter({hasText:'第一次打招呼'}).count()===0,'a session is listed under the wrong project');

 stage='expand a project and continue an earlier session';
 const shopEarlier=projectBlock(page,'shop-app').locator('.session-row.earlier').filter({hasText:'检查一下构建脚本'});
 await shopEarlier.waitFor({timeout:30000});
 assert(await projectBlock(page,'shop-app').locator('.session-row.earlier').filter({hasText:'fixture:echo'}).count()===0,'a session HarnessMesh already has is listed again as an earlier one');
 await projectBlock(page,'shop-app').locator('.side-note').filter({hasText:'reviewer 不提供之前的会话'}).waitFor();
 const blogBlock=projectBlock(page,'blog'),blogEarlier=blogBlock.locator('.session-row.earlier').filter({hasText:'整理博客的目录结构'});
 if(await blogBlock.locator('.project-toggle').getAttribute('aria-expanded')!=='true')await blogBlock.locator('.project-toggle').click();
 await blogEarlier.waitFor({timeout:30000});
 assert((await blogEarlier.innerText()).includes('helper'),'the earlier session does not name its agent');
 await noInternals(page,'sidebar with earlier sessions');
 await blogEarlier.click();
 const user=stream(page).locator('.msg.user').filter({hasText:'整理博客的目录结构'}),reply=stream(page).locator('.msg.agent').filter({hasText:'好的，目录已经按年份分好了。'});
 await user.waitFor({timeout:30000});await reply.waitFor();
 await stream(page).getByText('已接上 Agent 里已有的会话，上面是之前的记录。').waitFor({timeout:30000});
 await waitIdle(page);
 const order=await page.evaluate(()=>{const items=[...document.querySelectorAll('#conversation-stream .msg,#conversation-stream .event-note')];return items.map(n=>n.textContent);});
 const at=text=>order.findIndex(t=>t.includes(text));
 assert(at('整理博客的目录结构')>=0&&at('整理博客的目录结构')<at('好的，目录已经按年份分好了')&&at('好的，目录已经按年份分好了')<at('已接上 Agent'),'the earlier conversation is out of order: '+JSON.stringify(order));
 await noInternals(page,'continued session');
 await send(page,'fixture:cwd');
 await stream(page).getByText(`cwd=${blog} `).waitFor({timeout:30000});
 await waitIdle(page);
 await rowsIn(blogBlock).filter({hasText:'整理博客的目录结构'}).waitFor();
 assert(await blogBlock.locator('.session-row.earlier').filter({hasText:'整理博客的目录结构'}).count()===0,'the continued session is still offered as an earlier one');
 // Continue the shop's earlier session too, so its project holds two sessions.
 await shopEarlier.click();
 await stream(page).locator('.msg.agent').filter({hasText:'构建脚本没问题，可以直接发布。'}).waitFor({timeout:30000});
 await waitIdle(page);
 await rowsIn(projectBlock(page,'shop-app')).filter({hasText:'检查一下构建脚本'}).waitFor();

 stage='search, rename, pin and hide sessions';
 const search=page.locator('#session-search');
 await search.fill('博客');
 await until(async()=>(await page.locator('#session-list .session-row').count())===1,10,'search did not narrow the list to one session');
 assert(await page.locator('#session-list .session-row').first().innerText().then(t=>t.includes('整理博客的目录结构')),'search kept the wrong session');
 assert(await projectBlock(page,'shop-app').count()===0&&await projectBlock(page,'site').count()===0,'projects without matches stay visible while searching');
 await search.fill('');await projectBlock(page,'site').waitFor();
 const shopRows=()=>rowsIn(projectBlock(page,'shop-app')).locator('.session-row-title').allTextContents();
 await sessionMenu(page,projectBlock(page,'shop-app').locator('.session-item').filter({hasText:'fixture:echo 第一次打招呼'}),'改名');
 await page.locator('#name-input').fill('回声测试');await page.locator('#name-save').click();
 await rowsIn(projectBlock(page,'shop-app')).filter({hasText:'回声测试'}).waitFor();
 assert((await shopRows()).join('|')==='检查一下构建脚本|回声测试','unexpected order before pinning: '+(await shopRows()).join('|'));
 await sessionMenu(page,projectBlock(page,'shop-app').locator('.session-item').filter({hasText:'回声测试'}),'置顶');
 await until(async()=>(await shopRows()).join('|')==='回声测试|检查一下构建脚本',10,'a pinned session is not listed first');
 assert(await projectBlock(page,'shop-app').locator('.session-item').filter({hasText:'回声测试'}).locator('.row-pin').count()===1,'a pinned session is not marked');
 await sessionMenu(page,projectBlock(page,'shop-app').locator('.session-item').filter({hasText:'回声测试'}),'隐藏');
 await until(async()=>!(await shopRows()).includes('回声测试'),10,'a hidden session is still listed');
 const showHidden=page.locator('#show-hidden');await showHidden.waitFor();
 await showHidden.check();
 const hiddenItem=projectBlock(page,'shop-app').locator('.session-item.is-hidden').filter({hasText:'回声测试'});await hiddenItem.waitFor();
 await sessionMenu(page,hiddenItem,'取消隐藏');
 await projectBlock(page,'shop-app').locator('.session-item:not(.is-hidden)').filter({hasText:'回声测试'}).waitFor();
 await showHidden.uncheck();
 assert((await shopRows()).includes('回声测试'),'an unhidden session did not come back');

 stage='project menu: rename, move and share';
 await projectMenu(page,'site');
 await page.locator('.side-menu').getByRole('menuitem',{name:'改名'}).click();
 await page.locator('#name-input').fill('网站');await page.locator('#name-save').click();
 await projectBlock(page,'网站').waitFor();
 assert((await projectNames(page)).join('|')==='shop-app|blog|网站','unexpected project order: '+(await projectNames(page)).join('|'));
 await projectMenu(page,'网站');await page.locator('.side-menu').getByRole('menuitem',{name:'上移'}).click();
 await until(async()=>(await projectNames(page)).join('|')==='shop-app|网站|blog',10,'moving a project up did not change the order');
 await projectMenu(page,'网站');
 const share=page.locator('.side-menu').getByRole('menuitemcheckbox',{name:'共享给配对的电脑'});
 assert(await share.getAttribute('aria-checked')==='false','a project is shared before the owner shares it');
 assert((await page.locator('.side-menu').innerText()).includes('读写里面的文件'),'sharing does not say what the other computer can do');
 await share.click();
 await projectBlock(page,'网站').locator('.project-flag').first().waitFor();
 await projectMenu(page,'网站');
 assert(await share.getAttribute('aria-checked')==='true','the share mark is not shown');
 await shot(page,'project-menu-light.png');
 await page.keyboard.press('Escape');

 await projectMenu(page,'blog');await page.locator('.side-menu').getByRole('menuitem',{name:'隐藏'}).click();
 await until(async()=>(await projectNames(page)).join('|')==='shop-app|网站',10,'a hidden project is still listed');
 await page.locator('#show-hidden').check();
 await page.locator('#session-list .project.is-hidden').filter({hasText:'blog'}).waitFor();
 await projectMenu(page,'blog');await page.locator('.side-menu').getByRole('menuitem',{name:'取消隐藏'}).click();
 await page.locator('#session-list .project:not(.is-hidden)').filter({has:page.locator('.project-name',{hasText:/^blog$/})}).waitFor();
 await page.locator('#show-hidden').uncheck();
 assert((await projectNames(page)).join('|')==='shop-app|网站|blog','an unhidden project did not come back in place');

 stage='remove an added project: its sessions move to 其他';
 const scratch=folder('scratch');
 await page.locator('#session-list .add-project').click();await dialog.waitFor();
 await page.locator('#project-path').fill(scratch);await page.locator('#project-add').click();await dialog.waitFor({state:'hidden'});
 await page.locator('#new-session').click();await page.locator('#conversation-empty').waitFor();
 await picker.selectOption({label:'scratch'});
 await page.locator('#hero-input').fill('fixture:echo 临时试一下');await page.locator('#hero-send').click();
 await stream(page).getByText(/Echo #\d+: 临时试一下/).waitFor({timeout:30000});await waitIdle(page);
 await rowsIn(projectBlock(page,'scratch')).filter({hasText:'临时试一下'}).waitFor();
 await projectMenu(page,'shop-app');
 assert(await page.locator('.side-menu').getByRole('menuitem',{name:'移除'}).count()===0,'an agent\'s own folder offers to be removed');
 await page.keyboard.press('Escape');
 await projectMenu(page,'scratch');await page.locator('.side-menu').getByRole('menuitem',{name:'移除'}).click();
 await page.getByRole('button',{name:'确认',exact:true}).click();
 await until(async()=>(await projectBlock(page,'scratch').count())===0,10,'a removed project is still listed');
 await page.locator('#session-list .project.other .session-row').filter({hasText:'临时试一下'}).waitFor();
 assert((await projectNames(page)).join('|')==='shop-app|网站|blog','removing a project changed the others');

 stage='a paired computer opens a session in the shared project';
 const pageB=await open(urlB,'mac-b');
 await pageB.locator('#new-session').click();await pageB.locator('#conversation-empty').waitFor();
 await pageB.locator('#hero-agents .agent-chip').filter({hasText:'helper'}).click();
 const pickerB=pageB.locator('#hero-project select');await pickerB.waitFor({timeout:30000});
 assert((await pickerB.locator('option').allTextContents()).join('|')==='Agent 自己的目录|网站','the paired computer is not offered the shared project: '+(await pickerB.locator('option').allTextContents()).join('|'));
 assert(await pickerB.locator('option:checked').textContent()==='Agent 自己的目录','the paired computer does not start in the agent\'s own folder');
 await pickerB.selectOption({label:'网站'});
 await shot(pageB,'new-session-paired-light.png');
 await noInternals(pageB,'paired computer new session');
 await pageB.locator('#hero-input').fill('fixture:cwd');await pageB.locator('#hero-send').click();
 await stream(pageB).getByText(`cwd=${site} `).waitFor({timeout:30000});
 await noInternals(pageB,'paired computer session');
 // The owner sees it in the shared project; the paired computer never sees the folder.
 await until(async()=>{await page.evaluate(()=>loadSessions());return (await rowsIn(projectBlock(page,'网站')).filter({hasText:'fixture:cwd'}).count())===2;},30,'the paired computer\'s session is not listed in the shared project');
 const seenB=await pageB.evaluate(()=>({nodes:JSON.stringify(conv.nodes),records:(conv.list?.remote||[]).every(r=>!('cwd' in r.session))}));
 assert(seenB.records&&!seenB.nodes.includes(site),'the shared folder reached the paired computer');

 stage='older daemons keep the flat list';
 const legacy=await page.evaluate(()=>{
  const caps=state.runtime.capabilities;caps.projects=false;renderSessionList();
  const seen={tools:document.querySelector('#session-tools').hidden,footer:document.querySelector('#session-footer').hidden,projects:document.querySelectorAll('#session-list .project').length,rows:document.querySelectorAll('#session-list .session-row').length,earlier:document.querySelectorAll('#session-list .session-row.earlier').length};
  caps.projects=true;renderSessionList();return seen;
 });
 assert(legacy.tools&&legacy.footer&&!legacy.projects&&legacy.rows>0&&!legacy.earlier,'without the projects capability the sidebar is not the flat list: '+JSON.stringify(legacy));

 stage='screenshots';
 const blogSession=rowsIn(projectBlock(page,'blog')).filter({hasText:'整理博客的目录结构'});
 for(const mode of ['light','dark','narrow']){
  await page.emulateMedia({colorScheme:mode==='dark'?'dark':'light'});
  await page.setViewportSize(mode==='narrow'?{width:390,height:844}:{width:1440,height:960});
  const toggle=page.locator('#sidebar-toggle'),narrow=mode==='narrow';
  if(narrow&&await toggle.getAttribute('aria-expanded')!=='true')await toggle.click();
  await blogSession.click();await reply.waitFor();
  if(narrow)await page.evaluate(()=>scrollTo(0,0));
  await shot(page,`sidebar-${mode}.png`);
  if(narrow){await toggle.click();}
  await shot(page,`continued-session-${mode}.png`,{fullPage:narrow});
  if(narrow)assert(!await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth+1),'the continued session overflows a phone-sized window');
  if(narrow)await toggle.click();
  await page.locator('#session-list .add-project').click();await dialog.waitFor();
  await dialog.locator('.folder-row').filter({hasText:notes}).waitFor({timeout:30000});
  await dialog.locator('.folder-empty.loading').waitFor({state:'detached',timeout:30000});
  await shot(page,`add-project-${mode}.png`);
  if(narrow){const box=await dialog.boundingBox();assert(box.x>=0&&box.x+box.width<=390,'the add-project dialog overflows a phone-sized window');}
  await page.keyboard.press('Escape');await dialog.waitFor({state:'hidden'});
  if(narrow)await toggle.click();
  await page.locator('#new-session').click();await page.locator('#conversation-empty').waitFor();
  await picker.selectOption({label:'blog'});
  await shot(page,`new-session-${mode}.png`);
  if(narrow)assert(!await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth+1),'the new-session screen overflows a phone-sized window');
  await noInternals(page,`${mode} screens`);
 }
 assert(problems.length===0,`browser errors: ${problems.join(' | ')}`);
 console.log(JSON.stringify({ok:true,model_calls:0,checks:['projects-in-sidebar','add-from-history-and-path','continue-earlier-session','search-rename-pin-hide','project-rename-move-share','paired-computer-uses-shared-project','no-internals'],screenshots:taken}));
 for(const path of taken)console.log(path);
}catch(e){console.error(`projects panel smoke failed at ${stage}: ${e.stack||e} diagnostics=${root}`);process.exitCode=1;}
finally{await browser?.close();for(const child of owned.reverse()){try{child.kill('SIGTERM');}catch{}}}
