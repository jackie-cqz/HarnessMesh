// Exercises installed pi-acp and opencode acp against a disposable, loopback-only model fixture.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {mkdir, mkdtemp, rm, writeFile} from 'node:fs/promises';
import {createServer} from 'node:http';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createInterface} from 'node:readline';
import {setTimeout as delay} from 'node:timers/promises';
import {pathToFileURL} from 'node:url';

const provider = 'command-fixture';
const ids = ['smoke-first', 'smoke-second'];
const selected = provider + '/' + ids[1];
const answer = 'fixture-response';
const summary = 'fixture-compaction-summary';
const usage = {prompt_tokens: 500, completion_tokens: 20, total_tokens: 520};

async function serveModel(req, res, calls) {
  if (req.url !== '/v1/chat/completions' || req.method !== 'POST') {
    res.writeHead(404).end();
    return;
  }
  const parts = [];
  for await (const part of req) parts.push(part);
  const body = JSON.parse(Buffer.concat(parts));
  const instructions = body.messages.map(m => typeof m.content === 'string' ? m.content : JSON.stringify(m.content)).join('\n');
  const summarizing = /summari[sz](?:e|ing|ation)|provide a detailed prompt for continuing/i.test(instructions)
    && !body.tools?.length;
  calls.push({
    model: body.model, summarizing, hasSummary: instructions.includes(summary),
    plainCommand: body.messages.some(m => m.content === '/compact'),
  });
  const content = summarizing ? summary : answer;
  const base = {id: 'fixture-completion', model: body.model, created: Math.floor(Date.now()/1000)};
  if (!body.stream) {
    res.writeHead(200, {'Content-Type': 'application/json'}).end(JSON.stringify({
      ...base, object: 'chat.completion', choices: [{index: 0, message: {role: 'assistant', content}, finish_reason: 'stop'}], usage,
    }));
    return;
  }
  res.writeHead(200, {'Content-Type': 'text/event-stream'});
  const chunk = (delta, finish_reason, tokens = {}) => res.write('data: '+JSON.stringify({
    ...base, object: 'chat.completion.chunk', choices: [{index: 0, delta, finish_reason}], ...tokens,
  })+'\n\n');
  chunk({role:'assistant', content}, null);
  chunk({}, 'stop', {usage});
  res.end('data: [DONE]\n\n');
}

// Pass env and project as the daemon's environment and project if testing through the panel.
export async function makeAgentFixture() {
  const root = await mkdtemp(join(tmpdir(), 'hmesh-real-commands-'));
  const project = join(root, 'project');
  const piDir = join(root, '.pi', 'agent');
  await Promise.all([mkdir(project), mkdir(piDir, {recursive:true})]);
  const calls = [];
  const server = createServer((req, res) => serveModel(req, res, calls).catch(() => res.writeHead(500).end()));
  const dispose = async () => {
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
    await rm(root, {recursive:true,force:true});
  };
  try {
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');
    const baseURL = 'http://127.0.0.1:'+server.address().port+'/v1';
    const promptDir = join(piDir, 'prompts');
    await mkdir(promptDir);
    await writeFile(join(promptDir, 'pi-outline.md'), '---\ndescription: Outline the current project\n---\nList project goals.\n');
    const piModels = {providers: {[provider]: {
      api: 'openai-completions', apiKey: 'test-only', baseUrl: baseURL,
      models: ids.map(id => ({id, name: id, contextWindow: 65536, maxTokens: 4096})),
    }}};
    const piSettings = {
      defaultProvider: provider, defaultModel: ids[0], quietStartup: true,
      compaction: {keepRecentTokens: 24, reserveTokens: 1024},
    };
    await Promise.all([
      writeFile(join(piDir, 'models.json'), JSON.stringify(piModels)),
      writeFile(join(piDir, 'settings.json'), JSON.stringify(piSettings)),
    ]);
    const modelNames = Object.fromEntries(ids.map(id => [id,{name:id,limit:{context:65536, output:4096}}]));
    const opencode = {
      enabled_providers: [provider], model: provider+'/'+ids[0], small_model: selected,
      command: {'oc-outline': {description: 'Outline the current project', template: 'List project goals.'}},
      provider: {[provider]: {
        name: 'Command fixture', npm: '@ai-sdk/openai-compatible', models: modelNames,
        options: {baseURL, apiKey: 'test-only'},
      }},
    };
    const env = {
      PATH: process.env.PATH, HOME: root, USERPROFILE: root, TMPDIR: tmpdir(),
      XDG_CONFIG_HOME: join(root,'config'), XDG_CACHE_HOME:join(root,'cache'), XDG_DATA_HOME:join(root,'data'), XDG_STATE_HOME:join(root,'state'),
      PI_OFFLINE:'1', PI_CODING_AGENT_DIR:piDir, OPENCODE_CONFIG_CONTENT:JSON.stringify(opencode),
      OPENCODE_DISABLE_AUTOUPDATE:'true', OPENCODE_DISABLE_MODELS_FETCH:'true',
    };
    for (const name of ['SystemRoot','ComSpec','PATHEXT','TEMP','TMP']) if (process.env[name]) env[name] = process.env[name];
    return {root,project,env,calls,dispose};
  } catch(error) {
    await dispose();
    throw error;
  }
}

