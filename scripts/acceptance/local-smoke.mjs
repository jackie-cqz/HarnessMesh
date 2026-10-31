// Real two-node TLS and processes on ONE machine. No model calls or Tailnet required.
import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, copyFileSync, writeFileSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import net from 'node:net';
import { randomUUID } from 'node:crypto';
import { assert, atomicJson, fileHash, Mesh, until } from './common.mjs';
import { prepare, run } from './mac.mjs';
import { startWorker } from './worker.mjs';
const repo = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const binary = join(repo, 'target/debug', process.platform === 'win32' ? 'hmesh.exe' : 'hmesh');
const fixture = join(repo, 'target/debug', process.platform === 'win32' ? 'hmesh-fixture.exe' : 'hmesh-fixture');
const root = mkdtempSync(join(tmpdir(), 'hmesh-acceptance '));
const fixtureRepair = process.argv.includes('--fixture-repair');
assert(process.argv.slice(2).every(a => a === '--fixture-repair'), 'Unknown smoke flag');
const daemons = []; let worker; let repairCalls = 0;
async function freePort() {
  const s = net.createServer(); await new Promise((ok, no) => { s.once('error', no); s.listen(0, '127.0.0.1', ok); });
  const port = s.address().port; await new Promise(r => s.close(r)); return port;
}
async function node(name) {
  const dir = join(root, name); const project = join(root, `${name} project`); mkdirSync(project);
  const init = spawnSync(binary, ['--json', '--data-dir', dir, 'init', '--name', name, '--port', '0'], { encoding: 'utf8', windowsHide: true });
  assert(init.status === 0, 'Build hmesh-cli before local smoke');
  const mesh = new Mesh(binary, dir);
  const child = spawn(binary, ['--data-dir', dir, 'daemon', 'start'], { stdio: 'ignore', windowsHide: true }); daemons.push({ mesh, child });
  const card = await until(() => { try { mesh.call(['daemon', 'status']); return mesh.call(['node', 'show']); } catch { return null; } });
  const config = join(root, `${name}.toml`);
  const q = JSON.stringify;
  writeFileSync(config, `[agent]\nname=${q(name)}\nharness="pi"\nproject_id=${q(name)}\nmode="managed"\n[project]\nroot=${q(project)}\nallowed_tools=["read","write","edit"]\nartifact_paths=[]\nrequire_remote_task_approval=true\n[adapter]\nexecutable=${q(fixture)}\nprovider="fixture"\nmodel="fixture"\nenv_allowlist=[]\n`);
  const agent = mesh.call(['agent', 'register', '--config', config]);
  return { mesh, card, agent, dir, project };
}
try {
  const a = await node('mac-fixture'), b = await node('windows-fixture');
  const pair = a.mesh.call(['pair', 'request', '--peer', b.card.endpoint, '--expect-fingerprint', b.card.fingerprint]);
  b.mesh.call(['pair', 'approve', pair.request_id, '--expect-fingerprint', a.card.fingerprint]);
  a.mesh.call(['pair', 'approve', pair.request_id, '--expect-fingerprint', b.card.fingerprint]);
  const backend = join(a.project, 'original.mjs'), checker = join(b.project, 'checker.mjs');
  copyFileSync(join(repo, 'scripts/acceptance/fixtures/todo.mjs'), backend);
  copyFileSync(join(repo, 'scripts/acceptance/fixtures/checker.mjs'), checker);
  const owner = join(root, 'mac.json');
  atomicJson(owner, { binary, node_dir: a.dir, project_root: a.project, backend_file: backend, backend_port: await freePort(), backend_agent_id: a.agent.agent_id, project_id: 'mac-fixture', windows_node_id: b.card.node_id, checker_sha256: fileHash(checker), lease_seconds: 600, fault_needle: "service: 'tailnet-todo'", fault_replacement: "service: 'tailnet-todo-fault'" });
  const dir = join(root, 'run'); const prepared = prepare(owner, dir, fixtureRepair);
  const workerConfig = join(root, 'worker.json');
  atomicJson(workerConfig, { binary, node_dir: b.dir, project_root: b.project, project_id: 'windows-fixture', checker_file: checker, plan_file: prepared.plan_file, state_dir: join(root, 'worker state'), listen_port: await freePort() });
  const running = run(dir, { workerWaitSeconds: 30, allowModel: fixtureRepair, repairDriver: fixtureRepair ? {
    submit: ({ backend }) => { repairCalls++; writeFileSync(backend, readFileSync(backend, 'utf8').replace("service: 'tailnet-todo-fault'", "service: 'tailnet-todo'")); return { task_id: randomUUID() }; },
    observe: task => ({ task_id: task, last_attempt_id: randomUUID(), status: 'SUCCEEDED' }),
  } : undefined });
  // Attach rejection immediately so startup failures do not become unhandled promises.
  const observed = running.then(report => ({ report }), error => ({ error }));
  worker = await startWorker(workerConfig);
  const outcome = await observed; if (outcome.error) throw outcome.error;
  assert(outcome.report.status === 'passed', `Local acceptance ${outcome.report.status}: ${outcome.report.error ?? JSON.stringify(outcome.report.cleanup)}`);
  assert(outcome.report.model_submissions === 0, 'No model calls allowed in smoke');
  assert(repairCalls === (fixtureRepair ? 1 : 0), 'Expected exactly the configured fixture repair count');
  assert(a.mesh.call(['service', 'list']).every(s => !s.enabled), 'Backend service leaked');
  assert(b.mesh.call(['service', 'list']).every(s => !s.enabled), 'Worker service leaked');
  assert(b.mesh.call(['connection', 'list']).every(s => !s.enabled), 'Backend connection leaked');
  console.log(JSON.stringify({ passed: true, model_calls: 0, scope: 'single-machine real two-node mTLS', repair: fixtureRepair ? 'test-double only' : 'none', stages: Object.keys(outcome.report.stages), report: join(dir, 'report.json') }, null, 2));
} catch (error) { console.error(`Local smoke failed: ${error.message}; diagnostics: ${root}`); process.exitCode = 1; }
finally {
  await worker?.close();
  for (const { mesh, child } of daemons.reverse()) {
    try { mesh.call(['daemon', 'stop']); } catch { /* Stop only the child this test started. */ }
    if (child.exitCode === null) await new Promise(resolveExit => { const timer = setTimeout(() => { child.kill(); resolveExit(); }, 5000); child.once('exit', () => { clearTimeout(timer); resolveExit(); }); });
  }
}
