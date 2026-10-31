// Single-machine real TLS test of the guardian's intentional exit/restart protocol.
import {spawn,spawnSync} from 'node:child_process';
import {randomUUID} from 'node:crypto';
import {mkdtempSync,mkdirSync,writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import net from 'node:net';
import {Mesh,assert,atomicJson,endpoint,hash,jsonRequest,until} from '../acceptance/common.mjs';
const root=mkdtempSync(join(tmpdir(),'hmesh-guardian-')),binary=resolve('target/debug',process.platform==='win32'?'hmesh.exe':'hmesh'),fixture=resolve('target/debug',process.platform==='win32'?'hmesh-fixture.exe':'hmesh-fixture');
const daemons=[];let guard;
async function port(){const s=net.createServer();await new Promise(r=>s.listen(0,'127.0.0.1',r));const p=s.address().port;await new Promise(r=>s.close(r));return p;}
async function node(name){const dir=join(root,name),project=join(root,name+'-project');mkdirSync(project);const init=spawnSync(binary,['--json','--data-dir',dir,'init','--port','0'],{encoding:'utf8'});assert(init.status===0,'init failed');const mesh=new Mesh(binary,dir),child=spawn(binary,['--data-dir',dir,'daemon','start'],{stdio:'ignore'});daemons.push({mesh,child});const card=await until(()=>{try{mesh.call(['daemon','status']);return mesh.call(['node','show']);}catch{return null;}});return{mesh,card,dir,project};}
try{
 const a=await node('origin'),b=await node('worker');const pair=a.mesh.call(['pair','request','--peer',b.card.endpoint,'--expect-fingerprint',b.card.fingerprint]);b.mesh.call(['pair','approve',pair.request_id,'--expect-fingerprint',a.card.fingerprint]);a.mesh.call(['pair','approve',pair.request_id,'--expect-fingerprint',b.card.fingerprint]);
 const toml=join(root,'agent.toml'),q=JSON.stringify;writeFileSync(toml,`[agent]\nname="fixture"\nharness="pi"\nproject_id="fixture"\nmode="managed"\n[project]\nroot=${q(b.project)}\nallowed_tools=["read","write","edit"]\nartifact_paths=[]\nrequire_remote_task_approval=true\n[adapter]\nexecutable=${q(fixture)}\nprovider="fixture"\nmodel="fixture"\nenv_allowlist=[]\n`);const agent=b.mesh.call(['agent','register','--config',toml]);
 const run=randomUUID(),manifest={schema:1,run_id:run,mac_node_id:a.card.node_id,windows_node_id:b.card.node_id,agent_id:agent.agent_id,project_id:'fixture',service_id:randomUUID(),crash_id:randomUUID(),lease_seconds:3600,jobs:['recovery','mcp'].map(name=>({id:randomUUID(),name,goal:'fixture:wait',goal_sha256:hash('fixture:wait'),artifact:`.acceptance/${run}/${name}-child.json`,expected:{nonce:'fixture'}}))};
 const mf=join(root,'manifest.json'),cfg=join(root,'owner.json');atomicJson(mf,manifest);atomicJson(cfg,{binary,node_dir:b.dir,project_root:b.project,project_id:'fixture',manifest:mf,state_dir:join(root,'guardian'),port:await port()});
 guard=spawn(process.execPath,[resolve('scripts/reliability/supervisor.mjs'),'--config',cfg],{stdio:'ignore'});
 await until(()=>b.mesh.call(['service','list']).find(s=>s.id===manifest.service_id&&s.status.http_health==='healthy'));
 const cid=randomUUID(),cf=join(root,'conn.json');a.mesh.config(['service','connect'],{id:cid,peer_id:b.card.node_id,service_id:manifest.service_id,local_port:0,lease_seconds:600},cf);const base=await endpoint(a.mesh,cid);
 const ack=await jsonRequest(base,'/crash-probe',{method:'POST',body:{run_id:run,action_id:manifest.crash_id}});assert(ack.status===202,'Crash probe not reserved');
 const end=await until(async()=>{try{const r=await jsonRequest(base,'/status',{timeout:5000});return r.status===200&&r.json.crash?.state==='blocked'?r.json:null;}catch{return null;}},30);
 assert(end.restarts===1&&end.crash.mutation_count===1,'Unexpected replay or restart count');
 const replay=await jsonRequest(base,'/crash-probe',{method:'POST',body:{run_id:run,action_id:manifest.crash_id}});assert(replay.json.state==='blocked'&&replay.json.mutation_count===1,'Mutation replayed');
 await jsonRequest(base,'/finish',{method:'POST',body:{run_id:run,action_id:manifest.crash_id}});
 await until(()=>!b.mesh.call(['service','list']).find(s=>s.id===manifest.service_id)?.enabled,15);
 console.log(JSON.stringify({passed:true,scope:'single-machine real mTLS guardian exit and owner-supervised restart',model_calls:0,restarts:end.restarts,mutation_count:end.crash.mutation_count,post_crash_state:end.crash.state}));
}catch(e){console.error(e.message+' diagnostics='+root);process.exitCode=1;}
finally{if(guard&&guard.exitCode===null)guard.kill();for(const {mesh,child}of daemons.reverse()){try{mesh.call(['daemon','stop']);}catch{}if(child.exitCode===null)await new Promise(r=>{const t=setTimeout(()=>{child.kill();r();},5000);child.once('exit',()=>{clearTimeout(t);r();});});}}
