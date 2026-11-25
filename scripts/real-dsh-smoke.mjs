// Exercise the installed official DSH CLI through a disposable HarnessMesh node.
// The native provider uses an Anthropic Messages fixture on loopback.
import assert from 'node:assert/strict';
import {execFile, spawn} from 'node:child_process';
import {once} from 'node:events';
import {mkdir, mkdtemp, readFile, rm, writeFile} from 'node:fs/promises';
import {createServer} from 'node:http';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {promisify} from 'node:util';
import {until} from './acceptance/common.mjs';
import {agentConfig} from './tailnet-config.mjs';

const run = promisify(execFile);
const firstMessage = 'Remember this marker: hmesh-dsh-first-turn';
const firstAnswer = 'hmesh-dsh-first-reply';
const secondMessage = 'Check the earlier conversation.';
const secondAnswer = 'hmesh-dsh-history-preserved';
const permissionMessage = 'Ask before creating approved.txt for the test.';
const approvedContents = 'hmesh-dsh-approved';
const managedKey = 'fixture-dsh-managed';
const holdMessage = 'Hold this reply until cancellation.';

export async function makeDshFixture() {
  const root = await mkdtemp(join(tmpdir(), 'hmesh-dsh-'));
  const project = join(root, 'project');
  const calls = [];
  const server = createServer(async (req, res) => {
    if (new URL(req.url, 'http://localhost').pathname !== '/v1/messages' || req.method !== 'POST') {
      res.writeHead(404).end();
      return;
    }
    try {
      const parts = [];
      for await (const part of req) parts.push(part);
      const body = JSON.parse(Buffer.concat(parts));
      const messages = JSON.stringify(body.messages);
      const continued = messages.includes(secondMessage);
      const hasHistory = messages.includes(firstMessage) && messages.includes(firstAnswer);
      const held = messages.includes(holdMessage);
      const permission = messages.includes(permissionMessage) && !held;
      const previousWrites = permission ? calls.filter(c => c.permission).length : 0;
      const managed = req.headers['x-api-key'] === managedKey;
      calls.push({model: body.model, continued, hasHistory, held, permission, managed});
      res.writeHead(200, {'Content-Type':'text/event-stream'});
      const event = (type, fields) => res.write('event: '+type+'\ndata: '+JSON.stringify({type, ...fields})+'\n\n');
      event('message_start', {message: {id:'msg_fixture', type:'message', role:'assistant', model:body.model,
        content:[], usage:{input_tokens:30, output_tokens:0}}});
      if (held) return;
      const tryWrite = permission && previousWrites < 2;
      if (tryWrite) {
        const input = {file_path:'approved.txt', content:approvedContents};
        if (previousWrites === 1) {
          input.sandbox_permissions = 'workspace-write';
          input.justification = 'Allow the fixture to create approved.txt in the temporary project.';
        }
        event('content_block_start', {index:0, content_block:{
          type:'tool_use', id:'fixture_write_'+previousWrites, name:'write', input:{},
        }});
        event('content_block_delta', {index:0, delta:{type:'input_json_delta', partial_json:JSON.stringify(input)}});
      } else {
        let text = firstAnswer;
        if (continued) text = hasHistory ? secondAnswer : 'missing history';
        if (permission) text = approvedContents;
        event('content_block_start', {index:0, content_block:{type:'text', text:''}});
        event('content_block_delta', {index:0, delta:{type:'text_delta', text}});
      }
      event('content_block_stop', {index:0});
      event('message_delta', {delta:{stop_reason:tryWrite ? 'tool_use' : 'end_turn', stop_sequence:null}, usage:{output_tokens:10}});
      event('message_stop', {});
      res.end();
    } catch {
      res.destroy();
    }
  });
  const dispose = async () => {
    const closed = new Promise(resolve => server.close(resolve));
    server.closeAllConnections();
    await closed;
    await rm(root, {recursive:true, force:true});
  };
  try {
    await mkdir(project);
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');
    const env = {
      PATH: process.env.PATH, HOME:root, USERPROFILE:root, DSH_HOME:join(root,'.dsh'),
      TMPDIR:tmpdir(), DSH_TELEMETRY_DISABLED:'1', DSH_PERMISSION_MODE:'read-only',
      DEEPSEEK_BASE_URL:'http://127.0.0.1:'+server.address().port,
      DEEPSEEK_API_KEY:'fixture-only', FIXTURE_DSH_SERVICE_KEY:managedKey,
    };
    for (const name of ['SystemRoot','ComSpec','PATHEXT','TEMP','TMP']) if (process.env[name]) env[name] = process.env[name];
    return {root, project, env, calls, dispose};
  } catch (error) {
    await dispose();
    throw error;
  }
}

