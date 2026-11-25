// Read-only inbox behavior; no browser, Node daemon, personal config or model.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';
import {test} from 'node:test';

const source=readFileSync(new URL('../web/panel/inbox.js',import.meta.url),'utf8');
const plain=value=>JSON.parse(JSON.stringify(value));
function context(tasks=[]){
 const ctx=vm.createContext({Date,Map,Set,Promise,document:{querySelector:()=>null},access:'isolated-test-access',state:{snapshot:{observed_at:100,tasks:{data:{}}},details:new Map(),runtime:{capabilities:{sessions:false}},view:'inbox'},allTasks:()=>tasks,acceptance:task=>task._acceptance||{state:task.request?.expected_artifacts?.length?'pending':'not_required'},unwrap:reply=>{if(reply?.error)throw Object.assign(Error(reply.error.code),reply.error);return reply?.data;},data:(key,fallback)=>key==='node'?{node:{node_id:'local'}}:fallback||[],api:async()=>{throw Error('Unexpected request');}});
 vm.runInContext(source,ctx);
 return {ctx,state:vm.runInContext('inboxState',ctx),model:input=>plain(ctx.buildInboxItems(input))};
}
const task=(id,status='RUNNING',extra={})=>({task_id:id,target_node_id:'remote',status,updated_at:10,_direction:'outbound',_observed:50,request:{goal:'fixture only',expected_artifacts:['out/health.json']},...extra});
const session=(id,status,extra={})=>({session_id:id,node_id:'remote',status,updated_at:9999,agent_name:'fixture',approver:'origin',origin:{node_id:'local'},_remote:true,...extra});

test('only local task/session approvals and answerable permissions count as owner work',()=>{
 const {model}=context();
 const rows=model({localNodeId:'local',tasks:[task('outbound','PENDING_APPROVAL'),task('incoming','PENDING_APPROVAL',{_direction:'local'})],sessions:[session('local-approval','pending_approval',{_remote:false}),session('origin-permission','waiting_permission'),session('local-permission','waiting_permission',{approver:'local'}),session('auto','waiting_permission',{approver:'auto'})]});
 const owner=rows.filter(row=>row.needsOwner).map(row=>row.id).sort();
 assert.deepEqual(owner,['incoming','local-approval','origin-permission']);
 assert.equal(rows.find(row=>row.id==='outbound').category,'execution');
 assert.equal(rows.find(row=>row.id==='local-permission').category,'execution');
});

test('execution success, pending acceptance and failed acceptance remain different facts',()=>{
 const {model}=context();const rows=model({tasks:[task('pending','SUCCEEDED',{_acceptance:{state:'pending'}}),task('passed','SUCCEEDED',{_acceptance:{state:'passed'}}),task('assertion','SUCCEEDED',{_acceptance:{state:'failed'}}),task('execution','FAILED')]});
 assert.deepEqual(rows.map(row=>[row.id,row.status,row.acceptance,row.category]).sort(),[['assertion','SUCCEEDED','failed','attention'],['execution','FAILED',null,'attention'],['passed','SUCCEEDED','passed','recent'],['pending','SUCCEEDED','pending','acceptance']]);
});

test('remote list business timestamps are never represented as last-confirmed observations',()=>{
 const {model}=context();const rows=model({localNodeId:'local',sessionObserved:123,tasks:[task('unknown','RUNNING',{_observed:null,updated_at:99999})],sessions:[session('remote','waiting_permission'),session('local','idle',{_remote:false})]});
 assert.equal(rows.find(row=>row.id==='unknown').observedAt,null);
 assert.equal(rows.find(row=>row.id==='remote').observedAt,null);
 assert.equal(rows.find(row=>row.id==='local').observedAt,123);
});

test('an unconfirmed original request stays an attention item with the same stable identity',()=>{
 const {model}=context();const [row]=model({tasks:[task('fixed','UNCONFIRMED',{_request_id:'fixed',_observed:null})]});
 assert.equal(row.id,'fixed');assert.equal(row.requestId,'fixed');assert.equal(row.category,'attention');assert.equal(row.observedAt,null);
});

test('observer failure preserves the last confirmed record/time; reconnect updates it without a write',async()=>{
 const original=task('fixed');const {ctx,state}=context([original]);let mode='first';let writes=0;
 ctx.api=async(path,options)=>{if(options?.method&&options.method!=='GET')writes++;assert.equal(path,'/tasks/fixed/observation');if(mode==='offline')throw Object.assign(Error('offline'),{code:'PEER_UNREACHABLE'});return {data:{task:{...original,status:mode==='first'?'WAITING_INPUT':'SUCCEEDED'},observed_at:mode==='first'?200:300}};};
 await ctx.refreshInbox(true);let row=ctx.inboxModel()[0];assert.equal(row.status,'WAITING_INPUT');assert.equal(row.observedAt,200);
 mode='offline';await ctx.refreshInbox(true);row=ctx.inboxModel()[0];assert.equal(row.status,'WAITING_INPUT');assert.equal(row.observedAt,200);assert.equal(state.failures.get('task:fixed'),'PEER_UNREACHABLE');
 mode='recovered';await ctx.refreshInbox(true);row=ctx.inboxModel()[0];assert.equal(row.status,'SUCCEEDED');assert.equal(row.observedAt,300);assert.equal(state.failures.has('task:fixed'),false);assert.equal(writes,0);
});

