// Windows owner-side exact-task approver and crash-uncertainty probe. No remote commands.
import http from 'node:http';
import { existsSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { acquireLock, assert, atomicJson, fileHash, flags, hash, inside, keys, localFile, Mesh, readJson, UUID } from '../acceptance/common.mjs';
export async function guardian(configFile) {
  const config=readJson(localFile(configFile));keys(config,['binary','node_dir','project_root','project_id','manifest','state_dir','port']);
  const manifest=readJson(localFile(config.manifest));keys(manifest,['schema','run_id','mac_node_id','windows_node_id','agent_id','project_id','service_id','crash_id','lease_seconds','jobs']);
  assert(manifest.schema===1 && manifest.jobs?.length===2 && manifest.lease_seconds===3600,'Unsupported bounded manifest');
  for(const k of ['run_id','mac_node_id','windows_node_id','agent_id','service_id','crash_id'])assert(UUID.test(manifest[k]),'Invalid manifest identity');
  const project=realpathSync(config.project_root),stateDir=resolve(config.state_dir),release=acquireLock(join(stateDir,'guardian.lock'));
  const mesh=new Mesh(config.binary,config.node_dir),binding=fileHash(config.manifest),file=join(stateDir,'state.json');
  let state=existsSync(file)?readJson(file):{binding,started_at:Date.now(),jobs:{},crash:{state:'not_started'}};
  assert(state.binding===binding,'Different manifest for existing state');
  assert(mesh.call(['node','show']).node_id===manifest.windows_node_id && config.project_id===manifest.project_id,'Wrong Windows node/project');
  assert(mesh.call(['agents','list']).some(a=>a.agent_id===manifest.agent_id&&a.project_ids.includes(manifest.project_id)),'Wrong Windows Agent');
  for(const j of manifest.jobs){keys(j,['id','name','goal','goal_sha256','artifact','expected']);assert(UUID.test(j.id)&&hash(j.goal)===j.goal_sha256&&['recovery','mcp'].includes(j.name),'Invalid exact task');assert(j.artifact===`.acceptance/${manifest.run_id}/${j.name}-child.json`,'Invalid scoped artifact');}
  const save=()=>atomicJson(file,state);
  if(state.crash.state==='running'){state.crash.state='blocked';state.crash.error='Worker exited after mutation; automatic replay disabled';state.restarts=(state.restarts||0)+1;}
  save(); let timer,poll,closed=false;
  const expires=state.started_at+3600000;assert(Date.now()<expires,'Run expired');
  function checkJobs(){for(const j of manifest.jobs){
    const record=state.jobs[j.id]??={state:'waiting'};
    if(['passed','failed','blocked'].includes(record.state))continue;
    const tasks=mesh.call(['task','list']).executions;
    const task=tasks.find(t=>t.task_id===j.id);if(!task)continue;
    if(task.origin_node_id!==manifest.mac_node_id||task.agent_id!==manifest.agent_id||task.project_id!==manifest.project_id||hash(task.request.goal)!==j.goal_sha256||task.request.limits.timeout_seconds>600||task.request.limits.max_output_bytes>16777216){record.state='failed';record.error='Exact task binding mismatch; not approved';save();continue;}
    if(task.status==='PENDING_APPROVAL'){
      if(record.approval_reserved){record.state='blocked';record.error='Previous approval outcome unconfirmed';save();continue;}
      record.approval_reserved=true;save();const approved=mesh.call(['task','approve',j.id]);record.approved=true;record.status=approved.status;save();
    }
    if(task.status==='RUNNING'){record.state='running';record.attempt_id=task.last_attempt_id;record.observed_running_at??=new Date().toISOString();save();}
    if(task.status==='SUCCEEDED'){
      const artifact=inside(project,join(project,j.artifact));const bytes=readFileSync(artifact);assert(bytes.length<=65536,'Artifact too large');
      const value=JSON.parse(bytes);assert(Object.keys(value).length===Object.keys(j.expected).length&&Object.entries(j.expected).every(([k,v])=>value[k]===v),'Child JSON exact match failed');
      record.state='passed';record.task_id=task.task_id;record.attempt_id=task.last_attempt_id;record.artifact_sha256=hash(bytes);record.completed_at=new Date().toISOString();save();
    }else if(['FAILED','CANCELLED','UNKNOWN','REJECTED','EXPIRED'].includes(task.status)){record.state='blocked';record.status=task.status;save();}
  }}
  async function close(){if(closed)return;closed=true;clearInterval(poll);clearTimeout(timer);try{mesh.call(['service','revoke',manifest.service_id]);}catch{state.cleanup_unresolved=true;}save();server.close();server.closeAllConnections();release();}
  const server=http.createServer(async(req,res)=>{
    const send=(status,value)=>{res.writeHead(status,{'content-type':'application/json'});res.end(JSON.stringify(value));};
    try{
      assert(req.headers.host===`127.0.0.1:${config.port}`&&!req.headers.origin,'Unsupported caller');
      if(req.method==='GET'&&req.url==='/health')return send(200,{status:'ok'});
      if(req.method==='GET'&&req.url==='/status')return send(200,{run_id:manifest.run_id,binding,platform:process.platform,jobs:state.jobs,crash:state.crash,restarts:state.restarts||0});
      assert(req.method==='POST'&&req.headers['content-type']?.split(';')[0]==='application/json','Unsupported fixed operation');
      let body='';for await(const chunk of req){body+=chunk;assert(body.length<=2048,'Too large');}const b=JSON.parse(body);
      keys(b,['run_id','action_id']);assert(b.run_id===manifest.run_id&&b.action_id===manifest.crash_id,'Wrong action binding');
      if(req.url==='/crash-probe'){
        if(state.crash.state!=='not_started')return send(200,state.crash);
        state.crash={state:'running',action_id:manifest.crash_id};save();
        const mutation=join(stateDir,'probe-mutation.json');assert(!existsSync(mutation),'Existing mutation; refusing replay');
        atomicJson(mutation,{run_id:manifest.run_id,count:1});state.crash.mutation_count=1;save();send(202,state.crash);
        setTimeout(()=>process.exit(42),500);return;
      }
      if(req.url==='/finish'){send(200,{accepted:true,run_id:manifest.run_id});setTimeout(()=>void close(),3000);return;}
      send(404,{error:'No such operation'});
    }catch(e){send(409,{error:e.message});}
  });
  await new Promise((ok,no)=>{server.once('error',no);server.listen(config.port,'127.0.0.1',ok);});
  try{
    const existing=mesh.call(['service','list']).find(r=>r.id===manifest.service_id);
    if(existing)assert(existing.enabled,'Service inactive; start a new manifest');
    else mesh.config(['service','publish'],{id:manifest.service_id,project_id:manifest.project_id,name:`guardian-${manifest.run_id.slice(0,8)}`,target_port:config.port,allowed_peer_ids:[manifest.mac_node_id],allowed_restart_peer_ids:[],lease_seconds:Math.ceil((expires-Date.now())/1000),health_path:'/health',request_timeout_seconds:10,process:null},join(stateDir,'service.json'));
    poll=setInterval(()=>{try{checkJobs();}catch(e){state.error=e.message;save();}},500);
    timer=setTimeout(()=>void close(),expires-Date.now());
    console.log(JSON.stringify({ready:true,run_id:manifest.run_id,service_id:manifest.service_id}));
    for(const sig of ['SIGINT','SIGTERM'])process.once(sig,()=>void close());
    return {close,server};
  }catch(e){await close();throw e;}
}
if(process.argv[1]&&import.meta.url===pathToFileURL(resolve(process.argv[1])).href){try{const f=flags(process.argv.slice(2),['--config']);await guardian(f['--config']);}catch(e){console.error(e.message);process.exitCode=1;}}