async function check() {
  const binary = resolve('target/debug', process.platform === 'win32' ? 'hmesh.exe' : 'hmesh');
  const fixture = await makeDshFixture();
  const flags = ['--json','--data-dir', join(fixture.root, 'node')];
  const options = {env:fixture.env, cwd:fixture.project, timeout:60000, windowsHide:true};
  async function cli(...args) {
    const result = await run(binary, [...flags,...args], options);
    return JSON.parse(result.stdout);
  }
  let daemon;
  async function start() {
    daemon = spawn(binary, [...flags,'daemon','start'], {...options, timeout:undefined, stdio:'ignore'});
    await until(async () => {
      if (daemon.exitCode !== null) throw Error('DSH smoke node exited during startup');
      return cli('daemon','status').catch(() => false);
    });
  }
  async function stop() {
    if (!daemon || daemon.exitCode !== null || daemon.signalCode !== null) return;
    const closed = once(daemon,'close');
    try {
      await cli('daemon','stop');
    } finally {
      const timer = setTimeout(() => daemon.kill('SIGKILL'), 5000);
      try { await closed; } finally { clearTimeout(timer); }
    }
  }
  try {
    await cli('init','--name','dsh-smoke','--port','0');
    await start();
    const installed = (await cli('agent','detect')).installed.find(a => a.harness === 'dsh');
    assert(installed, 'Install the official @deepseek-ai/dsh on PATH and build hmesh first');
    const config = join(fixture.root,'agent.toml');
    await writeFile(config, agentConfig({node:'smoke', harness:'dsh', executable:installed.executable,
      project:fixture.project, projectId:'dsh-smoke'}));
    const agent = await cli('agent','register','--config',config);
    assert.equal(agent.probe.protocol_probe, 'acp_initialize_passed');
    const {session_id:id} = await cli('session','open','--agent',agent.agent_id);
    const menu = await cli('session','commands',id);
    assert.equal(menu.harness,'dsh');
    assert(!menu.commands.some(c => c.name === 'compact'));
    const model = menu.config_options.find(opt => opt.category === 'model');
    const choice = model.choices.find(c => c.value.includes('deepseek-v4-pro'));
    assert(choice && choice.value !== model.current, 'Expected an alternative DSH model from the fixture');
    await cli('session','config',id,model.id,choice.value);
    assert.equal(fixture.calls.length,0,'DSH called inference during configuration');
    async function settle(turn, sessionId = id) {
      return until(async () => {
        const session = await cli('session','show',sessionId);
        assert(!session.last_error, session.last_error);
        return session.status === 'idle' && session.turns === turn && session;
      },60,'DSH turn did not settle');
    }
    await cli('session','send',id,'--message',firstMessage);
    const first = await settle(1);
    assert.equal(first.last_final_text, firstAnswer);
    assert(first.agent_session_id);
    await stop();
    await start();
    await cli('session','send',id,'--message',secondMessage);
    const second = await settle(2);
    assert.equal(second.agent_session_id, first.agent_session_id);
    assert.equal(second.last_final_text, secondAnswer);
    assert(fixture.calls.some(c => c.continued && c.hasHistory));
    assert(fixture.calls.every(c => c.model === 'deepseek-v4-pro'));
    const cursor = (await cli('session','events',id)).next;
    await cli('session','send',id,'--message',permissionMessage);
    const request = await until(async () => {
      const page = await cli('session','events',id,'--after',String(cursor));
      if (page.session.last_error || page.session.turns === 3) throw Error('DSH ended before requesting permission');
      return page.events.find(e => e.kind === 'permission_request');
    },45,'DSH did not request one-time write permission');
    assert.deepEqual(request.payload.options.map(o => o.kind).sort(), ['allow_once','reject_once']);
    const artifact = join(fixture.project,'approved.txt');
    await assert.rejects(readFile(artifact), {code:'ENOENT'});
    await cli('session','permission',id,request.payload.key,'--allow');
    await settle(3);
    assert.equal(await readFile(artifact,'utf8'), approvedContents);
    await cli('session','send',id,'--message',holdMessage);
    await until(() => fixture.calls.some(c => c.held));
    await cli('session','cancel',id);
    const cancelled = await settle(4);
    assert.equal(cancelled.last_stop_reason, 'cancelled');
    await cli('session','close',id);
    const nativeSession = (await cli('session','open','--agent',agent.agent_id)).session_id;
    await cli('session','commands',nativeSession);
    await cli('session','config',nativeSession,model.id,choice.value);
    await cli('session','send',nativeSession,'--message',firstMessage);
    const nativeFirst = await settle(1,nativeSession);
    // A configured task must not pick up the official URL or the key from
    // this DSH user's native profile. Neither native file may be rewritten.
    const vault = join(fixture.env.DSH_HOME,'.credentials.yaml');
    const nativeCredential = 'version: 1\nrefs:\n  HMESH_DSH_API_KEY: fixture-native-conflict\n';
    const nativePatch = join(fixture.env.DSH_HOME,'cordis.patch.yml');
    const nativeRoute = '- id: llm-deepseek\n  config:\n    baseURL: http://127.0.0.1:1\n';
    await writeFile(vault, nativeCredential);
    await writeFile(nativePatch, nativeRoute);
    await cli('agent','service',agent.agent_id,'--mode','custom','--protocol','anthropic-messages',
      '--base-url',fixture.env.DEEPSEEK_BASE_URL,'--default-model','mesh-task-model',
      '--api-key-env','FIXTURE_DSH_SERVICE_KEY');
    const task = await cli('agent','verify',agent.agent_id,'--allow-model');
    await until(async () => {
      const record = await cli('task','show',task.task_id);
      assert(!record.error, record.error?.message);
      return record.status === 'SUCCEEDED';
    },60,'DSH managed task did not finish');
    assert(fixture.calls.some(c => c.model === 'mesh-task-model' && c.managed),
      'The one-shot task did not use its configured model and owner credential');
    await cli('session','send',nativeSession,'--message',secondMessage);
    const continued = await settle(2,nativeSession);
    assert.equal(continued.agent_session_id,nativeFirst.agent_session_id);
    assert.equal(continued.last_final_text,secondAnswer);
    assert(fixture.calls.some(c => c.managed && c.model === 'mesh-task-model' && c.continued && c.hasHistory));
    await cli('agent','service',agent.agent_id,'--mode','custom','--protocol','anthropic-messages',
      '--base-url',fixture.env.DEEPSEEK_BASE_URL,'--default-model','mesh-renamed','--clear-models',
      '--api-key-env','FIXTURE_DSH_SERVICE_KEY');
    await cli('session','send',nativeSession,'--message','check new model');
    const renamed = await settle(3,nativeSession);
    assert.equal(renamed.agent_session_id,nativeFirst.agent_session_id);
    assert(fixture.calls.some(c => c.managed && c.model === 'mesh-renamed' && c.hasHistory));
    const offered = (await cli('session','commands',nativeSession)).config_options.find(o => o.category === 'model');
    assert.deepEqual(offered.choices.map(c => JSON.parse(c.value)[1]), ['mesh-renamed']);
    // Native DSH has no hmesh provider after dropping managed settings.
    // The old conversation must survive a failed resume for a later retry.
    await cli('agent','service',agent.agent_id,'--mode','agent_login','--default-model','','--clear-models');
    const callsBeforeNative = fixture.calls.length;
    await cli('session','send',nativeSession,'--message','use native login');
    const failed = await until(async () => {
      const session = await cli('session','show',nativeSession);
      return session.status === 'idle' && session.last_error && session;
    });
    assert.match(failed.last_error,/restore its custom model service/);
    assert.match(failed.last_error,/open a new session/);
    assert.equal(failed.agent_session_id,nativeFirst.agent_session_id);
    assert.equal(fixture.calls.length,callsBeforeNative);
    await cli('agent','service',agent.agent_id,'--mode','custom','--protocol','anthropic-messages',
      '--base-url',fixture.env.DEEPSEEK_BASE_URL,'--default-model','mesh-renamed',
      '--api-key-env','FIXTURE_DSH_SERVICE_KEY');
    await cli('session','send',nativeSession,'--message',secondMessage);
    const retried = await settle(failed.turns+1,nativeSession);
    assert.equal(retried.agent_session_id,nativeFirst.agent_session_id);
    assert.equal(retried.last_final_text,secondAnswer);
    await cli('session','close',nativeSession);
    assert.equal(await readFile(vault,'utf8'), nativeCredential);
    assert.equal(await readFile(nativePatch,'utf8'), nativeRoute);
    console.log(JSON.stringify({harness:'dsh', registered:true, selectedModel:'deepseek-v4-pro',
      resumedAfterDaemonRestart:true, historyPreserved:true, approvedOnce:true, cancelled:true, managedTask:true, reroutedSession:true, nativeProfilePreserved:true, provider:'loopback'}));
  } finally {
    try { await stop(); } finally { await fixture.dispose(); }
  }
}

if (process.argv[1] && pathToFileURL(process.argv[1]).href === import.meta.url) await check();
