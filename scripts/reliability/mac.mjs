// Real-device owner controller. Static manifest tasks, one origin restart, one guardian exit.
import {spawn,spawnSync} from 'node:child_process';
import {existsSync,mkdirSync,readFileSync,writeFileSync} from 'node:fs';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {acquireLock,assert,atomicJson,Blocked,endpoint,fileHash,flags,hash,Journal,jsonRequest,Mesh,readJson,until} from '../acceptance/common.mjs';
export async function runReliability(configFile){
 const config=readJson(configFile),dir=resolve(config.run_dir),manifest=readJson(join(dir,'manifest.json')),mesh=new Mesh(config.binary,config.node_dir);
 const release=acquireLock(join(dir,'mac.lock'));const journal=new Journal(join(dir,'mac-state.json'),fileHash(join(dir,'manifest.json')));
 const report={schema:1,scope:'Real Windows/Mac origin restart, guardian crash uncertainty and native Pi MCP delegation',run_id:manifest.run_id,stages:{},started_at:new Date().toISOString(),limitations:['No Tailscale/TUN outage injected','Observed Unix descendant cleanup is not OS containment','Windows fixed-task approver is owner-started, not a policy change']};
 let base;const recovery=manifest.jobs.find(j=>j.name==='recovery'),mcp=manifest.jobs.find(j=>j.name==='mcp');
 const step=async(name,fn)=>{const value=await journal.once(name,fn);report.stages[name]={passed:true,result:value};atomicJson(join(dir,'report.json'),report);return value;};
 async function connect(){const id=(await import('node:crypto')).randomUUID();mesh.config(['service','connect'],{id,peer_id:manifest.windows_node_id,service_id:manifest.service_id,local_port:0,lease_seconds:3600},join(dir,`connection-${id}.json`));journal.data.connections??=[];journal.data.connections.push(id);journal.save();return endpoint(mesh,id);}
 async function status(){const r=await jsonRequest(base,'/status',{timeout:15000});assert(r.status===200&&r.json.run_id===manifest.run_id&&r.json.binding===fileHash(join(dir,'manifest.json')),'Guardian identity mismatch');return r.json;}
 async function terminal(id,seconds=650){return until(()=>{const t=mesh.call(['task','show',id]);if(t.status==='SUCCEEDED')return t;if(['FAILED','CANCELLED','UNKNOWN','REJECTED','EXPIRED'].includes(t.status))throw new Blocked(`Task ${id} ended ${t.status}`);return null;},seconds);}
 function events(id){const all=[];let after=0;for(let page=0;page<1000;page++){const rows=mesh.call(['task','events',id,'--after',String(after)]);if(!rows.length)return all;assert(rows.every(e=>e.seq>after),'Event cursor did not advance');all.push(...rows);after=rows.at(-1).seq;}throw new Blocked('Too many event pages');}
 try{
  assert(mesh.call(['node','show']).node_id===manifest.mac_node_id,'Wrong origin identity');
  await step('guardian_visible',()=>until(()=>{try{return mesh.call(['service','list','--peer',manifest.windows_node_id]).find(s=>s.id===manifest.service_id&&s.health==='healthy');}catch{return null;}},1200,'Start the Windows guardian kit'));
  base=await step('guardian_connection',connect);
  await step('guardian_identity',status);
  await step('submit_recovery',()=>mesh.call(['task','send',manifest.agent_id,'--peer',manifest.windows_node_id,'--project',manifest.project_id,'--task-id',recovery.id,'--request-id',recovery.id,'--timeout','600','--max-output-bytes','16777216','--prompt',recovery.goal]));
  const running=await step('recovery_running',()=>until(()=>{const t=mesh.call(['task','show',recovery.id]);if(t.status==='RUNNING')return {task_id:t.task_id,attempt_id:t.last_attempt_id};if(t.status==='SUCCEEDED')throw new Blocked('Recovery task finished before restart injection; do not claim running-task coverage');return null;},120));
  const before=mesh.call(['node','show']);
  await step('restart_origin',async()=>{
    const old=mesh.call(['daemon','status']);mesh.call(['daemon','stop']);
    await until(()=>{try{process.kill(old.pid,0);return false;}catch{return true;}},30,'Origin process did not stop');
    const db=join(config.node_dir,'state.sqlite3'),backup=join(dir,'before-schema-upgrade.sqlite3');
    const b=spawnSync('/usr/bin/sqlite3',[db,`.backup '${backup.replaceAll("'","''")}'`],{encoding:'utf8'});assert(b.status===0,'Private database backup failed');
    const daemon=spawn(mesh.binary,['--data-dir',config.node_dir,'daemon','start'],{detached:true,stdio:'ignore'});daemon.unref();
    const now=await until(()=>{try{const t=mesh.call(['daemon','status']);return t.pid===daemon.pid?t:null;}catch{return null;}},40,'Origin restart failed');
    const card=mesh.call(['node','show']);assert(card.node_id===before.node_id&&card.fingerprint===before.fingerprint,'Restart changed identity');
    return {old_pid:old.pid,new_pid:now.pid,node_id:card.node_id,fingerprint_unchanged:true};
  });
  base=await step('guardian_reconnection',connect);
  const recovered=await step('recover_original_request',()=>mesh.call(['task','recover',recovery.id]));
  assert(recovered.task_id===running.task_id&&recovered.last_attempt_id===running.attempt_id,'Recovery created a different task/attempt');
  const done=await step('recovery_complete',()=>terminal(recovery.id));
  assert(done.last_attempt_id===running.attempt_id,'Task attempt changed after restart');
  await step('windows_recovery_readback',()=>until(async()=>{const r=await status();const j=r.jobs[recovery.id];if(j?.state==='failed'||j?.state==='blocked')throw new Blocked('Windows recovery artifact check failed');return j?.state==='passed'?j:null;},30));
  const replay=events(recovery.id);await step('recovery_event_replay',()=>({count:replay.length,last_seq:replay.at(-1)?.seq,attempt_ids:[...new Set(replay.map(e=>e.attempt_id).filter(Boolean))]}));
  await step('guardian_crash_reserved',()=>jsonRequest(base,'/crash-probe',{method:'POST',body:{run_id:manifest.run_id,action_id:manifest.crash_id}}));
  const crashed=await step('guardian_restarted_blocked',()=>until(async()=>{try{const r=await status();return r.crash.state==='blocked'?r:null;}catch{return null;}},40));
  assert(crashed.restarts===1&&crashed.crash.mutation_count===1,'Guardian mutation replayed');
  const duplicate=await step('guardian_duplicate_refused',()=>jsonRequest(base,'/crash-probe',{method:'POST',body:{run_id:manifest.run_id,action_id:manifest.crash_id}}));assert(duplicate.json.state==='blocked'&&duplicate.json.mutation_count===1,'Crash action replayed');
  const parent=await step('native_mcp_parent',()=>{
    assert(existsSync(config.mcp_binary),'Build hmesh-mcp first');
    const project=resolve(config.project_root),scope=join(dir,'mcp-scope.json');mkdirSync(join(project,'.acceptance',manifest.run_id),{recursive:true});
    atomicJson(scope,{targets:[{peer_id:manifest.windows_node_id,agent_id:manifest.agent_id,project_id:manifest.project_id}],max_submissions:1,max_timeout_seconds:600,max_output_bytes:16777216});
    let toml=readFileSync(config.pi_config,'utf8').replace(/^name\s*=.*$/m,`name = "mac-native-mcp-${manifest.run_id.slice(0,8)}"`).replace(/^artifact_paths\s*=.*$/m,`artifact_paths = [".acceptance/${manifest.run_id}/mcp-parent.json"]`).replace(/^mesh_server\s*=.*\n?/mg,'').replace(/^mesh_scope\s*=.*\n?/mg,'');
    assert(/^\[adapter\]\s*$/m.test(toml),'Missing Pi adapter section');
    toml=toml.replace(/^\[adapter\]\s*$/m,`[adapter]\nmesh_server = ${JSON.stringify(resolve(config.mcp_binary))}\nmesh_scope = ${JSON.stringify(scope)}`);const cfg=join(dir,'mcp-agent.toml');writeFileSync(cfg,toml,{mode:0o600});const agent=mesh.call(['agent','register','--config',cfg]);
    journal.data.mcp_agent_id=agent.agent_id;journal.save();
    const submit={peer_id:manifest.windows_node_id,agent_id:manifest.agent_id,project_id:manifest.project_id,request_id:mcp.id,goal:mcp.goal,timeout_seconds:600,max_output_bytes:16777216};
    const expected={child_task_id:mcp.id,status:'SUCCEEDED',nonce:mcp.expected.nonce};
    const prompt=`Use the native mcp__harnessmesh__mesh_list_peers and mesh_list_agents tools to discover the permitted Windows Agent. Then call mesh_submit_task with EXACTLY these argument values, especially unchanged goal and request_id: ${JSON.stringify(submit)}. Do not submit more than once. Independent Windows owner approval will happen automatically for ONLY this exact task. Repeatedly call mesh_get_task with wait_seconds up to 20, advancing its event cursor, until SUCCEEDED. Never use shell or network tools. Only after observing SUCCEEDED, write .acceptance/${manifest.run_id}/mcp-parent.json containing exactly ${JSON.stringify(expected)} and read it back. If the task fails or remains uncertain, report failure instead of inventing success. Keep your response brief.`;
    return mesh.call(['agent','verify',agent.agent_id,'--allow-model','--timeout','600','--max-output-bytes','16777216','--prompt',prompt]);
  });
  await step('native_mcp_parent_complete',()=>terminal(parent.task_id));
  await step('native_mcp_windows_readback',()=>until(async()=>{const r=await status(),j=r.jobs[mcp.id];if(['failed','blocked'].includes(j?.state))throw new Blocked('MCP child check failed');return j?.state==='passed'?j:null;},30));
  await step('native_mcp_proof',()=>{
    const ev=events(parent.task_id),tools=ev.filter(e=>e.payload?.type==='tool_event').map(e=>e.payload.name);
    for(const name of ['mesh_list_peers','mesh_list_agents','mesh_submit_task','mesh_get_task'])assert(tools.some(t=>t.includes(name)),`Missing native MCP tool ${name}`);
    const file=join(config.project_root,'.acceptance',manifest.run_id,'mcp-parent.json'),receipt=readJson(file);assert(receipt.child_task_id===mcp.id&&receipt.status==='SUCCEEDED'&&receipt.nonce===mcp.expected.nonce,'Parent receipt mismatch');
    return {parent_task_id:parent.task_id,child_task_id:mcp.id,parent_receipt_sha256:fileHash(file),tools:[...new Set(tools)]};
  });
  report.status='passed';
 }catch(e){report.status=e instanceof Blocked?'blocked':'failed';report.error=e.message;}
 finally{
  report.cleanup={};if(base){try{const r=await jsonRequest(base,'/finish',{method:'POST',body:{run_id:manifest.run_id,action_id:manifest.crash_id}});report.cleanup.guardian_finish=r.status===200;await until(()=>!mesh.call(['service','list','--peer',manifest.windows_node_id]).some(s=>s.id===manifest.service_id),15,'Guardian cleanup unconfirmed');report.cleanup.guardian_revoked=true;}catch{report.cleanup.guardian_finish=false;}}
  if(report.status!=='passed'){for(const id of [recovery.id,mcp.id,journal.data.steps.native_mcp_parent?.result?.task_id].filter(Boolean)){try{const task=mesh.call(['task','show',id]);if(!['SUCCEEDED','FAILED','CANCELLED','REJECTED','EXPIRED','UNKNOWN'].includes(task.status))mesh.call(['task','cancel',id]);}catch{}}}
  for(const id of journal.data.connections||[]){try{mesh.call(['connection','revoke',id]);}catch{report.cleanup.connections_unresolved=true;}}
  if(report.status==='passed'&&(!report.cleanup.guardian_revoked||report.cleanup.connections_unresolved))report.status='blocked';
  report.completed_at=new Date().toISOString();report.journal=journal.data;atomicJson(join(dir,'report.json'),report);release();
 }
 return report;
}
if(process.argv[1]&&pathToFileURL(resolve(process.argv[1])).href===import.meta.url){try{const f=flags(process.argv.slice(2),['--config']);const r=await runReliability(f['--config']);console.log(JSON.stringify({status:r.status,error:r.error,run_id:r.run_id}));if(r.status!=='passed')process.exitCode=1;}catch(e){console.error(e.message);process.exitCode=1;}}
