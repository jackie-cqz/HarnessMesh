// Real browser + two real direct-network Nodes, a panel on one: pairing at
// an address typed by hand, and what a paired computer's card says about
// reaching it. address-shows takes connections (loopback stands in for a
// LAN address); address-joins (only MCP) types its host name and the
// passcode its screen shows under 按地址配对. An address that does not
// answer is explained in Chinese (the port refused, the name did not
// resolve); after pairing the card says when and how it was reached last,
// and once address-shows stops, that it cannot be reached and why. Dark and
// narrow layouts are checked for overflow, with screenshots kept next to
// the diagnostics. No model is called; nothing here needs mDNS.
import {spawn,spawnSync} from 'node:child_process';
import {mkdtempSync,mkdirSync,writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {createServer} from 'node:net';
import {Mesh,assert,until} from './acceptance/common.mjs';
const {chromium}=await import(process.env.HMESH_PLAYWRIGHT_MODULE||'playwright');
const root=mkdtempSync(join(tmpdir(),'hmesh-address-'));
const binary=resolve('target/debug','hmesh');
const fixture=resolve('target/debug','hmesh-fixture');
const owned=[];let browser;let stage='setup';const checks=[];
function start(args,stdout='ignore'){const child=spawn(binary,args,{stdio:['ignore',stdout,'ignore']});owned.push(child);return child;}
async function freePort(){const server=createServer();await new Promise(r=>server.listen(0,'127.0.0.1',r));const {port}=server.address();await new Promise(r=>server.close(r));return String(port);}
async function node(name,agent,network,port='0'){
 const dir=join(root,name),init=spawnSync(binary,['--json','--data-dir',dir,'init','--name',name,'--port',port,'--network','direct',...network],{encoding:'utf8'});
 assert(init.status===0,'CLI init failed: '+init.stderr);const mesh=new Mesh(binary,dir);start(['--data-dir',dir,'daemon','start']);
 const card=await until(()=>{try{mesh.call(['daemon','status']);return mesh.call(['node','show']);}catch{return null;}});
 if(agent){
  const project=join(root,`${name}-project`),config=join(root,`${name}.toml`);mkdirSync(project);
  writeFileSync(config,['[agent]',`name="${agent}"`,'harness="acp"',`project_id="${name}"`,'[project]','root='+JSON.stringify(project),'allowed_tools=["read"]','[adapter]','executable='+JSON.stringify(fixture),'env_allowlist=[]',''].join('\n'));
  mesh.call(['agent','register','--config',config]);
 }
 return{mesh,card,dir};
}
async function panel(node){
 const child=start(['--json','--data-dir',node.dir,'panel','--port','0'],'pipe');
 const lines=createInterface({input:child.stdout});
 const[line]=await Promise.race([once(lines,'line'),new Promise((_,reject)=>{const t=setTimeout(()=>reject(Error('Panel startup timeout')),20000);t.unref();})]);
 lines.close();return JSON.parse(line).url;
}
const peerOf=(mesh,id)=>mesh.call(['peers','list']).find(p=>(p.card??p.peer?.card).node_id===id);
const fits=page=>page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth&&[...document.querySelectorAll('.pair-address,.paired-card')].every(e=>e.scrollWidth<=e.clientWidth+1));
try{
 const port=await freePort();
 const b=await node('address-shows','shows-agent',['--listen-address','127.0.0.1'],port),a=await node('address-joins',null,['--outbound-only']);
 const aUrl=await panel(a);
 const errors=[];browser=await chromium.launch({headless:true,...(process.env.HMESH_CHROME?{executablePath:process.env.HMESH_CHROME}:{})});
 async function open(url,options={}){
  const page=await browser.newPage({viewport:{width:1280,height:900},...options});
  page.on('pageerror',e=>errors.push(String(e)));
  // The addresses that do not answer below are answered with an error status.
  page.on('console',m=>{if(m.type()==='error'&&!/status of 50\d|status of 40\d/.test(m.text()))errors.push(m.text());});
  await page.goto(url);await page.locator('#local-name').filter({hasText:/address-/}).waitFor({state:'attached'});
  await page.locator('[data-view="nodes"]').click();await page.locator('#pair-new').waitFor();return page;
 }
 const pa=await open(aUrl);
 async function tryAddress(page,address,passcode){
  await page.locator('#pair-address-input').fill(address);await page.locator('.pair-address-passcode').fill(passcode);
  await page.locator('#pair-address .button').click();
 }
 stage='refused-explained';
 await pa.locator('#pair-new').click();await pa.locator('#pair-address').waitFor();
 const closed=await freePort();
 await tryAddress(pa,`localhost:${closed}`,'123456');
 const refused=pa.locator('#pair-address .pair-message.error').filter({hasText:'连不上这台电脑'});
 await refused.waitFor({timeout:15000});
 assert((await refused.textContent()).includes('防火墙或网络隔离'),'A refused port is not explained: '+await refused.textContent());
 checks.push('refused-explained');
 stage='unresolved-explained';
 await tryAddress(pa,'nowhere.invalid','123456');
 await pa.locator('#pair-address .pair-message.error').filter({hasText:'主机名解析不出来'}).waitFor({timeout:15000});
 checks.push('unresolved-explained');
 stage='bad-address-explained';
 await tryAddress(pa,'http://nowhere.invalid/path','123456');
 await pa.locator('#pair-address .pair-message.error').filter({hasText:'地址写成 IP 或主机名'}).waitFor({timeout:15000});
 await tryAddress(pa,'localhost','12');
 await pa.locator('#pair-address .pair-message.error').filter({hasText:'6 位数字'}).waitFor();
 checks.push('bad-input-explained');
 stage='no-overflow-dialog';
 for(const [name,options] of [['dark',{colorScheme:'dark'}],['narrow',{viewport:{width:360,height:740}}]]){
  const page=await open(aUrl,options);await page.locator('#pair-new').click();await page.locator('#pair-address').waitFor();
  await tryAddress(page,`localhost:${closed}`,'123456');
  await page.locator('#pair-address .pair-message.error').filter({hasText:'连不上这台电脑'}).waitFor({timeout:15000});
  assert(await fits(page),`The address row overflows (${name})`);
  await page.locator('#pair-address').screenshot({path:join(root,`address-row-${name}.png`)});
  await page.close();
 }
 checks.push('address-row-no-overflow');
 stage='pairs-at-a-host-name';
 const passcode=b.mesh.call(['pair','show']).pairing_window.passcode;
 assert(/^[0-9]{6}$/.test(passcode),'No passcode on address-shows');
 await tryAddress(pa,`localhost:${port}`,passcode.slice(0,3)+' '+passcode.slice(3));
 await pa.locator('.pair-sent-title').filter({hasText:'已和 address-shows 配对'}).waitFor({timeout:20000});
 assert(peerOf(a.mesh,b.card.node_id)?.status==='trusted'&&peerOf(b.mesh,a.card.node_id)?.status==='trusted','Pairing is not trusted on both sides');
 checks.push('paired-at-a-host-name');
 stage='last-reached';
 await pa.locator('#pair-body .confirm-actions .button').click();
 const card=pa.locator('.paired-card').filter({hasText:'address-shows'});
 await card.locator('.paired-address-list').filter({hasText:`localhost:${port}（手动）`}).waitFor({timeout:15000});
 // The laptop asks for the other's addresses right after pairing; that call is where it answered last.
 await until(()=>peerOf(a.mesh,b.card.node_id)?.routes?.last_good_at||null,30);
 await pa.reload();await pa.locator('#local-name').filter({hasText:/address-/}).waitFor({state:'attached'});await pa.locator('[data-view="nodes"]').click();
 const reach=card.locator('.paired-reach');await reach.waitFor({timeout:15000});
 assert(/^上次连上：(刚刚|\d+分钟前)，经 (本机回环|主机名|手动)$/.test(await reach.textContent()),'Last reached is not said: '+await reach.textContent());
 const via=await pa.evaluate(()=>[routeVia({manual:['https://x.local:1']},'https://x.local:1'),routeVia({},'https://studio.local:39880'),routeVia({},'https://100.101.1.2:39880'),routeVia({},'https://[fd7a:115c:a1e0::5]:39880'),routeVia({},'https://192.168.1.20:39880'),routeVia({},'https://[fd12::5]:39880'),routeVia({},'https://127.0.0.1:39880'),routeVia({},'https://100.12.0.1:39880')]);
 assert(JSON.stringify(via)===JSON.stringify(['手动','主机名','Tailscale','Tailscale','局域网','局域网','本机回环','局域网']),'Routes are named wrongly: '+JSON.stringify(via));
 checks.push('last-reached-said','route-kinds');
 stage='unreachable-card';
 b.mesh.call(['daemon','stop']);await until(()=>{try{b.mesh.call(['daemon','status']);return null;}catch{return true;}});
 // The card is read every ten seconds; the second read has where it stopped.
 const problem=card.locator('.paired-problem').filter({hasText:'防火墙或网络隔离'});
 await problem.waitFor({timeout:45000});
 await card.locator('.badge').filter({hasText:'不可达'}).waitFor();
 assert(await card.locator('.paired-reach').count()===1,'Last reached is gone once unreachable');
 checks.push('unreachable-card-explained');
 stage='no-overflow-card';
 for(const [name,options] of [['dark',{colorScheme:'dark'}],['narrow',{viewport:{width:360,height:740}}]]){
  const page=await open(aUrl,options);
  const shown=page.locator('.paired-card').filter({hasText:'address-shows'});await shown.locator('.paired-problem').waitFor({timeout:45000});
  assert(await fits(page),`The paired card overflows (${name})`);
  await shown.screenshot({path:join(root,`paired-card-${name}.png`)});
  await page.close();
 }
 checks.push('card-no-overflow');
 const text=await pa.evaluate(()=>document.body.innerText);
 assert(!['{"','request_id','undefined','null','refused the connection','did not resolve','not_heard','unreachable_reasons'].some(word=>text.includes(word)),'Internal fields shown on the page');
 assert(!errors.length,'Browser errors: '+errors.join(' | '));
 checks.push('no-internal-fields');
 console.log(JSON.stringify({passed:true,artifacts:root,model_calls:0,scope:'isolated two-node direct network on loopback and headless browser',checks}));
}catch(e){console.error(stage+': '+e.message+' diagnostics='+root);process.exitCode=1;}
finally{await browser?.close();for(const p of owned.reverse()){if(p.exitCode===null){p.kill();await Promise.race([once(p,'exit'),new Promise(r=>setTimeout(r,3000))]);}}}