test('passive observation is limited to two concurrent GETs and rotates beyond the first eight',async()=>{
 const tasks=Array.from({length:12},(_,i)=>task('task-'+i));const {ctx}=context(tasks);let active=0,peak=0;const seen=[];
 ctx.api=async(path,options)=>{assert.equal(options,undefined);active++;peak=Math.max(peak,active);const id=path.split('/')[2];seen.push(id);await new Promise(resolve=>setTimeout(resolve,2));active--;return {data:{task:tasks.find(task=>task.task_id===id),observed_at:200}};};
 await ctx.refreshInbox(true);assert.equal(seen.length,8);assert.ok(peak<=2);
 await ctx.refreshInbox(true);assert.equal(seen.length,16);assert.equal(new Set(seen).size,12);assert.ok(peak<=2);
});

test('remote ACP observation requests status-only and never fetches permission/message events',async()=>{
 const remote=session('remote-session','waiting_permission');const {ctx,state}=context();ctx.state.runtime.capabilities.sessions=true;const paths=[];
 ctx.api=async(path,options)=>{assert.equal(options,undefined);paths.push(path);if(path==='/sessions/summary')return {observed_at:100,sessions:{data:{sessions:[],remote:[{peer_id:'remote',session:remote}]}}};assert.equal(path,'/sessions/remote-session/events?after=0&wait=0&status_only=true');return {data:{session:{...remote,status:'idle'},events:[],next:0}};};
 await ctx.refreshInbox(true);assert.deepEqual(paths,['/sessions/summary','/sessions/remote-session/events?after=0&wait=0&status_only=true']);const row=ctx.inboxModel()[0];assert.equal(row.status,'idle');assert.ok(row.observedAt>0);
 ctx.api=async()=>{throw Object.assign(Error('offline'),{code:'PEER_UNREACHABLE'});};await ctx.refreshInbox(true);assert.equal(ctx.inboxModel()[0].status,'idle');assert.equal(state.sessionError,'PEER_UNREACHABLE');
});

test('whole-overview failure retains old observations and is tracked separately',()=>{
 const {ctx,state}=context([task('fixed')]);ctx.inboxModel();ctx.noteInboxOverviewError({code:'LOCAL_UNREACHABLE'});const row=ctx.inboxModel()[0];assert.equal(row.status,'RUNNING');assert.equal(row.observedAt,50);assert.equal(state.overviewError,'LOCAL_UNREACHABLE');ctx.noteInboxOverviewError(null);assert.equal(state.overviewError,null);
});

test('an older observed record cannot roll back a newer list status',()=>{
 const current=task('fixed','SUCCEEDED',{updated_at:20});const {ctx,state}=context([current]);
 state.taskObservations.set('fixed',{record:task('fixed','RUNNING',{updated_at:10}),observedAt:1000});
 state.sessions={sessions:[],remote:[{peer_id:'remote',session:session('session','closed',{updated_at:20})}]};
 state.sessionObservations.set('session',{record:session('session','running',{updated_at:10}),observedAt:1000});
 const rows=ctx.inboxModel();assert.equal(rows.find(row=>row.id==='fixed').status,'SUCCEEDED');assert.equal(rows.find(row=>row.id==='session').status,'closed');assert.equal(rows.find(row=>row.id==='session').observedAt,null);
});

test('overview and interval hooks share one passive polling window',async()=>{
 const rows=Array.from({length:10},(_,i)=>task('fixed-'+i));const {ctx}=context(rows);let calls=0;
 ctx.api=async path=>{calls++;const id=path.split('/')[2];return {data:{task:rows.find(row=>row.task_id===id),observed_at:100}};};
 await ctx.refreshInbox();await ctx.refreshInbox();assert.equal(calls,8);
 await ctx.refreshInbox(true);assert.equal(calls,16);
});

test('a restarted owner process invalidates in-memory session observations',()=>{
 const {ctx,state}=context();ctx.state.runtime.daemon={running:true,pid:10};ctx.inboxModel();
 state.sessions={sessions:[session('fixed','idle',{_remote:false})],remote:[]};state.sessionsObserved=100;
 state.sessionObservations.set('fixed',{record:session('fixed','running',{updated_at:9999}),observedAt:200});
 ctx.state.runtime.daemon.pid=11;const row=ctx.inboxModel()[0];assert.equal(row.status,'idle');assert.equal(row.observedAt,null);assert.equal(state.sessionObservations.size,0);assert.equal(state.sessionError,'NODE_RESTARTED');
});

test('an unreadable verification preserves confirmed execution success with a separate read problem',async()=>{
 const original=task('fixed','SUCCEEDED');const {ctx,state}=context([original]);
 ctx.api=async()=>({data:{task:original,observed_at:200,verification:{error:{code:'PEER_UNREACHABLE'}}}});
 await ctx.refreshInbox(true);assert.equal(ctx.inboxModel()[0].status,'SUCCEEDED');assert.equal(ctx.inboxModel()[0].observedAt,200);assert.equal(state.failures.get('task:fixed'),'PEER_UNREACHABLE');
});

test('a response already in flight before a daemon restart cannot repopulate old observations',async()=>{
 const {ctx,state}=context([task('fixed')]);ctx.state.runtime.daemon={running:true,pid:10};let finish;
 ctx.api=()=>new Promise(resolve=>{finish=resolve;});const pending=ctx.refreshInbox(true);
 ctx.state.runtime.daemon.pid=11;ctx.inboxModel();finish({data:{task:task('fixed','WAITING_INPUT'),observed_at:200}});
 await pending;assert.equal(state.taskObservations.size,0);assert.equal(ctx.inboxModel()[0].status,'RUNNING');
});
