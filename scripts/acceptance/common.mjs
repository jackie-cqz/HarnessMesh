import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync, realpathSync, rmSync } from 'node:fs';
import { dirname, isAbsolute, relative, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

export const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
export const CHECKS = ['health', 'list', 'create', 'list_verify_created', 'patch', 'list_verify_patched', 'delete', 'list_verify_removed'];
export class Blocked extends Error {}
export const hash = bytes => createHash('sha256').update(bytes).digest('hex');
export const fileHash = file => hash(readFileSync(file));
export const readJson = file => JSON.parse(readFileSync(file, 'utf8').replace(/^\uFEFF/, ''));
export function assert(ok, message) { if (!ok) throw new Error(message); }
export function keys(value, allowed) {
  assert(value && typeof value === 'object' && !Array.isArray(value), 'Expected JSON object');
  assert(Object.keys(value).every(k => allowed.includes(k)), 'Unknown configuration or protocol field');
}
export function atomicJson(file, value) {
  mkdirSync(dirname(file), { recursive: true, mode: 0o700 });
  const tmp = `${file}.${process.pid}.tmp`;
  writeFileSync(tmp, `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600 });
  renameSync(tmp, file);
}
export function localFile(file) {
  assert(typeof file === 'string' && isAbsolute(file), 'Expected absolute local file path');
  return realpathSync(file);
}
export function inside(root, file) {
  root = realpathSync(root); file = realpathSync(file);
  const rel = relative(root, file);
  assert(rel && rel !== '..' && !rel.startsWith(`..${process.platform === 'win32' ? '\\' : '/'}`) && !isAbsolute(rel), 'File must be inside the owner project');
  return file;
}
export function validatePlan(p) {
  keys(p, ['schema', 'run_id', 'mac_node_id', 'windows_node_id', 'backend_service_id', 'worker_service_id', 'backend_connection_id', 'worker_connection_id', 'project_id', 'checker_sha256', 'health', 'repair', 'lease_seconds']);
  assert(p.schema === 1, 'Unsupported plan schema');
  for (const k of ['run_id', 'mac_node_id', 'windows_node_id', 'backend_service_id', 'worker_service_id', 'backend_connection_id', 'worker_connection_id']) assert(UUID.test(p[k]), `Invalid ${k}`);
  assert(p.mac_node_id !== p.windows_node_id, 'Two independent nodes required');
  assert(typeof p.project_id === 'string' && p.project_id.length > 0 && p.project_id.length <= 128, 'Invalid project');
  assert(/^[a-f0-9]{64}$/.test(p.checker_sha256), 'Expected reviewed checker SHA-256');
  assert(JSON.stringify(p.health) === JSON.stringify({ status: 'ok', service: 'tailnet-todo', node: 'mac' }), 'Unsupported fixed health contract');
  assert(typeof p.repair === 'boolean', 'repair must be boolean');
  assert(Number.isInteger(p.lease_seconds) && p.lease_seconds >= 600 && p.lease_seconds <= 3600, 'Lease must be 600..3600 seconds');
  return p;
}
export function loopback(url) {
  const u = new URL(url);
  assert(u.protocol === 'http:' && u.hostname === '127.0.0.1' && !u.username && !u.password && u.pathname === '/' && !u.search && !u.hash, 'Expected literal loopback HTTP origin');
  return u.origin;
}
export async function jsonRequest(base, path, { method = 'GET', body, timeout = 45000 } = {}) {
  base = loopback(base);
  const response = await fetch(base + path, { method, redirect: 'error', signal: AbortSignal.timeout(timeout), headers: body === undefined ? {} : { 'content-type': 'application/json' }, body: body === undefined ? undefined : JSON.stringify(body) });
  let size = 0; const chunks = [];
  for await (const chunk of response.body ?? []) { size += chunk.length; assert(size <= 256 * 1024, 'Response exceeds 256 KiB'); chunks.push(chunk); }
  const text = Buffer.concat(chunks).toString('utf8');
  return { status: response.status, json: text ? JSON.parse(text) : null };
}
export class Mesh {
  constructor(binary, nodeDir) { assert(isAbsolute(nodeDir), 'Expected absolute Node data directory'); this.binary = localFile(binary); this.nodeDir = realpathSync(nodeDir); }
  call(args, timeout = 45000) {
    const r = spawnSync(this.binary, ['--json', '--data-dir', this.nodeDir, ...args], { encoding: 'utf8', timeout, maxBuffer: 1024 * 1024, windowsHide: true });
    if (r.error || r.status !== 0) {
      let code = 'LOCAL_COMMAND_FAILED';
      try { code = JSON.parse(r.stderr).error.code; } catch { /* Do not expose raw subprocess diagnostics. */ }
      throw new Blocked(`HarnessMesh ${args.slice(0, 2).join(' ')}: ${code}; outcome may be uncertain`);
    }
    return JSON.parse(r.stdout);
  }
  config(args, spec, file) { atomicJson(file, spec); return this.call([...args, '--config', file]); }
}
export async function until(probe, seconds = 30, message = 'Read-only observation deadline exceeded') {
  const end = Date.now() + seconds * 1000;
  while (Date.now() < end) { const value = await probe(); if (value) return value; await delay(250); }
  throw new Blocked(message);
}
export async function endpoint(mesh, id) {
  return until(() => {
    const row = mesh.call(['connection', 'list']).find(r => r.id === id);
    if (row?.status?.endpoint) return loopback(row.status.endpoint);
    if (row && ['expired', 'stopped', 'failed_needs_input'].includes(row.status?.state)) throw new Blocked('Connection is inactive');
    return null;
  });
}
export function acquireLock(directory) {
  mkdirSync(dirname(directory), { recursive: true, mode: 0o700 });
  try { mkdirSync(directory, { mode: 0o700 }); }
  catch { throw new Blocked('Run is already locked. After a crash inspect its journal; never automatically remove a stale lock or replay mutations'); }
  writeFileSync(`${directory}/owner.json`, JSON.stringify({ pid: process.pid, started_at: new Date().toISOString() }), { mode: 0o600 });
  let released = false;
  return () => { if (!released) { released = true; rmSync(directory, { recursive: true }); } };
}
export class Journal {
  constructor(file, binding) {
    this.file = resolve(file);
    if (existsSync(file)) { this.data = readJson(file); assert(this.data.binding === binding, 'Journal belongs to a different frozen configuration'); }
    else { this.data = { schema: 1, binding, steps: {} }; this.save(); }
  }
  save() { atomicJson(this.file, this.data); }
  async once(name, action) {
    const old = this.data.steps[name];
    if (old?.state === 'passed') return old.result;
    if (old) throw new Blocked(`Step ${name} is ${old.state}; never automatically replay an uncertain mutation`);
    this.data.steps[name] = { state: 'running', started_at: new Date().toISOString() }; this.save();
    try {
      const result = await action();
      this.data.steps[name] = { ...this.data.steps[name], state: 'passed', result, completed_at: new Date().toISOString() }; this.save(); return result;
    } catch (error) {
      this.data.steps[name] = { ...this.data.steps[name], state: error instanceof Blocked ? 'blocked' : 'failed', error: error.message }; this.save(); throw error;
    }
  }
}
export async function runChecker(file, expectedHash, base, { signal, timeout = 180000, expectFailure = false } = {}) {
  if (signal?.aborted) throw new Blocked("Worker is stopping");
  assert(fileHash(file) === expectedHash, 'Frozen checker changed');
  const env = { BASE_URL: loopback(base) };
  for (const k of ['PATH', 'SystemRoot', 'WINDIR', 'TEMP', 'TMP', 'TMPDIR']) if (process.env[k]) env[k] = process.env[k];
  const child = spawn(process.execPath, [file], { cwd: dirname(file), env, detached: process.platform !== 'win32', stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
  return new Promise((resolveResult, reject) => {
    let stdout = '', size = 0, failure;
    const kill = () => {
      if (child.pid && child.exitCode === null) {
        if (process.platform === 'win32') spawnSync('taskkill', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
        else { try { process.kill(-child.pid, 'SIGKILL'); } catch { /* Child may already be gone. */ } }
      }
    };
    const abort = () => { failure = new Blocked('Worker stopped the owned checker; mutation outcome may be uncertain'); kill(); };
    signal?.addEventListener('abort', abort, { once: true });
    const timer = setTimeout(() => { failure = new Blocked('Checker timed out; mutation outcome may be uncertain'); kill(); child.stdout.destroy(); child.stderr.destroy(); }, timeout);
    child.stdout.on('data', d => { size += d.length; if (size > 256 * 1024) { failure = new Error('Checker output too large'); kill(); } else stdout += d.toString(); });
    child.stderr.on('data', d => { size += d.length; if (size > 256 * 1024) { failure = new Error('Checker output too large'); kill(); } });
    child.once('error', () => { clearTimeout(timer); reject(new Error('Cannot start pinned checker')); });
    child.once('close', code => {
      clearTimeout(timer); signal?.removeEventListener('abort', abort);
      try {
        if (failure) throw failure;
        assert(fileHash(file) === expectedHash, 'Checker changed during execution');
        if (expectFailure) { assert(Number.isInteger(code) && code !== 0, 'Frozen checker did not detect the injected defect'); resolveResult({ failed_as_expected: true, exit_code: code, checker_sha256: expectedHash, output_sha256: hash(stdout) }); return; }
        assert(code === 0, 'Pinned checker exited nonzero');
        const report = JSON.parse(stdout);
        assert(report.ok === true && report.base_url === base && Array.isArray(report.checks), 'Invalid checker report');
        assert(report.checks.length === CHECKS.length && CHECKS.every(name => report.checks.filter(c => c.name === name && c.status === 'pass').length === 1), 'Checker did not report all eight fixed checks');
        resolveResult({ ok: true, checker_sha256: expectedHash, checks: report.checks, output_sha256: hash(stdout) });
      } catch (error) { reject(error); }
    });
  });
}
export function flags(argv, names) {
  const out = {};
  for (let i = 0; i < argv.length; i++) {
    const key = argv[i]; assert(names.includes(key) && !(key in out), 'Unknown or repeated flag');
    if (key === '--allow-model') out[key] = true;
    else { assert(argv[i + 1] && !argv[i + 1].startsWith('--'), 'Missing flag value'); out[key] = argv[++i]; }
  }
  return out;
}