function client(program, args, cwd, env) {
  const child = spawn(program, args, {cwd, env, stdio: ['pipe', 'pipe', 'ignore']});
  const stopped = once(child, 'close').catch(() => {});
  const pending = new Map();
  const updates = [];
  let nextId = 0;
  const fail = error => {
    for (const waiter of pending.values()) waiter.reject(error);
    pending.clear();
  };
  const write = message => child.stdin.write(JSON.stringify({jsonrpc: '2.0', ...message}) + '\n');
  child.once('error', fail);
  child.stdin.on('error', fail);
  child.once('close', code => fail(Error(program+' exited '+code)));
  createInterface({input: child.stdout}).on('line', line => {
    let msg;
    try { msg = JSON.parse(line); }
    catch { fail(Error(program+' emitted invalid JSON')); return; }
    if (msg.method === 'session/update') {
      updates.push(msg.params);
    } else if (msg.method && msg.id != null) {
      write({id: msg.id, error: {code: -32601, message: 'Fixture does not implement this request'}});
    } else if (pending.has(msg.id)) {
      const waiter = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) waiter.reject(Error(program+' '+waiter.method+': '+msg.error.message));
      else waiter.resolve(msg.result);
    }
  });
  async function call(method, params) {
    const id = ++nextId;
    let timeout;
    try {
      return await new Promise((resolve, reject) => {
        timeout = setTimeout(() => reject(Error(program+' timed out in '+method)), 45000);
        pending.set(id, {method, resolve, reject});
        write({id, method, params});
      });
    } finally {
      clearTimeout(timeout);
      pending.delete(id);
    }
  }
  async function close() {
    child.stdin.end();
    const force = setTimeout(() => child.kill('SIGKILL'), 2500);
    try { await stopped; } finally { clearTimeout(force); }
  }
  return {updates, call, close};
}

async function check(kind) {
  const fixture = await makeAgentFixture();
  const [program, ...args] = kind === 'pi' ? ['pi-acp'] : ['opencode', 'acp'];
  const acp = client(program, args, fixture.project, fixture.env);
  try {
    const init = await acp.call('initialize',{protocolVersion:1,clientCapabilities:{}});
    assert.equal(init.protocolVersion,1);
    const record = await acp.call('session/new',{cwd:fixture.project,mcpServers:[]});
    const {sessionId}=record;
    const commandSnapshot = () => acp.updates.findLast(({sessionId: id, update}) =>
      id === sessionId && update.sessionUpdate === 'available_commands_update')?.update.availableCommands;
    for (let i = 0; i < 100 && !commandSnapshot(); i++) await delay(50);
    const commands = commandSnapshot();
    assert(Array.isArray(commands),'Missing command list');
    const compactAdvertised = commands.some(cmd=>cmd.name==='compact');
    const customName = kind === 'pi' ? 'pi-outline' : 'oc-outline';
    assert.equal(commands.find(command => command.name === customName)?.description, 'Outline the current project');
    if (kind === 'pi') {
      assert(compactAdvertised, 'Pi should advertise compact');
      assert(commands.find(command => command.name === 'compact').input?.hint, 'Pi should advertise compact arguments');
    }
    if (init.agentInfo?.name === 'OpenCode' && init.agentInfo.version === '1.19.6') {
      assert(!compactAdvertised, 'Recheck version-specific compact support');
    }
    assert(!commands.some(cmd=>cmd.name==='models'||cmd.name==='model'),'Update UI alias assumptions for this agent');
    const model = record.configOptions.find(opt=>opt.category==='model');
    assert(model,'No model settings');
    const configured = await acp.call('session/set_config_option',{sessionId,configId:model.id,value:selected});
    assert.equal(configured.configOptions.find(opt=>opt.id===model.id).currentValue,selected);
    assert.equal(fixture.calls.length,0,'Discovery or model selection called the model');
    const prompt=text=>acp.call('session/prompt',{sessionId,prompt:[{type:'text',text}]});
    for (let i=0;i<3;i++) {
      await prompt('Record this sample '+i+': '+('blue green yellow '.repeat(65)));
    }
    assert(fixture.calls.some(call=>call.model===ids[1]),'The selected model did not receive a request');
    const before = fixture.calls.length;
    const compact = await prompt('/compact');
    assert.equal(compact.stopReason,'end_turn');
    const duringCompact = fixture.calls.slice(before);
    assert(duringCompact.some(call => call.summarizing && !call.plainCommand),'The adapter did not request a summary from the loopback model');
    const beforeContinuation = fixture.calls.length;
    await prompt('Continue the sample conversation.');
    assert(fixture.calls.slice(beforeContinuation).some(call=>call.hasSummary), 'The continued turn did not carry the model-generated summary');
    return {
      agent: kind, agentName: init.agentInfo?.name, version: init.agentInfo?.version,
      commandCount: commands.length, compactAdvertised, selectedModel: selected,
      compacted: true, continued: true,
    };
  } finally {
    try { await acp.close(); } finally { await fixture.dispose(); }
  }
}

if (process.argv[1] && pathToFileURL(process.argv[1]).href===import.meta.url) {
  const kinds = process.argv.slice(2);
  for(const kind of (kinds.length?kinds:['pi','opencode'])){
    assert(['pi','opencode'].includes(kind),'Choose pi or opencode');
    console.log(JSON.stringify(await check(kind)));
  }
}
