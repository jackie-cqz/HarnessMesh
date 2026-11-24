// Black-box MCP smoke: two disposable TLS Nodes, the shipped `hmesh mcp`
// server and the explicit Rust fixture harness. No model or account access.
import { spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { once } from 'node:events';
import { createServer } from 'node:net';
import { createInterface } from 'node:readline';
import { Mesh, assert, until } from './acceptance/common.mjs';

const root = mkdtempSync(join(tmpdir(), 'hmesh-mcp-'));
const suffix = process.platform === 'win32' ? '.exe' : '';
const binary = resolve('target/debug/hmesh' + suffix);
const fixture = resolve('target/debug/hmesh-fixture' + suffix);
const owned = [];
const checks = [];
let stage = 'setup';
const pass = name => checks.push({ name, status: 'pass' });

async function freePort() {
  const reservation = createServer();
  reservation.listen(0, '127.0.0.1');
  await once(reservation, 'listening');
  const port = reservation.address().port;
  await new Promise(r => reservation.close(r));
  return port;
}
async function node(name) {
  const dir = join(root, name);
  mkdirSync(dir);
  const mesh = new Mesh(binary, dir);
  mesh.call(['init', '--name', name, '--port', String(await freePort())]);
  const child = spawn(binary, ['--data-dir', dir, 'daemon', 'start'], { stdio: 'ignore', windowsHide: true });
  owned.push(child);
  await until(() => { try { return mesh.call(['daemon', 'status']); } catch { return null; } }, 30);
  return { dir, mesh, child, card: mesh.call(['node', 'show']) };
}
const finished = (n, id) => until(() => {
  const t = n.mesh.call(['task', 'show', id]);
  return ['SUCCEEDED', 'FAILED', 'UNKNOWN', 'CANCELLED', 'REJECTED'].includes(t.status) && t;
}, 60);

/** Minimal MCP stdio client; asserts every stdout line is a JSON-RPC frame. */
function mcpClient(dir) {
  const child = spawn(binary, ['--data-dir', dir, 'mcp'], { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  owned.push(child);
  const waiting = new Map();
  let nextId = 1;
  createInterface({ input: child.stdout }).on('line', line => {
    const frame = JSON.parse(line);
    assert(frame.jsonrpc === '2.0', 'Non JSON-RPC output on MCP stdout');
    waiting.get(frame.id)?.(frame);
    waiting.delete(frame.id);
  });
  const send = value => child.stdin.write(JSON.stringify(value) + '\n');
  const request = (method, params) => new Promise((resolve, reject) => {
    const id = nextId++;
    const timer = setTimeout(() => reject(new Error(`MCP ${method} timed out`)), 90000);
    waiting.set(id, frame => { clearTimeout(timer); resolve(frame); });
    send({ jsonrpc: '2.0', id, method, params });
  });
  return {
    child,
    notify: method => send({ jsonrpc: '2.0', method }),
    request,
    async tool(name, args) {
      const frame = await request('tools/call', { name, arguments: args });
      assert(!frame.error, `MCP protocol error for ${name}`);
      return frame.result;
    },
  };
}

try {
  const origin = await node('mcp-origin');
  const worker = await node('mcp-worker');
  const project = join(root, 'project');
  mkdirSync(project);
  const config = join(root, 'fixture.toml');
  writeFileSync(config, ['[agent]', 'name="mcp-fixture"', 'harness="acp"', 'project_id="mcp-demo"', '[project]', 'root=' + JSON.stringify(project), 'allowed_tools=["read","write","edit"]', 'artifact_paths=["out/health.json"]', '[adapter]', 'executable=' + JSON.stringify(fixture), 'provider="fixture"', 'model="fixture"', 'env_allowlist=[]', ''].join('\n'));
  const agent = worker.mesh.call(['agent', 'register', '--config', config]);
  // --allow-model reaches only this explicitly registered test double.
  const probe = worker.mesh.call(['agent', 'verify', agent.agent_id, '--allow-model', '--prompt', 'fixture:create']);
  assert((await finished(worker, probe.task_id)).status === 'SUCCEEDED', 'Fixture verification failed');
  const pairing = origin.mesh.call(['pair', 'request', '--peer', worker.card.endpoint, '--expect-fingerprint', worker.card.fingerprint]);
  worker.mesh.call(['pair', 'approve', pairing.request_id, '--expect-fingerprint', origin.card.fingerprint]);
  origin.mesh.call(['pair', 'approve', pairing.request_id, '--expect-fingerprint', worker.card.fingerprint]);

  stage = 'pairing-alone-shows-remote-agents';
  // No policy file is written: pairing is enough.
  const directory = origin.mesh.call(['agents', 'list', '--all']);
  const remote = directory.find(n => n.name === 'mcp-worker');
  assert(remote?.reachable && remote.agents.some(a => a.agent_id === agent.agent_id), 'Paired worker agents not visible');
  pass(stage);

  stage = 'mcp-handshake-and-tools';
  const mcp = mcpClient(origin.dir);
  const init = await mcp.request('initialize', { protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'mcp-smoke', version: '1' } });
  assert(init.result?.protocolVersion === '2025-06-18', 'Client protocol version not honored');
  mcp.notify('notifications/initialized');
  const tools = (await mcp.request('tools/list', {})).result.tools.map(t => t.name);
  assert(JSON.stringify(tools) === JSON.stringify(['mesh_list_agents', 'mesh_send', 'mesh_wait', 'mesh_get_session', 'mesh_cancel', 'mesh_close']), 'Unexpected tool list');
  pass(stage);

  stage = 'mcp-lists-agents-by-reference';
  const listed = await mcp.tool('mesh_list_agents', {});
  const target = listed.structuredContent.agents.find(a => a.ref === 'mcp-worker/mcp-fixture');
  assert(target?.ready === true && target.local === false, 'Remote agent reference missing or not ready');
  pass(stage);

  stage = 'mcp-send-opens-a-session-and-returns-reply';
  const reply = await mcp.tool('mesh_send', { agent: 'mcp-worker/mcp-fixture', message: 'fixture:reply', wait_seconds: 60 });
  const body = reply.structuredContent;
  assert(!reply.isError && body.done === true && body.status === 'idle', 'Remote session turn did not finish automatically');
  assert(body.final_text === 'Final answer: the build is green.', 'Final reply missing or wrong');
  pass(stage);

  stage = 'mcp-send-continues-the-session';
  const follow = (await mcp.tool('mesh_send', { session_id: body.session_id, message: 'fixture:echo again', wait_seconds: 60 })).structuredContent;
  assert(follow.final_text === 'Echo #2: again' && follow.turns === 2, 'Follow-up did not continue the conversation');
  const progress = (await mcp.tool('mesh_get_session', { session_id: body.session_id })).structuredContent;
  assert(progress.events.some(e => e.kind === 'turn_end'), 'Session events missing');
  const closed = (await mcp.tool('mesh_close', { session_id: body.session_id })).structuredContent;
  assert(closed.status === 'closed', 'Session did not close');
  pass(stage);

  stage = 'peer-access-confirm-parks-sessions';
  const switched = worker.mesh.call(['peers', 'access', 'mcp-origin', 'confirm']);
  assert(switched.access === 'confirm', 'Access switch not applied');
  const parked = (await mcp.tool('mesh_send', { agent: 'mcp-worker/mcp-fixture', message: 'fixture:reply' })).structuredContent;
  assert(parked.status === 'pending_approval', 'Confirm mode did not park the session');
  const still = (await mcp.tool('mesh_wait', { session_id: parked.session_id, wait_seconds: 1 })).structuredContent;
  assert(still.done === false && still.status === 'pending_approval', 'Parked session advanced without approval');
  worker.mesh.call(['session', 'approve', parked.session_id]);
  const approved = (await mcp.tool('mesh_wait', { session_id: parked.session_id, wait_seconds: 60 })).structuredContent;
  assert(approved.status === 'idle' && approved.final_text === 'Final answer: the build is green.', 'Approved session did not return its reply');
  worker.mesh.call(['peers', 'access', 'mcp-origin', 'auto']);
  pass(stage);

  stage = 'unknown-agent-is-a-tool-error';
  const unknown = await mcp.tool('mesh_send', { agent: 'mcp-worker/missing', message: 'fixture:reply' });
  assert(unknown.isError && /known agents/.test(unknown.structuredContent.error.message), 'Unknown agent not explained');
  pass(stage);

  mcp.child.stdin.end();
  const [code] = await once(mcp.child, 'exit');
  assert(code === 0, 'MCP server did not exit cleanly on stdin close');
  console.log(JSON.stringify({ status: 'passed', checks, real_model_calls: 0, evidence: 'two-local-nodes-explicit-fixture' }));
} catch (e) {
  console.error(JSON.stringify({ status: 'failed', stage, error: e.message, checks }));
  process.exitCode = 1;
} finally {
  for (const child of owned.reverse()) {
    if (child.exitCode === null && child.signalCode === null) {
      child.kill('SIGINT');
      await Promise.race([once(child, 'exit'), new Promise(r => setTimeout(r, 5000))]);
      if (child.exitCode === null && child.signalCode === null) child.kill();
    }
  }
}
