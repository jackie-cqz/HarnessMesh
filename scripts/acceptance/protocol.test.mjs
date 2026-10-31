import { test } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import http from 'node:http';
import { ActionEngine, stageOrder } from './worker.mjs';
import { hash, Journal, loopback, runChecker, validatePlan } from './common.mjs';
import { TodoProbe } from './checks.mjs';
const plan = () => ({ schema: 1, ...Object.fromEntries(['run_id', 'mac_node_id', 'windows_node_id', 'backend_service_id', 'worker_service_id', 'backend_connection_id', 'worker_connection_id'].map(k => [k, randomUUID()])), project_id: 'test', checker_sha256: hash('checker'), health: { status: 'ok', service: 'tailnet-todo', node: 'mac' }, repair: false, lease_seconds: 1800 });
test('fixed actions reserve once, reject cross-run/commands/order and never reexecute', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'hmesh-actions-'));
  try {
    const p = plan(), j = new Journal(join(dir, 'state.json'), 'binding'); let calls = 0;
    const engine = new ActionEngine(p, j, async () => { calls++; return { passed: true }; });
    const action = { run_id: p.run_id, action_id: randomUUID(), stage: 'baseline' };
    assert.throws(() => engine.submit({ ...action, command: 'anything' }));
    assert.throws(() => engine.submit({ ...action, run_id: randomUUID() }));
    assert.throws(() => engine.submit({ ...action, stage: 'seed' }));
    assert.equal(engine.submit(action).state, 'running'); engine.submit(action); await engine.pending;
    assert.equal(engine.submit(action).state, 'passed'); assert.equal(calls, 1);
    assert.throws(() => engine.submit({ ...action, action_id: randomUUID() }));
    assert.throws(() => new Journal(join(dir, 'state.json'), 'different'));
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
test('worker crash leaves unknown mutations blocked, never replays them', () => {
  const dir = mkdtempSync(join(tmpdir(), 'hmesh-crash-'));
  try {
    const p = plan(), j = new Journal(join(dir, 'state.json'), 'binding'); const id = randomUUID();
    j.data.steps.baseline = { state: 'running', action_id: id, stage: 'baseline' }; j.save();
    const engine = new ActionEngine(p, new Journal(j.file, 'binding'), () => { throw Error('must not execute'); });
    assert.equal(engine.submit({ run_id: p.run_id, action_id: id, stage: 'baseline' }).state, 'blocked');
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
test('local mutations journal reservation before dispatch and refuse replay', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'hmesh-local-'));
  try {
    const j = new Journal(join(dir, 'state.json'), 'binding'); let calls = 0;
    await assert.rejects(j.once('restart', () => { calls++; throw Error('lost response'); }));
    await assert.rejects(j.once('restart', () => { calls++; })); assert.equal(calls, 1);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
test('frozen plan rejects unknown fields and arbitrary endpoints', () => {
  assert.equal(validatePlan(plan()).schema, 1);
  assert.throws(() => validatePlan({ ...plan(), command: 'shell' }));
  for (const u of ['http://localhost:80', 'http://example.com', 'http://127.0.0.1:80/path', 'http://x:y@127.0.0.1']) assert.throws(() => loopback(u));
  assert.equal(stageOrder({ repair: true }).filter(s => s === 'repaired').length, 1);
});
test('revocation does not pass on an accessible backend', async () => {
  const server = http.createServer((_q, res) => { res.setHeader('content-type', 'application/json'); res.end('{}'); });
  await new Promise(r => server.listen(0, '127.0.0.1', r));
  try { await assert.rejects(new TodoProbe(`http://127.0.0.1:${server.address().port}`, {}, randomUUID()).revoked()); }
  finally { server.closeAllConnections(); await new Promise(r => server.close(r)); }
});
test('checker hash pin rejects changed scripts before execution', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'hmesh-checker-'));
  try { const f = join(dir, 'checker.mjs'); writeFileSync(f, 'throw Error("must not run")'); await assert.rejects(runChecker(f, hash('expected'), 'http://127.0.0.1:1'), /Frozen checker changed/); }
  finally { rmSync(dir, { recursive: true, force: true }); }
});
test('checker execution strips model credentials and distinguishes expected defect from success', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'hmesh-pinned-'));
  const old = process.env.HMESH_TEST_SECRET; process.env.HMESH_TEST_SECRET = 'must-not-inherit';
  try {
    const f = join(dir, 'checker.mjs');
    const names = ['health', 'list', 'create', 'list_verify_created', 'patch', 'list_verify_patched', 'delete', 'list_verify_removed'];
    writeFileSync(f, `if(process.env.HMESH_TEST_SECRET)process.exit(4);console.log(JSON.stringify({ok:true,base_url:process.env.BASE_URL,checks:${JSON.stringify(names.map(name => ({ name, status: 'pass' })))}}));`);
    const digest = hash((await import('node:fs')).readFileSync(f));
    assert.equal((await runChecker(f, digest, 'http://127.0.0.1:1')).ok, true);
    await assert.rejects(runChecker(f, digest, 'http://127.0.0.1:1', { expectFailure: true }), /did not detect/);
    writeFileSync(f, 'process.exit(2)'); const bad = hash('process.exit(2)');
    assert.equal((await runChecker(f, bad, 'http://127.0.0.1:1', { expectFailure: true })).failed_as_expected, true);
    await assert.rejects(runChecker(f, bad, 'http://127.0.0.1:1'), /nonzero/);
  } finally { if (old === undefined) delete process.env.HMESH_TEST_SECRET; else process.env.HMESH_TEST_SECRET = old; rmSync(dir, { recursive: true, force: true }); }
});
test('timed out checkers stop and never count as an expected defect failure', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'hmesh-timeout-'));
  try {
    const f = join(dir, 'checker.mjs'), source = 'setInterval(()=>{},1000)'; writeFileSync(f, source);
    await assert.rejects(runChecker(f, hash(source), 'http://127.0.0.1:1', { timeout: 50, expectFailure: true }), /timed out/);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
test('worker kit contains only fixed source and public plan, never owner configuration', async () => {
  const { makeKit } = await import('./kit.mjs');
  const { readdirSync, readFileSync } = await import('node:fs');
  const dir = mkdtempSync(join(tmpdir(), 'hmesh-kit-'));
  try {
    writeFileSync(join(dir, 'plan.json'), JSON.stringify(plan()));
    writeFileSync(join(dir, 'mac-owner.json'), '{"secret":"must-not-transfer"}');
    const out = join(dir, 'kit'); makeKit(dir, out);
    assert.deepEqual(readdirSync(out).sort(), ['README.txt', 'checks.mjs', 'common.mjs', 'plan.json', 'start-worker.ps1', 'worker.mjs']);
    assert.equal(readFileSync(join(out, 'plan.json'), 'utf8'), readFileSync(join(dir, 'plan.json'), 'utf8'));
    assert.throws(() => makeKit(dir, out), /new directory/);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
test('exclusive run lock prevents concurrent controllers and workers', async () => {
  const { acquireLock } = await import('./common.mjs');
  const dir = mkdtempSync(join(tmpdir(), 'hmesh-lock-'));
  try {
    const lock = join(dir, 'exclusive.lock'); const release = acquireLock(lock);
    assert.throws(() => acquireLock(lock), /already locked/);
    release(); const again = acquireLock(lock); again();
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
