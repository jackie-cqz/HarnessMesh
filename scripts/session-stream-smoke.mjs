// Real browser + two real TLS nodes: how one conversation stream looks,
// whichever agent family produced it. The only agent is the ACP test
// fixture, which plays each family's stream shape; no model is called.
import {spawn,spawnSync} from 'node:child_process';
import {mkdtempSync,mkdirSync,readFileSync,readdirSync,writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {Mesh,assert,until} from './acceptance/common.mjs';
const {chromium}=await import(process.env.HMESH_PLAYWRIGHT_MODULE||'playwright');
const root=mkdtempSync(join(tmpdir(),'hmesh-stream-'));
const shots=process.env.HMESH_SCREENSHOT_DIR||root;
const binary=resolve('target/debug',process.platform==='win32'?'hmesh.exe':'hmesh');
const fixture=resolve('target/debug',process.platform==='win32'?'hmesh-fixture.exe':'hmesh-fixture');
const owned=[],problems=[];let browser,page;let stage='setup';
function start(args,stdout='ignore'){const child=spawn(binary,args,{stdio:['ignore',stdout,'ignore'],windowsHide:true});owned.push(child);return child;}
async function node(name){
 const dir=join(root,name),init=spawnSync(binary,['--json','--data-dir',dir,'init','--name',name,'--port','0'],{encoding:'utf8'});
 assert(init.status===0,'CLI init failed');const mesh=new Mesh(binary,dir);start(['--data-dir',dir,'daemon','start']);
 const card=await until(()=>{try{mesh.call(['daemon','status']);return mesh.call(['node','show']);}catch{return null;}});
 return{mesh,card,dir};
}
function register(node,name){
 const project=join(root,`${name}-project`);mkdirSync(project);const config=join(root,`${name}.toml`);
 writeFileSync(config,['[agent]',`name="${name}"`,'harness="acp"','project_id="demo"','[project]','root='+JSON.stringify(project),'allowed_tools=["read","write","edit"]','[adapter]','executable='+JSON.stringify(fixture),'args=[]','env_allowlist=[]',''].join('\n'));
 return node.mesh.call(['agent','register','--config',config]);
}
async function panel(node){
 const child=start(['--json','--data-dir',node.dir,'panel','--port','0'],'pipe');
 const lines=createInterface({input:child.stdout});
 const[line]=await Promise.race([once(lines,'line'),new Promise((_,reject)=>{const t=setTimeout(()=>reject(Error('Panel startup timeout')),20000);t.unref();})]);
 lines.close();return JSON.parse(line);
}
async function openSession(page,group,agentName,message){
 await page.locator('#new-session').click();
 await page.locator('#session-agents .session-group').filter({has:page.locator('.session-group-name',{hasText:new RegExp('^'+group+'$')})}).getByRole('button',{name:new RegExp(agentName)}).click();
 await page.locator('#conversation-empty').waitFor();
 await page.locator('#hero-target').filter({hasText:agentName}).waitFor();
 await page.locator('#hero-input').fill(message);
 await page.locator('#hero-send').click();
 await page.locator('#conversation').waitFor();
}
const stream=page=>page.locator('#conversation-stream');
const step=(page,id)=>stream(page).locator(`.step[data-tool="${id}"]`);
const settle=page=>page.waitForTimeout(400);
// Opens a step if it is folded, then returns its text.
async function stepText(page,id){
 const row=step(page,id).first();
 if(await row.evaluate(d=>d.tagName==='DETAILS'&&!d.open))await row.locator('summary').first().click();
 return row.innerText();
}
try{
 stage='static rules';
 // C3: what is drawn follows ACP meaning, never which agent sent it.
 const streamSource=readFileSync('web/panel/session-stream.js','utf8');
 assert(!/harness|claude|codex|opencode|\bpi\b|deepseek|\bdsh\b|kimi|minimax|hermes|qoder/i.test(streamSource),'the stream renderer branches on an agent');
 // C4: no HTML strings anywhere in the panel.
 for(const file of readdirSync('web/panel').filter(f=>f.endsWith('.js')))assert(!/innerHTML|outerHTML|insertAdjacentHTML|document\.write/.test(readFileSync(join('web/panel',file),'utf8')),`${file} builds HTML from strings`);

 const a=await node('mac-a'),b=await node('mac-b');
 register(a,'helper');register(b,'worker');
 const pairing=a.mesh.call(['pair','request','--peer',b.card.endpoint,'--expect-fingerprint',b.card.fingerprint]);
 b.mesh.call(['pair','approve',pairing.request_id,'--expect-fingerprint',a.card.fingerprint]);a.mesh.call(['pair','approve',pairing.request_id,'--expect-fingerprint',b.card.fingerprint]);
 const ready=await panel(a);
 browser=await chromium.launch({headless:true,...(process.env.HMESH_CHROME?{executablePath:process.env.HMESH_CHROME}:{})});
 page=await browser.newPage({viewport:{width:1440,height:960}});
 page.on('response',r=>{if(r.status()>=400)problems.push(`${r.request().method()} ${new URL(r.url()).pathname} → ${r.status()}`);});
 page.on('pageerror',e=>problems.push(e.message));page.on('console',m=>{if(m.type()==='error')problems.push(m.text());});
 const answers=[];
 page.on('request',r=>{if(r.method()==='POST'&&/\/permissions\//.test(new URL(r.url()).pathname))answers.push(r.postDataJSON());});
 await page.goto(ready.url);await page.locator('#local-name').filter({hasText:'mac-a'}).waitFor({state:'attached'});
 await page.locator('#session-agents .session-group').filter({hasText:'mac-b'}).getByText('worker').waitFor();

 stage='every family in one turn';
 await openSession(page,'mac-a（本机）','helper','fixture:stream');
 await page.waitForFunction(()=>conv.record?.status==='idle'&&conv.record?.turns>=1,null,{timeout:30000});
 await settle(page);await page.screenshot({path:join(shots,'stream-light.png')});
 await page.emulateMedia({colorScheme:'dark'});await settle(page);await page.screenshot({path:join(shots,'stream-dark.png')});
 await page.emulateMedia({colorScheme:'light'});

 stage='C4 the answer is Markdown, safely';
 const answer=stream(page).locator('.turn').last().locator('.msg.answer');
 await answer.locator('h2',{hasText:'Fixed'}).waitFor();
 assert(await answer.locator('strong',{hasText:'subtracted'}).count()===1,'bold not rendered');
 assert(await answer.locator('table tbody tr').count()===2,'table not rendered');
 assert((await answer.locator('pre code').innerText()).includes('a + b'),'code block not rendered');
 assert(await answer.locator('a[href="https://doc.rust-lang.org/book/"][rel~="noopener"]').count()===1,'http link not rendered');
 assert(await answer.locator('a[href^="javascript"]').count()===0,'a javascript: link was made clickable');
 assert((await answer.innerText()).includes('this link'),'the unsafe link text is gone');
 assert(!(await answer.innerText()).includes('**'),'Markdown markers left in the answer');
 assert((await answer.innerText()).includes('Raw <b>tags</b> and <script>alert(1)</script> stay text.'),'HTML in a reply is not shown as text');
 assert(await answer.locator('script,b').count()===0,'HTML in a reply became elements');
 // An update or content type this panel does not know is skipped quietly.
 assert(await step(page,'future-tool').count()>0,'a step with unknown content is missing');

 stage='C3 the process folds after the turn, failures stay out';
 const turn=stream(page).locator('.turn').last();
 const work=turn.locator('details.work');
 assert(await work.count()===1&&!(await work.evaluate(d=>d.open)),'the finished turn\'s process is not folded');
 const summary=(await work.locator(':scope > summary').innerText()).replace(/\s+/g,' ');
 assert(/运行/.test(summary)&&/编辑/.test(summary)&&/失败/.test(summary),`the fold summary does not say what happened: ${summary}`);
 const failed=turn.locator('.work-failed .step');
 assert(await failed.count()===2,`failed steps are not shown outside the fold: ${await failed.count()}`);
 assert(await turn.locator('.work-failed .step[data-tool="codex-run"]').isVisible(),'failed command hidden');
 await work.locator(':scope > summary').first().click();
 assert(await turn.locator('.work-failed').isHidden(),'failed steps shown twice while the process is open');

 stage='C2 one live plan';
 assert(await turn.locator('.plan').count()===1,`plans drawn: ${await turn.locator('.plan').count()}`);
 assert(await turn.locator('.plan .plan-entry.completed').count()===3,'the plan is not the latest one');

 stage='C3 steps by kind';
 const label=async id=>(await step(page,id).first().locator('.step-label').innerText()).replace(/\s+/g,' ').trim();
 assert(/^读取 lib\.rs/.test(await label('claude-read')),`read: ${await label('claude-read')}`);
 assert(/^搜索/.test(await label('claude-grep'))&&(await label('claude-grep')).includes('fn add'),`search: ${await label('claude-grep')}`);
 assert(/^运行 cargo test/.test(await label('claude-bash')),`claude command: ${await label('claude-bash')}`);
 assert(/^运行 cargo test/.test(await label('codex-run')),`codex command: ${await label('codex-run')}`);
 assert(/^运行 npm test/.test(await label('oc-bash')),`opencode command: ${await label('oc-bash')}`);
 assert(/^运行 git status --short/.test(await label('dsh-shell')),`kind other with a command: ${await label('dsh-shell')}`);
 assert(/^读取 lib\.rs/.test(await label('dsh-read')),`kind other with a path: ${await label('dsh-read')}`);
 assert(/^读取 README\.md/.test(await label('pi-read')),`raw tool name: ${await label('pi-read')}`);
 assert(/^编辑 lib\.rs/.test(await label('claude-edit')),`edit: ${await label('claude-edit')}`);
 const delegate=stream(page).locator('.step.delegate[data-tool="mesh-send"]');
 assert(await delegate.count()===1&&(await delegate.innerText()).includes('mac-b/worker'),'delegation is not its own card');
 await delegate.locator(':scope > summary').click();
 const delegated=await delegate.innerText();
 assert(delegated.includes('Run the integration tests')&&delegated.includes('All 12 integration tests passed.')&&!delegated.includes('{"'),`delegation reply: ${delegated}`);
 assert(await delegate.locator('.delegate-reply strong',{hasText:'integration'}).count()===1,'the other agent\'s answer is not Markdown');

 stage='C2 diffs';
 const edit=step(page,'claude-edit').first();
 assert((await edit.locator('.step-meta').innerText()).replace(/\s/g,'')==='+1−1',`edit stats: ${await edit.locator('.step-meta').innerText()}`);
 await edit.locator('summary').first().click();
 assert((await edit.locator('.diff .add').innerText()).includes('a + b')&&(await edit.locator('.diff .del').innerText()).includes('a - b'),'diff lines missing');
 const created=step(page,'pi-write').first();
 assert((await created.locator('.step-meta').innerText()).includes('+2'),'new file stats');
 assert((await stepText(page,'pi-write')).includes('新文件'),'new file not marked');

 stage='C1 command output from all three places';
 const claude=await stepText(page,'claude-bash');
 assert(claude.includes('test result: ok. 3 passed')&&!claude.includes('```'),`content text output: ${claude}`);
 const codex=await stepText(page,'codex-run');
 assert(codex.includes('running 3 tests')&&codex.includes('test add_works ... FAILED')&&codex.includes('退出 101'),`_meta output: ${codex}`);
 assert((await stepText(page,'pi-bash')).includes('NOTES.md\nREADME.md'),'Pi deltas missing');
 assert((await stepText(page,'mm-run')).includes('No rule to make target'),'rawOutput error missing');
 assert((await stepText(page,'oc-bash')).includes('# pass 3'),'rawOutput output missing');
 const kimi=await stepText(page,'kimi-grep');
 assert(kimi.includes('TODO: handle overflow')&&!kimi.includes('"pattern"'),`arguments shown as output: ${kimi}`);
 await settle(page);await page.screenshot({path:join(shots,'stream-open-light.png'),fullPage:true});
 await page.emulateMedia({colorScheme:'dark'});await settle(page);await page.screenshot({path:join(shots,'stream-open-dark.png'),fullPage:true});
 await page.emulateMedia({colorScheme:'light'});

 stage='events recorded by earlier versions';
 const legacy=await page.evaluate(()=>{
  const events=[
   {seq:1,kind:'user_message',payload:{text:'old'}},
   {seq:2,kind:'tool_call',payload:{id:'x',title:'Read file',kind:'read',status:'pending',input:'{"path":"docs/a.txt"}'}},
   {seq:3,kind:'tool_call_update',payload:{id:'x',status:'completed',output:'hello'}},
   {seq:4,kind:'plan',payload:{entries:[{content:'one',status:'pending'}]}},
   {seq:5,kind:'plan',payload:{entries:[{content:'one',status:'completed'}]}},
   {seq:6,kind:'agent_message_chunk',payload:{text:'done'}},
   {seq:7,kind:'turn_end',payload:{stop_reason:'end_turn'}},
  ];
  const box=document.createElement('div');
  renderStream(box,streamModel(events),{key:'legacy',live:false,open:new Map(),waiting:new Set(),note:()=>null,user:e=>({key:'user',sig:'1',make:()=>{const d=document.createElement('div');d.textContent=e.payload.text;return d;}}),head:[],tail:[]});
  return{label:box.querySelector('.step .step-label')?.textContent,output:box.querySelector('.step .step-output')?.textContent,plans:box.querySelectorAll('.plan').length,done:box.querySelectorAll('.plan .plan-entry.completed').length,answer:box.querySelector('.msg.answer')?.textContent};
 });
 assert(legacy.label==='读取 a.txt docs'&&legacy.output==='hello'&&legacy.plans===1&&legacy.done===1&&legacy.answer==='done',`earlier events: ${JSON.stringify(legacy)}`);
 stage='replies nested too deep stay readable';
 const deep=await page.evaluate(()=>['>'.repeat(8000)+' quoted','- '.repeat(4000)+'item','*'.repeat(9000)+'x','['.repeat(5000)+'x'].map(text=>{try{return renderMarkdown(text).textContent.length>0;}catch(e){return String(e);}}));
 assert(deep.every(r=>r===true),`deep Markdown: ${JSON.stringify(deep)}`);

 stage='running steps stay open; updates keep the page';
 await openSession(page,'mac-a（本机）','helper','fixture:stream-running');
 const live=step(page,'long-run').first();
 await live.waitFor({timeout:30000});
 await page.waitForFunction(()=>conv.events.some(e=>e.payload?.id==='long-run'));
 assert(await stream(page).locator('.turn').last().locator('details.work').evaluate(d=>d.open),'the running turn is folded');
 // Streamed output is recorded about once a second.
 await live.locator('.term-out',{hasText:'watching for changes'}).waitFor({timeout:10000});
 await step(page,'claude-edit').first().locator('summary').first().click();
 await page.evaluate(()=>{window.__kept=[...document.querySelectorAll('#conversation-stream .turn')].at(-1)?.querySelector('.msg.user');});
 await page.screenshot({path:join(shots,'stream-running.png')});
 await page.locator('#composer-send').click();
 await stream(page).getByText('本轮已停止').waitFor({timeout:30000});
 assert(await page.evaluate(()=>!!window.__kept&&window.__kept.isConnected),'the stream was rebuilt instead of updated');
 assert(await step(page,'claude-edit').first().evaluate(d=>d.open),'an opened step closed when the stream updated');

 stage='C5 every option on this computer';
 await openSession(page,'mac-a（本机）','helper','fixture:permission-options');
 const dock=page.locator('#permission-dock .permission');
 await dock.waitFor({timeout:30000});
 const names=await dock.getByRole('button').allInnerTexts();
 assert(JSON.stringify(names)===JSON.stringify(['拒绝','始终拒绝','始终允许','允许一次']),`local options: ${JSON.stringify(names)}`);
 assert((await dock.innerText()).includes('运行 make test'),'the request does not show which step it is for');
 await page.screenshot({path:join(shots,'permission-local.png')});
 await dock.getByRole('button',{name:'始终允许'}).click();
 await stream(page).getByText('Chose always.').waitFor({timeout:30000});
 assert(answers.at(-1)?.option_id==='always'&&answers.at(-1)?.allow===true,`local answer: ${JSON.stringify(answers.at(-1))}`);
 await stream(page).locator('.event-note',{hasText:'始终允许'}).waitFor();

 stage='C5 one-time only for another computer';
 await openSession(page,'mac-b','worker','fixture:permission-options');
 await dock.waitFor({timeout:30000});
 const remoteNames=await dock.getByRole('button').allInnerTexts();
 assert(JSON.stringify(remoteNames)===JSON.stringify(['拒绝','允许一次']),`remote options: ${JSON.stringify(remoteNames)}`);
 await page.screenshot({path:join(shots,'permission-remote.png')});
 await dock.getByRole('button',{name:'允许一次'}).click();
 await stream(page).getByText('Chose allow.').waitFor({timeout:30000});
 assert(answers.at(-1)&&!('option_id' in answers.at(-1))&&answers.at(-1).allow===true,`remote answer: ${JSON.stringify(answers.at(-1))}`);

 stage='C5 a request for a step the stream never showed';
 await openSession(page,'mac-a（本机）','helper','fixture:permission-unseen');
 await dock.waitFor({timeout:30000});
 assert((await dock.innerText()).includes('编辑 notes.txt'),'unseen call not described');
 assert(await dock.locator('.diff .add').count()===1,'the edit being approved is not shown');
 await dock.getByRole('button',{name:'允许一次'}).click();
 await stream(page).getByText('Chose allow.').waitFor({timeout:30000});

 stage='narrow window';
 await page.locator('.session-row').filter({has:page.getByText('fixture:stream',{exact:true})}).first().click();
 await stream(page).locator('.msg.answer h2').waitFor();
 await page.setViewportSize({width:390,height:844});await settle(page);
 assert(!await page.evaluate(()=>{const s=document.querySelector('#conversation-stream');return s.scrollWidth>s.clientWidth+1;}),'the stream overflows a phone-sized window');
 await page.screenshot({path:join(shots,'stream-narrow.png'),fullPage:true});
 assert(problems.length===0,`browser errors: ${problems.join(' | ')}`);
 console.log(JSON.stringify({ok:true,screenshots:shots}));
}catch(e){await page?.screenshot({path:join(shots,'failure.png')}).catch(()=>{});console.error(`session stream smoke failed at ${stage}: ${e.stack||e}${problems.length?`\nbrowser errors: ${problems.join(' | ')}`:''}`);process.exitCode=1;}
finally{await browser?.close();for(const child of owned.reverse()){try{child.kill('SIGTERM');}catch{}}}
