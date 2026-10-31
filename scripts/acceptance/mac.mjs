import { randomUUID } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { acquireLock, assert, atomicJson, Blocked, endpoint, fileHash, flags, hash, inside, Journal, jsonRequest, keys, localFile, Mesh, readJson, until, UUID, validatePlan } from './common.mjs';

export function prepare(configFile, runDir, repair = false) {
  const config = readJson(localFile(configFile));
  keys(config, ['binary', 'node_dir', 'project_root', 'backend_file', 'backend_port', 'backend_agent_id', 'project_id', 'windows_node_id', 'checker_sha256', 'lease_seconds', 'fault_needle', 'fault_replacement']);
  const mesh = new Mesh(config.binary, config.node_dir);
  const card = mesh.call(['node', 'show']);
  const project = realpathSync(config.project_root), original = inside(project, localFile(config.backend_file));
  assert(Number.isInteger(config.backend_port) && config.backend_port > 0 && config.backend_port <= 65535, 'Invalid backend port');
  assert(UUID.test(config.backend_agent_id), 'Invalid backend Agent UUID');
  assert(mesh.call(['agents', 'list']).some(a => a.agent_id === config.backend_agent_id && a.project_ids.includes(config.project_id)), 'Wrong backend Agent/project binding');
  assert(mesh.call(['peers', 'list']).some(p => p.card.node_id === config.windows_node_id && p.status === 'trusted'), 'Windows must already be paired');
  runDir = resolve(runDir); assert(!existsSync(runDir), 'Use a new run directory');
  const run = randomUUID();
  const plan = validatePlan({ schema: 1, run_id: run, mac_node_id: card.node_id, windows_node_id: config.windows_node_id, backend_service_id: randomUUID(), worker_service_id: randomUUID(), backend_connection_id: randomUUID(), worker_connection_id: randomUUID(), project_id: config.project_id, checker_sha256: config.checker_sha256, health: { status: 'ok', service: 'tailnet-todo', node: 'mac' }, repair, lease_seconds: config.lease_seconds ?? 1800 });
  if (repair) {
    assert(typeof config.fault_needle === 'string' && config.fault_needle.length > 0 && typeof config.fault_replacement === 'string', 'Repair run needs explicit owner fault mutation');
    assert(config.fault_needle !== config.fault_replacement && readFileSync(original, 'utf8').split(config.fault_needle).length === 2, 'Fault mutation must match exactly once');
  }
  mkdirSync(dirname(runDir), { recursive: true, mode: 0o700 });
  mkdirSync(runDir, { mode: 0o700 });
  const appDir = join(project, '.acceptance', run); mkdirSync(appDir, { recursive: true, mode: 0o700 });
  const backend = join(appDir, 'server.mjs'); copyFileSync(original, backend);
  copyFileSync(original, join(runDir, 'original-server.mjs'));
  atomicJson(join(runDir, 'plan.json'), plan);
  atomicJson(join(runDir, 'mac-owner.json'), { ...config, binary: mesh.binary, node_dir: mesh.nodeDir, project_root: project, backend_file: original, run_backend_file: backend, original_sha256: fileHash(original) });
  return { run_dir: runDir, plan_file: join(runDir, 'plan.json'), run_id: run };
}
export async function run(runDir, options = {}) {
  const release = acquireLock(join(realpathSync(runDir), 'coordinator.lock'));
  try { return await runLocked(runDir, options); } finally { release(); }
}
async function runLocked(runDir, { allowModel = false, workerWaitSeconds = 120, repairDriver } = {}) {
  runDir = realpathSync(runDir);
  const planFile = join(runDir, 'plan.json'), plan = validatePlan(readJson(planFile)), planHash = fileHash(planFile);
  const config = readJson(join(runDir, 'mac-owner.json'));
  assert(!plan.repair || allowModel, 'Repair run requires --allow-model; no mutations made');
  const mesh = new Mesh(config.binary, config.node_dir);
  assert(mesh.call(['node', 'show']).node_id === plan.mac_node_id, 'Wrong Mac node identity');
  assert(fileHash(config.backend_file) === config.original_sha256, 'Original model-authored backend changed');
  const journal = new Journal(join(runDir, 'coordinator-state.json'), hash(JSON.stringify({ planHash, config })));
  const backend = inside(config.project_root, config.run_backend_file);
  const report = { schema: 1, run_id: plan.run_id, plan_sha256: planHash, checker_sha256: plan.checker_sha256, mode: plan.repair ? (repairDriver ? 'fixture-repair' : 'one-real-repair') : 'no-model', started_at: journal.data.started_at ?? new Date().toISOString(), stages: {}, model_submissions: 0, limitations: ['Transport outage and OS/TUN manipulation are not part of this run', 'Pinned checker uses owner OS privileges; hash pinning is not a sandbox', 'JSON file persistence checks process restart, not power-loss durability'] };
  const reportFile = join(runDir, 'report.json');
  journal.data.started_at = report.started_at; journal.save();
  let workerBase;
  const once = async (name, action) => {
    try { const result = await journal.once(name, action); report.stages[name] = { state: 'passed', result }; atomicJson(reportFile, report); return result; }
    catch (error) { report.stages[name] = { state: error instanceof Blocked ? 'blocked' : 'failed', error: error.message }; atomicJson(reportFile, report); throw error; }
  };
  const health = async (revision) => until(() => {
    const service = mesh.call(['service', 'list']).find(s => s.id === plan.backend_service_id);
    if (service?.enabled && service.status?.http_health === 'healthy' && service.status.revision === revision && service.status.process === 'running') return { revision, pid: service.status.pid };
    return null;
  }, 30, 'Backend did not become healthy at expected revision');
  async function remote(stage) {
    const name = `worker_${stage}`;
    const old = journal.data.steps[name];
    // Polling an existing action is safe; never repeat its POST after a lost response.
    let id = old?.action_id;
    if (!old) {
      id = randomUUID(); journal.data.steps[name] = { state: 'running', action_id: id }; journal.save();
      try {
        const ack = await jsonRequest(workerBase, '/actions', { method: 'POST', body: { run_id: plan.run_id, action_id: id, stage }, timeout: 15000 });
        if (ack.status !== 202 || ack.json?.action_id !== id) throw new Blocked('Worker did not acknowledge fixed action; mutation outcome unknown');
      } catch (error) {
        // A GET can recover the committed action if the acknowledgement was lost.
        report.stages[name] = { state: 'observing', action_id: id }; atomicJson(reportFile, report);
      }
    } else if (old.state === 'passed') {
      report.stages[name] = { state: 'passed', result: old.result }; return old.result;
    }
    const result = await until(async () => {
      let r;
      try { r = await jsonRequest(workerBase, `/results/${id}`, { timeout: 15000 }); }
      catch { throw new Blocked(`Cannot observe ${stage}; no automatic replay`); }
      if (r.status !== 200 || r.json?.action_id !== id || r.json.stage !== stage) throw new Blocked(`No durable result for ${stage}; no automatic replay`);
      if (r.json.state === 'running') return null;
      if (r.json.state === 'failed' || r.json.state === 'blocked') { journal.data.steps[name] = { state: r.json.state, action_id: id, error: r.json.error }; journal.save(); report.stages[name] = journal.data.steps[name]; }
      if (r.json.state === 'failed') throw new Error(`Worker ${stage} failed: ${r.json.error}`);
      if (r.json.state !== 'passed') throw new Blocked(`Worker ${stage} ${r.json.state}: ${r.json.error}`);
      return r.json.result;
    }, 600, 'Worker action timed out; inspect existing reservation');
    journal.data.steps[name] = { state: 'passed', action_id: id, result }; journal.save();
    report.stages[name] = { state: 'passed', result }; atomicJson(reportFile, report); return result;
  }
  async function revokeBackend() {
    const rows = mesh.call(['service', 'list']);
    const row = rows.find(r => r.id === plan.backend_service_id);
    if (!row) return { not_created: true };
    if (row.enabled) mesh.call(['service', 'revoke', plan.backend_service_id]);
    return until(() => {
      const current = mesh.call(['service', 'list']).find(r => r.id === plan.backend_service_id);
      return current && !current.enabled && current.status.process === 'stopped' ? { disabled: true, process_stopped: true } : null;
    });
  }
  try {
    const published = await once('publish_backend', () => mesh.config(['service', 'publish'], {
      id: plan.backend_service_id, project_id: plan.project_id, name: `todo-${plan.run_id.slice(0, 8)}`, target_port: config.backend_port,
      allowed_peer_ids: [plan.windows_node_id], allowed_restart_peer_ids: [], lease_seconds: plan.lease_seconds, health_path: '/health', request_timeout_seconds: 30,
      process: { executable: process.execPath, cwd: config.project_root, args: [backend], env: { PORT: String(config.backend_port) } },
    }, join(runDir, 'backend-service.json')));
    const initial = await once('backend_health', () => health(published.revision));
    await once('worker_visible', () => until(() => { try { return mesh.call(['service', 'list', '--peer', plan.windows_node_id]).find(s => s.id === plan.worker_service_id && s.health === 'healthy'); } catch { return null; } }, workerWaitSeconds, 'Windows worker not ready; start owner worker with this plan'));
    await once('worker_connection', () => mesh.config(['service', 'connect'], { id: plan.worker_connection_id, peer_id: plan.windows_node_id, service_id: plan.worker_service_id, local_port: 0, lease_seconds: plan.lease_seconds }, join(runDir, 'worker-connection.json')));
    workerBase = await endpoint(mesh, plan.worker_connection_id);
    await once('worker_identity', () => until(async () => {
      const r = await jsonRequest(workerBase, '/status', { timeout: 10000 });
      assert(r.status === 200 && r.json?.run_id === plan.run_id && r.json.plan_sha256 === planHash && r.json.checker_sha256 === plan.checker_sha256, 'Worker plan/identity binding mismatch');
      return r.json.ready ? r.json : null;
    }, 120));
    await remote('baseline'); await remote('seed');
    const restarted = await once('restart_backend', () => mesh.call(['service', 'restart', plan.backend_service_id]));
    const after = await once('restart_health', () => health(restarted.revision));
    assert(after.revision > initial.revision && after.pid !== initial.pid, 'Expected new process and service revision');
    await remote('verify_persistence');
    if (plan.repair) {
      await once('inject_fault', () => {
        const source = readFileSync(backend, 'utf8');
        assert(source.split(config.fault_needle).length === 2, 'Fault target changed; refusing mutation');
        writeFileSync(backend, source.replace(config.fault_needle, config.fault_replacement));
        return { fault_sha256: fileHash(backend), original_sha256: config.original_sha256 };
      });
      const broken = await once('restart_fault', () => mesh.call(['service', 'restart', plan.backend_service_id]));
      await once('fault_health', () => health(broken.revision)); await remote('assert_broken');
      const task = await once('submit_one_repair', () => repairDriver ? repairDriver.submit({ backend, plan }) : mesh.call(['agent', 'verify', config.backend_agent_id, '--allow-model', '--max-output-bytes', '16777216', '--timeout', '600', '--prompt',
        `Repair only ${relative(config.project_root, backend).replaceAll('\\', '/')} using read/write/edit. The fixed GET /health response must contain ${JSON.stringify(plan.health)}. Preserve the existing Todo CRUD contract and persistent data. Do not alter other files, execute shell commands or use networking. There is exactly one repair attempt. Read back the corrected file and finish briefly.`]));
      report.model_submissions = repairDriver ? 0 : 1;
      report.repair_submissions = 1;
      await once('repair_completion', () => until(() => {
        const current = repairDriver ? repairDriver.observe(task.task_id) : mesh.call(['task', 'show', task.task_id]);
        if (current.status === 'SUCCEEDED') return { task_id: current.task_id, attempt_id: current.last_attempt_id, status: current.status };
        if (['FAILED', 'CANCELLED', 'REJECTED', 'UNKNOWN'].includes(current.status)) throw new Blocked(`Repair ended ${current.status}; no second repair`);
        return null;
      }, 620, 'Repair outcome unconfirmed; no automatic retry'));
      assert(fileHash(planFile) === planHash && fileHash(config.backend_file) === config.original_sha256, 'Repair changed frozen input or original application');
      const fixed = await once('restart_repaired', () => mesh.call(['service', 'restart', plan.backend_service_id]));
      await once('repair_health', () => health(fixed.revision)); await remote('repaired');
    }
    await once('revoke_backend', revokeBackend); await remote('assert_revoked'); await remote('cleanup');
    report.status = 'passed';
  } catch (error) {
    report.status = error instanceof Blocked ? 'blocked' : 'failed'; report.error = error.message;
    if (!repairDriver && journal.data.steps.submit_one_repair && journal.data.steps.submit_one_repair.state !== 'passed') report.model_submissions = null;
    // Do not issue further application mutations after an uncertain outcome.
  } finally {
    report.cleanup = {};
    const repairTask = journal.data.steps.submit_one_repair?.result?.task_id;
    if (repairTask && report.status !== 'passed' && !repairDriver) {
      try {
        const current = mesh.call(['task', 'show', repairTask]);
        if (!['SUCCEEDED', 'FAILED', 'CANCELLED', 'REJECTED', 'UNKNOWN'].includes(current.status)) {
          mesh.call(['task', 'cancel', repairTask]);
          const end = await until(() => {
            const t = mesh.call(['task', 'show', repairTask]);
            return ['SUCCEEDED', 'FAILED', 'CANCELLED', 'REJECTED', 'UNKNOWN'].includes(t.status) ? t : null;
          }, 15, 'Repair cancellation unconfirmed');
          report.cleanup.repair = { task_id: end.task_id, status: end.status };
        }
      } catch (error) { report.cleanup.repair = { unresolved: error.message }; }
    }
    if (!workerBase) report.cleanup.worker = { unresolved: 'No authenticated worker channel; any started worker stops at its lease deadline' };
    if (workerBase) {
      try {
        const done = await jsonRequest(workerBase, '/finish', { method: 'POST', body: { run_id: plan.run_id }, timeout: 15000 });
        assert(done.status === 200 && done.json?.run_id === plan.run_id && done.json.accepted, 'Worker did not confirm finish');
        report.worker_evidence = done.json.steps;
        await until(() => !mesh.call(['service', 'list', '--peer', plan.windows_node_id]).some(s => s.id === plan.worker_service_id), 20, 'Worker service cleanup unconfirmed');
        report.cleanup.worker = 'revoked';
      } catch (error) { report.cleanup.worker = { unresolved: error.message }; if (report.status === 'passed') report.status = 'blocked'; }
    }
    try { report.cleanup.backend = await revokeBackend(); } catch (error) { report.cleanup.backend = { unresolved: error.message }; }
    try {
      if (mesh.call(['connection', 'list']).some(c => c.id === plan.worker_connection_id && c.enabled)) mesh.call(['connection', 'revoke', plan.worker_connection_id]);
      report.cleanup.worker_connection = 'revoked_or_not_created';
    } catch (error) { report.cleanup.worker_connection = { unresolved: error.message }; }
    if (Object.values(report.cleanup).some(x => x?.unresolved) && report.status === 'passed') report.status = 'blocked';
    report.completed_at = new Date().toISOString(); report.steps = journal.data.steps;
    try { report.backend_sha256 = fileHash(backend); } catch { report.backend_sha256 = null; if (report.status === 'passed') report.status = 'failed'; }
    atomicJson(reportFile, report);
  }
  return report;
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try {
    const action = process.argv[2];
    const args = flags(process.argv.slice(3), ['--config', '--out', '--run-dir', '--allow-model', '--wait-worker-seconds']);
    if (action === 'prepare') {
      assert(args['--config'] && args['--out'], 'prepare requires --config ABSOLUTE_JSON --out NEW_DIR');
      console.log(JSON.stringify(prepare(args['--config'], args['--out'], !!args['--allow-model']), null, 2));
    } else {
      assert(action === 'run' && args['--run-dir'], 'Usage: mac.mjs prepare|run with owner paths');
      const wait = Number(args['--wait-worker-seconds'] ?? 120); assert(Number.isInteger(wait) && wait >= 1 && wait <= 1200, 'Invalid worker wait budget');
      const report = await run(args['--run-dir'], { allowModel: !!args['--allow-model'], workerWaitSeconds: wait });
      console.log(JSON.stringify({ status: report.status, run_id: report.run_id, report: join(resolve(args['--run-dir']), 'report.json') }));
      if (report.status !== 'passed') process.exitCode = 1;
    }
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
