// Real browser + two real direct-network Nodes, a panel on each: pairing with
// a computer found nearby. pair-shows (takes connections; loopback stands in
// for a LAN address) shows a passcode while 配对新电脑 is open; pair-joins
// (only MCP) lists it under 附近的电脑, a wrong passcode is refused, and the
// right one pairs both ways with nothing to approve. No model is called.
// Nearby computers are found over mDNS, which reaches loopback on macOS;
// other systems may not route multicast there, so they skip this check.
import {spawn,spawnSync} from 'node:child_process';
import {mkdtempSync,mkdirSync,writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {createServer} from 'node:net';
import {Mesh,assert,until} from './acceptance/common.mjs';
if(process.platform!=='darwin'){console.log(JSON.stringify({skipped:true,reason:'multicast on loopback is reliable on macOS only'}));process.exit(0);}
const {chromium}=await import(process.env.HMESH_PLAYWRIGHT_MODULE||'playwright');
const root=mkdtempSync(join(tmpdir(),'hmesh-nearby-'));
const binary=resolve('target/debug','hmesh');
const fixture=resolve('target/debug','hmesh-fixture');
const owned=[];let browser;let stage='setup';
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
const trusted=mesh=>mesh.call(['peers','list']).filter(p=>(p.status??p.peer?.status)==='trusted').map(p=>(p.card??p.peer?.card).node_id);
try{
 const b=await node('pair-shows','shows-agent',['--listen-address','127.0.0.1'],await freePort()),a=await node('pair-joins',null,['--outbound-only']);
 const aUrl=await panel(a),bUrl=await panel(b);
 const errors=[];browser=await chromium.launch({headless:true,...(process.env.HMESH_CHROME?{executablePath:process.env.HMESH_CHROME}:{})});
 async function open(url,options={}){
  const page=await browser.newPage({viewport:{width:1280,height:900},...options});
  page.on('pageerror',e=>errors.push(String(e)));
  // The deliberate wrong passcode below is answered with an error status.
  page.on('console',m=>{if(m.type()==='error'&&!/status of 50\d|status of 40\d/.test(m.text()))errors.push(m.text());});
  await page.goto(url);await page.locator('#local-name').filter({hasText:/pair-/}).waitFor({state:'attached'});
  await page.locator('[data-view="nodes"]').click();await page.locator('#pair-new').waitFor();return page;
 }
 const pa=await open(aUrl),pb=await open(bUrl);
 stage='not-announced-while-closed';
 await pa.locator('#pair-new').click();await pa.locator('.pair-nearby-empty').waitFor({timeout:15000});
 assert(!await pa.locator('.pair-nearby-row').count(),'A computer is listed nearby before its pairing window opened');
 await pa.locator('[data-close="pair-dialog"]').click();
 stage='shows-passcode';
 await pb.locator('#pair-new').click();await pb.locator('.pair-passcode-digits').waitFor();
 const passcode=(await pb.locator('.pair-passcode-digits').textContent()).replace(/\s/g,'');
 assert(/^[0-9]{6}$/.test(passcode)&&passcode===b.mesh.call(['daemon','status']).pairing_window.passcode,'Panel passcode differs from the daemon');
 stage='listed-nearby';
 await pa.locator('#pair-new').click();
 const row=pa.locator('.pair-nearby-row').filter({hasText:'pair-shows'});await row.waitFor({timeout:15000});
 assert((await row.innerText()).includes(new URL(b.card.endpoint).host),'Nearby row does not show the address');
 for(const options of [{colorScheme:'dark'},{viewport:{width:390,height:844}}]){
  const page=await open(aUrl,options);await page.locator('#pair-new').click();await page.locator('.pair-nearby-row').first().waitFor({timeout:15000});
  assert(!await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),'Pairing dialog overflows');await page.close();
 }
 stage='wrong-passcode';
 await row.locator('button').click();
 await pa.locator('.pair-passcode').fill(passcode==='000000'?'000001':'000000');await pa.locator('.pair-passcode-form button').click();
 await pa.locator('.pair-nearby-row .pair-message.error').filter({hasText:'口令不对'}).waitFor({timeout:15000});
 assert(!trusted(a.mesh).length&&!trusted(b.mesh).length,'A wrong passcode trusted someone');
 assert(!await pa.locator('.pair-passcode-form button').isDisabled(),'Pair button stays disabled after a wrong passcode');
 stage='right-passcode';
 await pa.locator('.pair-passcode').fill(passcode.slice(0,3)+' '+passcode.slice(3));await pa.locator('.pair-passcode-form button').click();
 await pa.locator('.pair-sent-title').filter({hasText:'已和 pair-shows 配对'}).waitFor({timeout:20000});
 assert(trusted(a.mesh).includes(b.card.node_id)&&trusted(b.mesh).includes(a.card.node_id),'Pairing is not trusted on both sides');
 assert(!b.mesh.call(['pair','list']).some(p=>p.direction==='incoming'&&p.status==='pending'),'Something is left to approve');
 stage='passcode-used';
 await until(()=>b.mesh.call(['daemon','status']).pairing_window.passcode===null||null);
 // The dialog left open on pair-shows follows the daemon, without a reload.
 await pb.locator('.pair-passcode-own').filter({hasText:'口令已经用过了'}).waitFor({timeout:15000});
 stage='three-wrong-tries';
 await pb.locator('.pair-window-line .button').filter({hasText:'换一个口令'}).click();
 await pb.locator('.pair-passcode-digits').waitFor();
 const fresh=(await pb.locator('.pair-passcode-digits').textContent()).replace(/\s/g,'');
 const wrong=fresh==='000000'?'000001':'000000';
 for(let i=0;i<3;i++){const r=spawnSync(binary,['--json','--data-dir',a.dir,'pair','join',new URL(b.card.endpoint).host,'--passcode',wrong],{encoding:'utf8'});assert(r.status!==0&&/passcode did not match/.test(r.stderr),'A wrong passcode was not refused: '+r.stderr);}
 await pb.locator('.pair-passcode-own').filter({hasText:'口令错了 3 次'}).waitFor({timeout:15000});
 assert(await pb.locator('#own-pair-code').count()===1&&(await pb.locator('#own-pair-code').textContent()).startsWith('hmesh://'),'The pairing code is gone after three wrong passcodes');
 assert(b.mesh.call(['daemon','status']).pairing_window.closes_at!==null,'Three wrong passcodes closed pairing by code too');
 stage='daemon-restart';
 // Restarted, the daemon has closed pairing; the dialog left open says so.
 await pb.locator('.pair-window-line .button').filter({hasText:'换一个口令'}).click();await pb.locator('.pair-passcode-digits').waitFor();
 b.mesh.call(['daemon','stop']);await until(()=>{try{b.mesh.call(['daemon','status']);return null;}catch{return true;}});
 start(['--data-dir',b.dir,'daemon','start']);await until(()=>{try{return b.mesh.call(['daemon','status']).running;}catch{return null;}});
 await pb.locator('.pair-window').filter({hasText:'配对窗口已关闭'}).waitFor({timeout:15000});
 assert(!await pb.locator('.pair-passcode-digits').count(),'An old passcode is still shown after the daemon restarted');
 const text=await pa.evaluate(()=>document.body.innerText)+await pb.evaluate(()=>document.body.innerText);
 assert(!['{"','request_id','undefined','null','mDNS','SPAKE'].some(word=>text.includes(word)),'Internal fields shown on the page');
 assert(!errors.length,'Browser errors: '+errors.join(' | '));
 console.log(JSON.stringify({passed:true,model_calls:0,scope:'isolated two-node direct network, mDNS on loopback and headless browser',checks:['not-listed-while-closed','passcode-matches-daemon','listed-nearby-with-address','no-overflow','wrong-passcode-trusts-nobody','button-usable-after-mistake','right-passcode-pairs-both-ways','nothing-to-approve','passcode-used-once','dialog-follows-the-daemon','three-wrong-end-only-the-passcode','restart-closes-the-dialog-window','no-internal-fields']}));
}catch(e){console.error(stage+': '+e.message+' diagnostics='+root);process.exitCode=1;}
finally{await browser?.close();for(const p of owned.reverse()){if(p.exitCode===null){p.kill();await Promise.race([once(p,'exit'),new Promise(r=>setTimeout(r,3000))]);}}}
