// Register independent agents first, then exercise every stored connection against
// loopback. A model listing alone cannot prove that an agent speaks a protocol.
import assert from 'node:assert/strict';
import {execFile, spawn} from 'node:child_process';
import {randomUUID} from 'node:crypto';
import {once} from 'node:events';
import {mkdir, mkdtemp, rm, writeFile} from 'node:fs/promises';
import {createServer} from 'node:http';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {promisify} from 'node:util';
import {until} from './acceptance/common.mjs';
import {agentConfig} from './tailnet-config.mjs';

const protocols = {
  'openai-chat': '/v1/chat/completions',
  'anthropic-messages': '/v1/messages',
  'openai-responses': '/v1/responses',
};
const answer = 'Provider connection verified.';
const run = promisify(execFile);

function reply(res, protocol, model) {
  res.writeHead(200, {'content-type':'text/event-stream'});
  function event(type, data) {
    if (type) res.write('event: '+type+'\n');
    res.write('data: '+JSON.stringify(data)+'\n\n');
  }
  if (protocol === 'openai-chat') {
    const chunk = {id:'chatcmpl-fixture', object:'chat.completion.chunk', created:1, model};
    event(null, {...chunk, choices:[{index:0, delta:{role:'assistant',content:answer}, finish_reason:null}]});
    event(null, {...chunk, choices:[{index:0, delta:{}, finish_reason:'stop'}], usage:{prompt_tokens:12,completion_tokens:5,total_tokens:17}});
    res.end('data: [DONE]\n\n');
  } else if (protocol === 'anthropic-messages') {
    const send = (type, fields) => event(type, {type,...fields});
    send('message_start', {message:{id:'msg_fixture', type:'message', role:'assistant', content:[], model, stop_reason:null,
      stop_sequence:null, usage:{input_tokens:12,output_tokens:0}}});
    send('content_block_start', {index:0, content_block:{type:'text',text:''}});
    send('content_block_delta', {index:0, delta:{type:'text_delta',text:answer}});
    send('content_block_stop', {index:0});
    send('message_delta', {delta:{stop_reason:'end_turn',stop_sequence:null}, usage:{output_tokens:5}});
    send('message_stop', {});
    res.end();
  } else {
    let sequence = 0;
    const send = (type, fields) => event(type, {type,sequence_number:sequence++,...fields});
    const response = {id:'resp_fixture', object:'response', created_at:1, model, status:'in_progress', output:[], error:null};
    const item = {id:'msg_fixture', type:'message', role:'assistant', status:'in_progress', content:[]};
    const index = {output_index:0, item_id:item.id, content_index:0};
    const part = {type:'output_text',text:'',annotations:[],logprobs:[]};
    send('response.created', {response});
    send('response.output_item.added', {output_index:0,item});
    send('response.content_part.added', {...index,part});
    send('response.output_text.delta', {...index,delta:answer,logprobs:[]});
    send('response.output_text.done', {...index,text:answer,logprobs:[]});
    part.text=answer;
    send('response.content_part.done', {...index,part});
    item.status='completed';item.content=[part];
    send('response.output_item.done', {output_index:0,item});
    send('response.completed', {response:{...response,status:'completed',output:[item],usage:{input_tokens:12,output_tokens:5,total_tokens:17}}});
    res.end();
  }
}

const harnesses = process.argv.length>2 ? process.argv.slice(2) : ['opencode','pi','dsh'];
assert(harnesses.every(h=>['opencode','pi','dsh'].includes(h)), 'Choose opencode, pi or dsh');
const root = await mkdtemp(join(tmpdir(),'hmesh-provider-protocols-'));
const project = join(root,'project'), nodeDir = join(root,'node');
const env = {PATH:process.env.PATH, HOME:root, USERPROFILE:root, TMPDIR:tmpdir(),
  XDG_CONFIG_HOME:join(root,'config'), XDG_CACHE_HOME:join(root,'cache'), XDG_DATA_HOME:join(root,'data'), XDG_STATE_HOME:join(root,'state'),
  DSH_HOME:join(root,'dsh'), DSH_TELEMETRY_DISABLED:'1', PI_OFFLINE:'1', OPENCODE_DISABLE_AUTOUPDATE:'true', OPENCODE_DISABLE_MODELS_FETCH:'true'};
for (const name of ['SystemRoot','ComSpec','PATHEXT','TEMP','TMP']) if (process.env[name]) env[name]=process.env[name];
const binary=resolve('target/debug', process.platform==='win32'?'hmesh.exe':'hmesh');
const flags=['--json','--data-dir',nodeDir];
async function cli(args, input='') {
  const result=run(binary,[...flags,...args],{env,cwd:project,timeout:60000,windowsHide:true});
  result.child.stdin.end(input);
  return JSON.parse((await result).stdout);
}

