// Real browser + two real TLS Nodes, a panel on each: pairing from the panel.
// pair-asks pastes pair-approves' pairing code; pair-approves approves once with
// the check code; pair-asks finishes by itself. Each side then lists the other's
// Agent. No model is called; the only executable Agent is the test fixture.
import {spawn,spawnSync} from 'node:child_process';
import {mkdtempSync,mkdirSync,writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {Mesh,assert,until} from './acceptance/common.mjs';
const {chromium}=await import(process.env.HMESH_PLAYWRIGHT_MODULE||'playwright');
const root=mkdtempSync(join(tmpdir(),'hmesh-pairing-'));
const binary=resolve('target/debug',process.platform==='win32'?'hmesh.exe':'hmesh');
const fixture=resolve('target/debug',process.platform==='win32'?'hmesh-fixture.exe':'hmesh-fixture');
const owned=[];let browser;let stage='setup';
function start(args,stdout='ignore'){const child=spawn(binary,args,{stdio:['ignore',stdout,'ignore'],windowsHide:true});owned.push(child);return child;}
async function node(name,agent,network=[]){
 const dir=join(root,name),init=spawnSync(binary,['--json','--data-dir',dir,'init','--name',name,'--port','0',...network],{encoding:'utf8'});
 assert(init.status===0,'CLI init failed');const mesh=new Mesh(binary,dir);start(['--data-dir',dir,'daemon','start']);
 const card=await until(()=>{try{mesh.call(['daemon','status']);return mesh.call(['node','show']);}catch{return null;}});
 if(!agent)return{mesh,card,dir};
 const project=join(root,`${name}-project`),config=join(root,`${name}.toml`);mkdirSync(project);
 writeFileSync(config,['[agent]',`name="${agent}"`,'harness="acp"',`project_id="${name}"`,'[project]','root='+JSON.stringify(project),'allowed_tools=["read"]','[adapter]','executable='+JSON.stringify(fixture),'env_allowlist=[]',''].join('\n'));
 mesh.call(['agent','register','--config',config]);
 return{mesh,card,dir};
}
async function panel(node){
 const child=start(['--json','--data-dir',node.dir,'panel','--port','0'],'pipe');
 const lines=createInterface({input:child.stdout});
 const[line]=await Promise.race([once(lines,'line'),new Promise((_,reject)=>{const t=setTimeout(()=>reject(Error('Panel startup timeout')),20000);t.unref();})]);
 lines.close();return JSON.parse(line).url;
}
const trusted=mesh=>mesh.call(['peers','list']).filter(p=>(p.status??p.peer?.status)==='trusted').map(p=>(p.card??p.peer?.card).node_id);
const sees=(mesh,agent)=>JSON.stringify(mesh.call(['agents','list','--all'])).includes(`"${agent}"`);
try{
 const a=await node('pair-asks','asks-agent'),b=await node('pair-approves','approves-agent');
 const aUrl=await panel(a),bUrl=await panel(b);
 const errors=[];browser=await chromium.launch({headless:true,...(process.env.HMESH_CHROME?{executablePath:process.env.HMESH_CHROME}:{})});
 async function open(url,options={}){
  const page=await browser.newPage({viewport:{width:1280,height:900},...options});
  page.on('pageerror',e=>errors.push(String(e)));
  // The two deliberate mistakes below are answered with 400; nothing else may fail.
  page.on('console',m=>{if(m.type()==='error'&&!/status of 400/.test(m.text()))errors.push(m.text());});
  await page.goto(url);await page.locator('#local-name').filter({hasText:/pair-/}).waitFor({state:'attached'});
  await page.locator('[data-view="nodes"]').click();await page.locator('#pair-new').waitFor();return page;
 }
 const pa=await open(aUrl),pb=await open(bUrl);
 stage='read-code';
 await pb.locator('#pair-new').click();
 await pb.getByRole('button',{name:'本机配对码',exact:true}).click();
 await pb.getByRole('button',{name:'复制配对码',exact:true}).waitFor();
 assert(await pb.locator('#pair-code').isHidden(),'The remote input stayed visible in the local code view');
 const code=(await pb.locator('#own-pair-code').textContent()).trim();
 assert(code===b.mesh.call(['pair','show']).pair_code,'Panel pairing code differs from the CLI');
 await pb.locator('[data-close="pair-dialog"]').click();
 stage='send-request';
 await pa.locator('#pair-new').click();await pa.locator('#pair-code').fill('not a code');await pa.locator('#pair-send').click();
 await pa.locator('#pair-message.error').filter({hasText:'这不是配对码'}).waitFor();
 // A code that wrapped when it was pasted still reads.
 await pa.locator('#pair-code').fill(code.slice(0,30)+'\n'+code.slice(30));await pa.locator('#pair-send').click();
 const check=(await pa.locator('.pair-digits').textContent()).trim();
 assert(/^[0-9A-F]{6}$/.test(check)&&a.card.fingerprint.toUpperCase().endsWith(check),'Check code is not the last six digits of the asking computer');
 assert(!trusted(a.mesh).length&&!trusted(b.mesh).length,'A side trusts the other before its approval');
 assert(!sees(a.mesh,'approves-agent')&&!sees(b.mesh,'asks-agent'),'An Agent is visible before pairing finished');
 stage='request-on-the-other-side';
 await pb.reload();await pb.locator('#local-name').filter({hasText:/pair-/}).waitFor({state:'attached'});
 await pb.locator('[data-view="inbox"]').click();await pb.locator('.inbox-item[data-kind="pair"]').waitFor();
 assert(await pb.locator('#nav-inbox-count').textContent()==='1','Pair request is not counted as owner work');
 await pb.locator('.inbox-item[data-kind="pair"] .button').click();
 const request=pb.locator('.pair-card[data-direction="incoming"]');await request.waitFor();
 for(const options of [{colorScheme:'dark'},{viewport:{width:390,height:844}}]){
  const page=await open(bUrl,options);await page.locator('.pair-card[data-direction="incoming"]').waitFor();
  assert(!await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),'Pairing page overflows');
  await page.setViewportSize({width:360,height:450});
  await page.locator('#pair-new').click();
  await page.getByRole('button',{name:'本机配对码',exact:true}).click();
  assert(await page.evaluate(()=>{
   const scroll=document.querySelector('.pair-scroll'),footer=document.querySelector('.pair-footer');
   return scroll.scrollWidth<=scroll.clientWidth&&footer.getBoundingClientRect().bottom<=innerHeight;
  }),'Pairing dialog overflows or hides its actions');
  await page.close();
 }
 stage='approve';
 await pb.locator('.pair-check').fill('000000');await request.getByRole('button',{name:'批准',exact:true}).click();
 await request.locator('.pair-message').filter({hasText:'校验码不对'}).waitFor();
 assert(!trusted(b.mesh).length,'A wrong check code approved the request');
 await pb.locator('.pair-check').fill(check.toLowerCase());await request.getByRole('button',{name:'批准',exact:true}).click();
 stage='finish-on-its-own';
 // The dialog left open on pair-asks turns into the finished state.
 await pa.locator('.pair-sent-title').filter({hasText:'已和 pair-approves 配对'}).waitFor({timeout:20000});
 await pa.locator('#pair-body .confirm-actions .button').click();
 await pa.locator('#node-list .entity-card').filter({hasText:'pair-approves'}).waitFor();
 await pa.getByRole('button',{name:'查看远端 Agent',exact:true}).click();
 await pa.locator('#remote-agents').getByRole('heading',{name:'approves-agent',exact:true}).waitFor();
 await pa.getByRole('button',{name:'关闭列表',exact:true}).click();
 assert(await pa.locator('#remote-agents').isHidden(),'Remote Agent list did not close');
 assert(trusted(a.mesh).includes(b.card.node_id)&&trusted(b.mesh).includes(a.card.node_id),'Pairing is not trusted on both sides');
 assert(sees(a.mesh,'approves-agent')&&sees(b.mesh,'asks-agent'),'A paired computer cannot see the other side\'s Agent');
 const text=await pa.evaluate(()=>document.body.innerText);
 assert(!['{"','request_id','undefined','null'].some(word=>text.includes(word)),'Internal fields shown on the page');
 stage='direct-network-and-mcp-only';
 const direct=await node('pair-direct','direct-agent',['--network','direct','--listen-address','127.0.0.1']);
 const caller=await node('pair-mcp',null,['--network','direct','--outbound-only']);
 const directPage=await open(await panel(direct)),callerPage=await open(await panel(caller));
 assert(direct.mesh.call(['daemon','status']).pairing_window.closes_at===null,'Direct pairing opened before the user opened the dialog');
 await directPage.locator('#pair-new').click();
 await directPage.locator('.pair-window').filter({hasText:'配对窗口还剩'}).waitFor();
 assert(direct.mesh.call(['daemon','status']).pairing_window.closes_at>Date.now()/1000,'Direct pairing dialog did not open the server window');
 await directPage.getByRole('button',{name:'本机配对码',exact:true}).click();
 const directCode=await directPage.locator('#own-pair-code').textContent();
 await directPage.locator('[data-close="pair-dialog"]').click();
 await callerPage.locator('#pair-new').click();
 await callerPage.locator('#pair-code').fill(directCode);
 assert(await callerPage.locator('#own-pair-code').count()===0,'MCP-only node exposed an inbound pairing code');
 await callerPage.locator('#pair-send').click();
 const directCheck=await callerPage.locator('.pair-digits').textContent();
 const directRequest=directPage.locator('.pair-card[data-direction="incoming"]');
 await directRequest.locator('.pair-check').fill(directCheck);
 await directRequest.getByRole('button',{name:'批准',exact:true}).click();
 await callerPage.locator('.pair-sent-title').filter({hasText:'已和 pair-direct 配对'}).waitFor({timeout:20000});
 // Both fixtures bind loopback. Model the connectivity value a LAN listener
 // reports for an outbound-only peer, without exposing this test over the LAN.
 await directPage.route('**/api/overview',async route=>{
  const response=await route.fetch(),snapshot=await response.json();
  for(const peer of snapshot.peers.data||[])if(peer.card.node_id===caller.card.node_id)peer.node_connectivity='outbound_only';
  await route.fulfill({response,json:snapshot});
 });
 await directPage.locator('#refresh').click();
 const callerCard=directPage.locator('.paired-card').filter({hasText:'pair-mcp'});
 await callerCard.getByText('只调用',{exact:true}).waitFor();
 assert(await callerCard.getByRole('button',{name:'查看远端 Agent',exact:true}).count()===0,'MCP-only peer offered an inbound Agent action');
 assert(!errors.length,'Browser errors: '+errors.join(' | '));
 console.log(JSON.stringify({passed:true,model_calls:0,scope:'isolated mTLS nodes and headless browser',checks:['pairing-code-matches-cli','bad-code-explained','check-code-is-fingerprint-tail','no-trust-before-approval','request-counted-in-inbox','no-overflow','wrong-check-rejected','asking-side-finishes-itself','trusted-both-ways','agents-visible-both-ways','no-internal-fields','direct-pairing-window','mcp-only-pairs-outbound']}));
}catch(e){console.error(stage+': '+e.message+' diagnostics='+root);process.exitCode=1;}
finally{await browser?.close();for(const p of owned.reverse()){if(p.exitCode===null){p.kill();await Promise.race([once(p,'exit'),new Promise(r=>setTimeout(r,3000))]);}}}
