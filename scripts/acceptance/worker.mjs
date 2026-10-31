// Owner-started fixed-action worker. All cross-device traffic goes through HarnessMesh.
import http from 'node:http';
import { existsSync, realpathSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { TodoProbe } from './checks.mjs';
import { acquireLock, assert, atomicJson, Blocked, endpoint, fileHash, flags, hash, inside, Journal, keys, localFile, Mesh, readJson, runChecker, until, UUID, validatePlan } from './common.mjs';

export function stageOrder(plan) {
  return ['baseline', 'seed', 'verify_persistence', ...(plan.repair ? ['assert_broken', 'repaired'] : []), 'assert_revoked', 'cleanup'];
}
export class ActionEngine {
  constructor(plan, journal, perform) {
    this.plan = plan; this.journal = journal; this.perform = perform; this.busy = false;
    for (const step of Object.values(journal.data.steps)) if (step.state === 'running') { step.state = 'blocked'; step.error = 'Worker restarted during action; mutation outcome unknown'; }
    journal.save();
  }
  submit(body) {
    keys(body, ['run_id', 'action_id', 'stage']);
    assert(body.run_id === this.plan.run_id && UUID.test(body.action_id), 'Action belongs to a different run or invalid ID');
    const old = this.journal.data.steps[body.stage];
    if (old) { assert(old.action_id === body.action_id, 'Stage already reserved with a different action ID'); return old; }
    assert(!this.busy, 'Another fixed action is running');
    const next = stageOrder(this.plan).find(stage => this.journal.data.steps[stage]?.state !== 'passed');
    assert(body.stage === next, 'Action out of order or unsupported');
    const step = { action_id: body.action_id, stage: body.stage, state: 'running', started_at: new Date().toISOString() };
    this.journal.data.steps[body.stage] = step; this.journal.save(); this.busy = true;
    this.pending = Promise.resolve().then(() => this.perform(body.stage)).then(result => { step.state = 'passed'; step.result = result; }, error => {
      step.state = error instanceof Blocked ? 'blocked' : 'failed'; step.error = error.message;
    }).finally(() => { step.completed_at = new Date().toISOString(); this.journal.save(); this.busy = false; });
    return step;
  }
  result(id) { return Object.values(this.journal.data.steps).find(step => step.action_id === id); }
}
export async function startWorker(configFile) {
  const owner = readJson(localFile(configFile));
  assert(typeof owner.state_dir === 'string', 'Missing worker state directory');
  const release = acquireLock(join(resolve(owner.state_dir), 'worker.lock'));
  try { return await startWorkerLocked(configFile, release); } catch (error) { release(); throw error; }
}
async function startWorkerLocked(configFile, release) {
  const config = readJson(localFile(configFile));
  keys(config, ['binary', 'node_dir', 'project_root', 'checker_file', 'plan_file', 'state_dir', 'listen_port', 'project_id']);
  const planFile = localFile(config.plan_file), plan = validatePlan(readJson(planFile));
  const planHash = fileHash(planFile);
  const project = realpathSync(config.project_root), checker = inside(project, localFile(config.checker_file));
  assert(fileHash(checker) === plan.checker_sha256, 'Checker differs from the reviewed pinned artifact');
  assert(Number.isInteger(config.listen_port) && config.listen_port > 0 && config.listen_port <= 65535, 'Invalid worker loopback port');
  const stateDir = resolve(config.state_dir), mesh = new Mesh(config.binary, config.node_dir);
  assert(mesh.call(['node', 'show']).node_id === plan.windows_node_id, 'Wrong worker node identity');
  assert(typeof config.project_id === 'string' && mesh.call(['agents', 'list']).some(a => a.project_ids.includes(config.project_id)), 'Worker project must already be registered');
  const peer = mesh.call(['peers', 'list']).find(p => p.card.node_id === plan.mac_node_id && p.status === 'trusted');
  assert(peer, 'Mac peer must already be paired and trusted');
  const journal = new Journal(join(stateDir, 'worker-state.json'), hash(JSON.stringify({ planHash, checker, node: plan.windows_node_id })));
  // Journal also pins local connection evidence. No arbitrary endpoint can arrive via HTTP.
  let probe, base, ready = false, closing = false, timer;
  const abortChecker = new AbortController();
  const createdAt = journal.data.created_at ?? Date.now(); journal.data.created_at = createdAt; journal.save();
  const expires = createdAt + plan.lease_seconds * 1000;
  assert(expires > Date.now(), 'Worker run lease expired; create a new run');
  async function perform(stage) {
    assert(ready && !closing && Date.now() < expires, 'Worker not ready or run expired');
    assert(fileHash(planFile) === planHash && fileHash(checker) === plan.checker_sha256, 'Frozen plan or checker changed');
    switch (stage) {
      case 'baseline': return { checker: await runChecker(checker, plan.checker_sha256, base, { signal: abortChecker.signal }), independent: await probe.crud() };
      case 'seed': {
        const todo = await probe.seed();
        // Commit the business identity before any verification or response.
        journal.data.seed = todo; journal.save(); await probe.verify(todo);
        return { passed: true, id: todo.id };
      }
      case 'verify_persistence': {
        assert(journal.data.seed, 'Missing persistent test item'); await probe.verify(journal.data.seed);
        const result = await probe.remove(journal.data.seed); journal.data.seed_removed = true; journal.save(); return result;
      }
      case 'assert_broken': {
        await probe.healthCheck({ ...plan.health, service: `${plan.health.service}-fault` });
        const failure = await runChecker(checker, plan.checker_sha256, base, { signal: abortChecker.signal, expectFailure: true });
        return { passed: true, original_contract_failed: true, checker: failure };
      }
      case 'repaired': return { checker: await runChecker(checker, plan.checker_sha256, base, { signal: abortChecker.signal }), independent: await probe.crud() };
      case 'assert_revoked': return probe.revoked();
      case 'cleanup': {
        mesh.call(['connection', 'revoke', plan.backend_connection_id]);
        return { passed: true, backend_connection_revoked: true };
      }
      default: throw new Error('Unsupported fixed action');
    }
  }
  const engine = new ActionEngine(plan, journal, perform);
  const server = http.createServer(async (req, res) => {
    const send = (status, data) => { res.writeHead(status, { 'content-type': 'application/json', 'cache-control': 'no-store' }); res.end(JSON.stringify(data)); };
    try {
      assert(req.headers.host === `127.0.0.1:${config.listen_port}` && !req.headers.origin, 'Only local owner clients or mTLS forwarding are supported');
      if (req.url === '/health' && req.method === 'GET') return send(200, { status: 'ok' });
      if (req.url === '/status' && req.method === 'GET') return send(200, { run_id: plan.run_id, plan_sha256: planHash, checker_sha256: plan.checker_sha256, ready, expires_at: expires, platform: process.platform });
      if (req.url?.startsWith('/results/') && req.method === 'GET') {
        const id = req.url.slice('/results/'.length); assert(UUID.test(id), 'Invalid action ID');
        const result = engine.result(id); return send(result ? 200 : 404, result ?? { error: 'No reservation; do not automatically retry a mutation' });
      }
      if (req.url === '/finish' && req.method === 'POST') {
        assert(req.headers['content-type']?.split(';')[0] === 'application/json', 'JSON content type required');
        let body = ''; for await (const c of req) { body += c; assert(body.length <= 4096, 'Request too large'); }
        const value = JSON.parse(body); keys(value, ['run_id']); assert(value.run_id === plan.run_id, 'Wrong finish run');
        ready = false; abortChecker.abort();
        journal.data.finish_requested = true; journal.save();
        send(200, { accepted: true, run_id: plan.run_id, steps: journal.data.steps });
        // Revoke only after the response has traversed the authenticated service.
        setTimeout(() => { void close(); }, 3000);
        return;
      }
      if (req.url === '/actions' && req.method === 'POST') {
        assert(req.headers['content-type']?.split(';')[0] === 'application/json', 'JSON content type required');
        assert(ready && !closing && Date.now() < expires, 'Worker not ready or lease expired');
        let size = 0; const chunks = [];
        for await (const chunk of req) { size += chunk.length; assert(size <= 4096, 'Action request too large'); chunks.push(chunk); }
        const result = engine.submit(JSON.parse(Buffer.concat(chunks).toString('utf8'))); return send(202, result);
      }
      send(404, { error: 'No such fixed action endpoint' });
    } catch (error) { send(409, { error: error.message }); }
  });
  server.requestTimeout = 10000; server.headersTimeout = 10000;
  await new Promise((ok, no) => { server.once('error', no); server.listen(config.listen_port, '127.0.0.1', ok); });
  async function close() {
    if (closing) return; closing = true; ready = false; clearTimeout(timer); abortChecker.abort();
    const failures = [];
    for (const [command, id] of [['connection', plan.backend_connection_id], ['service', plan.worker_service_id]]) {
      try { mesh.call([command, 'revoke', id]); } catch { failures.push(`${command} cleanup unresolved`); }
    }
    if (engine.busy) { try { await until(() => !engine.busy, 10, 'Owned action cleanup unconfirmed'); } catch { failures.push('Owned action still unresolved; lock retained'); } }
    journal.data.cleanup = { at: new Date().toISOString(), failures }; journal.save();
    server.close(); server.closeAllConnections(); if (!engine.busy) release();
  }
  try {
    assert(!journal.data.finish_requested, 'Run already finished; use a new plan');
    const existing = mesh.call(['service', 'list']).find(r => r.id === plan.worker_service_id);
    if (existing) assert(existing.enabled && existing.expires_at * 1000 > Date.now(), 'Old worker service inactive; use a new run');
    else mesh.config(['service', 'publish'], { id: plan.worker_service_id, project_id: config.project_id, name: `acceptance-${plan.run_id.slice(0, 8)}`, target_port: config.listen_port, allowed_peer_ids: [plan.mac_node_id], allowed_restart_peer_ids: [], lease_seconds: Math.ceil((expires - Date.now()) / 1000), health_path: '/health', request_timeout_seconds: 10, process: null }, join(stateDir, 'worker-service.json'));
    timer = setTimeout(() => { void close(); }, Math.max(1, expires - Date.now()));
    await until(() => mesh.call(['service', 'list', '--peer', plan.mac_node_id]).find(s => s.id === plan.backend_service_id && s.health === 'healthy'), 120, 'Mac backend not published or unreachable');
    const connections = mesh.call(['connection', 'list']);
    if (!connections.some(c => c.id === plan.backend_connection_id)) mesh.config(['service', 'connect'], { id: plan.backend_connection_id, peer_id: plan.mac_node_id, service_id: plan.backend_service_id, local_port: 0, lease_seconds: Math.ceil((expires - Date.now()) / 1000) }, join(stateDir, 'backend-connection.json'));
    base = await endpoint(mesh, plan.backend_connection_id); probe = new TodoProbe(base, plan.health, plan.run_id);
    await probe.healthCheck(); ready = true;
    console.log(JSON.stringify({ worker: 'ready', run_id: plan.run_id, plan_sha256: planHash, evidence: journal.file }));
    return { close, server, engine, journal, base };
  } catch (error) { await close(); throw error; }
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    const args = flags(process.argv.slice(2), ['--config']); assert(args['--config'], 'Usage: node scripts/acceptance/worker.mjs --config ABSOLUTE_JSON');
    const worker = await startWorker(args['--config']);
    for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => { void worker.close(); });
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