const routes = new Map();
const server = createServer(async(req,res)=>{
  try {
    const [,name,...segments]=new URL(req.url,'http://localhost').pathname.split('/');
    const route=routes.get(name);
    if (!route) {res.writeHead(404).end();return;}
    const path='/'+segments.join('/');
    // Keep only the authentication verdict. Failure output never contains a key.
    const validKey=route.protocol==='anthropic-messages'
      ? req.headers['x-api-key']===route.key
      : req.headers.authorization==='Bearer '+route.key;
    if (!validKey) {route.problem='wrong authentication header';res.writeHead(401).end();return;}
    if (req.method==='GET' && path==='/v1/models') {
      route.listed=true;
      res.writeHead(200,{'content-type':'application/json'}).end(JSON.stringify({data:[{id:route.model}]}));return;
    }
    if (req.method!=='POST'||path!==protocols[route.protocol]) {
      route.problem='unexpected request '+req.method+' '+path;res.writeHead(404).end();return;
    }
    const parts=[];for await (const chunk of req) parts.push(chunk);
    const body=JSON.parse(Buffer.concat(parts));
    if (body.model!==route.model || !(route.protocol==='openai-responses'?body.input:body.messages)) {
      route.problem='unexpected model or request body';res.writeHead(400).end();return;
    }
    route.prompts++;reply(res,route.protocol,route.model);
  } catch {res.destroy();}
});

let daemon;
try {
  await mkdir(project);server.listen(0,'127.0.0.1');await once(server,'listening');
  const base='http://127.0.0.1:'+server.address().port;
  await cli(['init','--name','provider-test','--port','0']);
  daemon=spawn(binary,[...flags,'daemon','start'],{env,cwd:project,stdio:'ignore',windowsHide:true});
  await until(()=>cli(['daemon','status']).catch(()=>false));
  const installed=(await cli(['agent','detect'])).installed, catalog=await cli(['agent','catalog']);
  for (const harness of harnesses) {
    const agent=installed.find(a=>a.harness===harness);assert(agent,harness+' must be installed on PATH');
    for (const [index,protocol] of Object.keys(protocols).entries()) {
      const name=harness+'-'+index, model='fixture-'+index, key=randomUUID();
      const cfg=join(root,name+'.toml');
      await writeFile(cfg,agentConfig({node:name,harness,executable:agent.executable,project,projectId:'provider-test',catalog}));
      const card=await cli(['agent','register','--config',cfg]);
      assert.equal(card.probe.protocol_probe,'acp_initialize_passed');
      const route={agent:card.agent_id,harness,protocol,model,key,prompts:0,listed:false,problem:null};
      routes.set(name,route);
      const url=base+'/'+name+(protocol==='anthropic-messages'?'':'/v1');
      const view=await cli(['agent','service',card.agent_id,'--mode','custom','--base-url',url,
        '--protocol',protocol,'--default-model',model,'--api-key-stdin'],key+'\n');
      assert.equal(view.protocol,protocol);
      assert(!JSON.stringify(view).includes(key),'agent service returned a full key');
    }
  }
  for (const [name,route] of routes) {
    await cli(['agent','service',route.agent,'--test']);
    assert(route.listed,name+' model list: '+(route.problem||'no request'));
    const {session_id}=await cli(['session','open','--agent',route.agent]);
    await cli(['session','send',session_id,'--message','Reply in one short sentence.']);
    await until(async()=>{
      assert(!route.problem,name+': '+route.problem);
      const s=await cli(['session','show',session_id]);
      if(s.last_error)throw Error(name+' session failed: '+s.last_error.replaceAll(route.key,'[redacted]'));
      if(s.status!=='idle'||s.turns!==1)return false;
      assert.equal(s.last_final_text,answer,name+' final answer');
      return true;
    },60,name+' did not finish');
    assert(route.prompts>0,name+' did not send inference to its own endpoint');
    await cli(['session','close',session_id]);
    console.log(JSON.stringify({harness:route.harness,protocol:route.protocol,listed:true,authenticated:true,receivedReply:true}));
  }
} finally {
  if(daemon&&daemon.exitCode===null&&daemon.signalCode===null){
    const stopped=once(daemon,'close');
    try{await cli(['daemon','stop']);}catch{daemon.kill('SIGTERM');}
    const force=setTimeout(()=>daemon.kill('SIGKILL'),5000);
    try{await stopped;}finally{clearTimeout(force);}
  }
  server.closeAllConnections();
  await new Promise(resolve=>server.close(resolve));
  await rm(root,{recursive:true,force:true});
}
