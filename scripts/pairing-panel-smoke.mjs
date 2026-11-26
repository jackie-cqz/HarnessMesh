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
function start(args,stdout='ignore',env={}){const child=spawn(binary,args,{stdio:['ignore',stdout,'ignore'],windowsHide:true,env:{...process.env,...env}});owned.push(child);return child;}
async function node(name,agent,network=[],env={}){
 const dir=join(root,name),init=spawnSync(binary,['--json','--data-dir',dir,'init','--name',name,'--port','0',...network],{encoding:'utf8'});
 assert(init.status===0,'CLI init failed');const mesh=new Mesh(binary,dir);start(['--data-dir',dir,'daemon','start'],'ignore',env);
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
  // The deliberate mistakes below are answered with 400 (a pairing code or
  // check code) or 502 (an address the daemon refuses); nothing else may fail.
  page.on('console',m=>{if(m.type()==='error'&&!/status of 400|status of 502/.test(m.text()))errors.push(m.text());});
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
 const callerUrl=await panel(caller),directPage=await open(await panel(direct)),callerPage=await open(callerUrl);
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
 stage='peer-addresses';
 // The card shows where pair-direct is reached; 改地址 sets addresses by hand, kept only here.
 await callerPage.locator('#pair-body .confirm-actions .button').click();
 const directCard=callerPage.locator('.paired-card').filter({hasText:'pair-direct'});
 await directCard.locator('.paired-address-list').filter({hasText:new URL(direct.card.endpoint).host}).waitFor();
 const manualOf=()=>caller.mesh.call(['peers','list']).find(p=>(p.card??p.peer?.card).node_id===direct.card.node_id).routes.manual;
 await directCard.getByRole('button',{name:'改地址',exact:true}).click();
 await directCard.getByRole('button',{name:'取消',exact:true}).click();
 assert(!await directCard.locator('.paired-address-input').count(),'Cancel left the address editor open');
 await directCard.getByRole('button',{name:'改地址',exact:true}).click();
 const addressInput=directCard.locator('.paired-address-input');
 await addressInput.fill('https://user@studio.local:39999');await directCard.getByRole('button',{name:'保存',exact:true}).click();
 await directCard.locator('.pair-message').filter({hasText:'主机名:端口'}).waitFor();
 assert(!manualOf().length,'A refused address was kept');
 await addressInput.fill('studio-backup.local:39999  10.0.0.7:39999');await directCard.getByRole('button',{name:'保存',exact:true}).click();
 await directCard.locator('.paired-address-list').filter({hasText:'studio-backup.local:39999（手动）'}).waitFor();
 assert(JSON.stringify(manualOf())===JSON.stringify(['https://studio-backup.local:39999','https://10.0.0.7:39999']),'Panel addresses differ from the daemon: '+JSON.stringify(manualOf()));
 for(const options of [{colorScheme:'dark'},{viewport:{width:360,height:740}}]){
  const page=await open(callerUrl,options),card=page.locator('.paired-card').filter({hasText:'pair-direct'});
  await card.locator('.paired-address-list').filter({hasText:'（手动）'}).waitFor();
  assert(!await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),'Paired card overflows with its addresses');
  await card.getByRole('button',{name:'改地址',exact:true}).click();await card.locator('.paired-address-input').waitFor();
  assert(await page.evaluate(()=>{
   const row=document.querySelector('.paired-addresses.editing'),box=row.getBoundingClientRect();
   return document.documentElement.scrollWidth<=innerWidth&&[...row.querySelectorAll('input,button')].every(n=>{const r=n.getBoundingClientRect();return r.left>=box.left-1&&r.right<=box.right+1;});
  }),'Address editor overflows its card');
  await page.close();
 }
 await directCard.getByRole('button',{name:'改地址',exact:true}).click();
 assert(await addressInput.inputValue()==='studio-backup.local:39999 10.0.0.7:39999','Editor does not start from the saved addresses');
 await addressInput.fill('');await directCard.getByRole('button',{name:'保存',exact:true}).click();
 await directCard.locator('.paired-address-list').filter({hasText:'（手动）'}).waitFor({state:'detached'});
 assert(!manualOf().length,'Clearing left manual addresses behind');
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
 stage='peer-access-modes';
 // Give this same node a second trusted peer so changing the MCP-only peer
 // can be checked independently from both the panel and the daemon.
 const secondPair=a.mesh.call(['pair','request','--code',directCode.trim()]);
 direct.mesh.call(['pair','approve',secondPair.request_id,'--expect-fingerprint',a.card.fingerprint]);
 await until(()=>trusted(direct.mesh).includes(a.card.node_id)&&trusted(a.mesh).includes(direct.card.node_id));
 await directPage.locator('#refresh').click();
 const otherCard=directPage.locator('.paired-card').filter({hasText:'pair-asks'});
 const mode=(card,text)=>card.locator('.peer-access-mode').filter({hasText:text});
 const modeOf=peer=>direct.mesh.call(['peers','list']).find(p=>(p.card??p.peer?.card).node_id===peer.card.node_id)?.access;
 await callerCard.getByText('发到本机的请求',{exact:true}).waitFor();
 await mode(callerCard,'自动开始').waitFor();
 await mode(otherCard,'自动开始').waitFor();
 await callerCard.getByRole('button',{name:'设为本机确认',exact:true}).click();
 await directPage.locator('#confirm-dialog[open]').waitFor();
 assert(modeOf(caller)==='auto','Peer access changed before confirmation');
 await directPage.locator('#confirm-accept').click();
 await mode(callerCard,'本机确认').waitFor();
 await mode(otherCard,'自动开始').waitFor();
 assert(modeOf(caller)==='confirm'&&modeOf(a)==='auto','Setting one peer to confirm changed the wrong peer');
 await directPage.reload();
 await directPage.locator('#local-name').filter({hasText:'pair-direct'}).waitFor({state:'attached'});
 await directPage.locator('[data-view="nodes"]').click();
 await mode(callerCard,'本机确认').waitFor();
 await mode(otherCard,'自动开始').waitFor();
 await callerCard.getByRole('button',{name:'设为自动开始',exact:true}).click();
 await directPage.locator('#confirm-dialog[open]').waitFor();
 await directPage.locator('#confirm-accept').click();
 await mode(callerCard,'自动开始').waitFor();
 assert(modeOf(caller)==='auto'&&modeOf(a)==='auto','Returning one peer to auto changed the wrong peer');
 stage='trust-network';
 // On every interface, the port is open only to trusted networks. The
 // daemon reads the networks it seems to be on from a file (debug builds).
 const networksFile=join(root,'every-networks.json');
 writeFileSync(networksFile,JSON.stringify([{interface:'en0',address:'192.0.2.10',subnet:'192.0.2.0/24',gateway:'192.0.2.1',gateway_mac:'02:00:5e:00:53:01'}]));
 const every=await node('pair-every',null,['--network','direct','--listen-address','::'],{HMESH_TEST_NETWORKS:networksFile});
 const everyUrl=await panel(every),everyPage=await open(everyUrl);
 const notice=everyPage.locator('.pair-network.untrusted');
 await notice.filter({hasText:'这台电脑现在的网络还没认过：别的电脑连不进来。'}).waitFor();
 assert((await notice.textContent()).includes('192.0.2.0/24 · en0'),'The untrusted network is not named');
 assert(await everyPage.locator('.pair-identity .badge').textContent()==='局域网直连','Network badge is wrong');
 for(const options of [{colorScheme:'dark'},{viewport:{width:360,height:740}}]){
  const page=await open(everyUrl,options);await page.locator('.pair-network.untrusted').waitFor();
  assert(await page.evaluate(()=>{
   const box=document.querySelector('.pair-network').getBoundingClientRect(),trust=document.querySelector('#trust-network').getBoundingClientRect();
   return document.documentElement.scrollWidth<=innerWidth&&trust.left>=box.left-1&&trust.right<=box.right+1&&box.right<=innerWidth;
  }),'The network notice overflows');
  await page.screenshot({path:join(root,`trust-network-${options.colorScheme||'narrow'}.png`)});
  await page.close();
 }
 await everyPage.getByRole('button',{name:'信任这个网络',exact:true}).click();
 await everyPage.locator('.pair-network').waitFor({state:'detached'});
 const trustedNetworks=every.mesh.call(['networks','list']).trusted;
 assert(trustedNetworks.length===1&&trustedNetworks[0].subnet==='192.0.2.0/24'&&trustedNetworks[0].gateway==='02:00:5e:00:53:01','The panel did not trust the network: '+JSON.stringify(trustedNetworks));
 assert(every.mesh.call(['node','show']).endpoint.startsWith('https://192.0.2.10:'),'The card does not name the trusted network');
 await everyPage.screenshot({path:join(root,'trust-network-trusted.png')});
 stage='trust-subnet-only';
 // On a network whose router's hardware address cannot be read, trusting it
 // goes by the subnet alone, and the panel says so before and after.
 writeFileSync(networksFile,JSON.stringify([{interface:'en5',address:'198.51.100.7',subnet:'198.51.100.0/24'},{interface:'en0',address:'192.0.2.10',subnet:'192.0.2.0/24',gateway:'192.0.2.1',gateway_mac:'02:00:5e:00:53:01'}]));
 every.mesh.call(['networks','list']);
 await everyPage.locator('#refresh').click();
 const before=everyPage.locator('.pair-network.untrusted').filter({hasText:'198.51.100.0/24 · en5'});
 await before.filter({hasText:'信任后只按网段认'}).waitFor();
 await everyPage.getByRole('button',{name:'信任这个网络',exact:true}).click();
 const after=everyPage.locator('.pair-network').filter({hasText:'198.51.100.0/24 只按网段认：同网段的别的网络也会被当成认过。'});
 await after.waitFor();
 assert(!await everyPage.locator('.pair-network.untrusted').count(),'Still offered after trusting');
 const listed=every.mesh.call(['networks','list']);
 assert(listed.trusted.some(t=>t.subnet==='198.51.100.0/24'&&t.subnet_only===true)&&listed.notes.some(n=>n.includes('198.51.100.0/24 is trusted by its subnet only')),'The CLI does not say it is trusted by subnet only: '+JSON.stringify(listed));
 for(const options of [{colorScheme:'dark'},{viewport:{width:360,height:740}}]){
  const page=await open(everyUrl,options);await page.locator('.pair-network').filter({hasText:'只按网段认'}).waitFor();
  assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth&&document.querySelector('.pair-network').getBoundingClientRect().right<=innerWidth),'The subnet-only note overflows');
  await page.screenshot({path:join(root,`trust-subnet-only-${options.colorScheme||'narrow'}.png`)});
  await page.close();
 }
 assert(!errors.length,'Browser errors: '+errors.join(' | '));
 console.log(JSON.stringify({passed:true,artifacts:root,model_calls:0,scope:'isolated mTLS nodes and headless browser',checks:['pairing-code-matches-cli','bad-code-explained','check-code-is-fingerprint-tail','no-trust-before-approval','request-counted-in-inbox','no-overflow','wrong-check-rejected','asking-side-finishes-itself','trusted-both-ways','agents-visible-both-ways','no-internal-fields','direct-pairing-window','mcp-only-pairs-outbound','peer-address-shown','peer-address-refused-explained','peer-address-saved-to-daemon','peer-address-no-overflow','peer-address-cleared','peer-access-per-paired-computer','peer-access-survives-reload','mcp-only-peer-access-editable','network-untrusted-said','network-notice-no-overflow','network-trusted-from-panel','network-subnet-only-said-before','network-subnet-only-said-after','network-subnet-only-no-overflow']}));
}catch(e){console.error(stage+': '+e.message+' diagnostics='+root);process.exitCode=1;}
finally{await browser?.close();for(const p of owned.reverse()){if(p.exitCode===null){p.kill();await Promise.race([once(p,'exit'),new Promise(r=>setTimeout(r,3000))]);}}}
