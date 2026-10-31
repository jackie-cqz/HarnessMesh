// Inject one crash into a newly published, per-run backend copy only.
import {copyFileSync,mkdirSync} from 'node:fs';
import {randomUUID} from 'node:crypto';
import {join,resolve} from 'node:path';
import {Mesh,atomicJson,assert,fileHash,until} from '../acceptance/common.mjs';
import {TodoProbe} from '../acceptance/checks.mjs';
export async function backendCrash(config){
 const mesh=new Mesh(config.binary,config.node_dir),run=randomUUID(),dir=resolve(config.output_dir,run),app=join(config.project_root,'.acceptance',run);
 mkdirSync(dir,{recursive:true,mode:0o700});mkdirSync(app,{recursive:true,mode:0o700});const originalHash=fileHash(config.backend_file),file=join(app,'server.mjs');copyFileSync(config.backend_file,file);
 const id=randomUUID(),probe=new TodoProbe(`http://127.0.0.1:${config.port}`,{status:'ok',service:'tailnet-todo',node:'mac'},run),report={schema:1,run_id:run,scope:'Real Mac managed backend process crash and subsequent local HTTP client recovery; no model call or TUN change',model_calls:0};
 try{
  mesh.config(['service','publish'],{id,project_id:config.project_id,name:`crash-${run.slice(0,8)}`,target_port:config.port,allowed_peer_ids:[],allowed_restart_peer_ids:[],lease_seconds:600,health_path:'/health',request_timeout_seconds:10,process:{executable:process.execPath,cwd:config.project_root,args:[file],env:{PORT:String(config.port)}}},join(dir,'service.json'));
  const before=await until(()=>{const s=mesh.call(['service','list']).find(s=>s.id===id);return s?.status.process==='running'&&s.status.http_health==='healthy'?s:null;});
  const todo=await probe.seed();await probe.verify(todo);
  const fresh=mesh.call(['service','list']).find(s=>s.id===id);assert(fresh.status.pid===before.status.pid&&fresh.enabled&&fileHash(file)===originalHash,'Owned service identity changed before crash');
  process.kill(before.status.pid,'SIGKILL');
  const after=await until(()=>{const s=mesh.call(['service','list']).find(s=>s.id===id);return s?.status.pid&&s.status.pid!==before.status.pid&&s.status.process==='running'&&s.status.http_health==='healthy'?s:null;},25,'Bounded process supervisor did not recover');
  assert(after.status.starts===before.status.starts+1,'Unexpected restart count');await probe.verify(todo);await probe.remove(todo);
  report.before={pid:before.status.pid,starts:before.status.starts,revision:before.revision};report.after={pid:after.status.pid,starts:after.status.starts,revision:after.revision};report.persistence_passed=true;report.original_unchanged=fileHash(config.backend_file)===originalHash;report.status='passed';
 }catch(e){report.status='failed';report.error=e.message;}
 finally{try{mesh.call(['service','revoke',id]);const row=await until(()=>{const s=mesh.call(['service','list']).find(s=>s.id===id);return s&&!s.enabled&&s.status.process==='stopped'?s:null;});report.cleanup={disabled:!row.enabled,process_stopped:row.status.process==='stopped'};}catch(e){report.cleanup={unresolved:e.message};report.status='blocked';}atomicJson(join(dir,'report.json'),report);}
 return {report,path:join(dir,'report.json')};
}
