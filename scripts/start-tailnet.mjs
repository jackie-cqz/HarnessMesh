#!/usr/bin/env node
// Run on each device locally. No SSH, sudo, system installation or model call.
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { delimiter, dirname, isAbsolute, join, resolve } from 'node:path';
import { homedir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';

import { resolvePi, credentialSource, configName } from './tailnet-config.mjs';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const flags = new Map();
const valued = new Set(['--root', '--port', '--pi', '--models', '--credential-source', '--tailscale-executable']);
for (let i = 2; i < process.argv.length; i++) {
  const flag = process.argv[i];
  if (valued.has(flag) && process.argv[i + 1] && !process.argv[i + 1].startsWith('--')) flags.set(flag, process.argv[++i]);
  else if (flag === '--skip-build') flags.set(flag, true);
  else if (flag === '--help') {
    console.log('node scripts/start-tailnet.mjs [--root ABSOLUTE_DIR] [--port 39880] [--pi ABSOLUTE_PATH] [--models ABSOLUTE_JSON] [--credential-source ABSOLUTE_AUTH_JSON] [--tailscale-executable ABSOLUTE_PATH] [--skip-build]');
    process.exit(0);
  } else throw new Error(`Unknown option or missing value: ${flag}`);
}
const root = flags.has('--root') ? flags.get('--root') : join(repo, '.hmesh-v0', `tailnet-${process.platform}`);
if (!isAbsolute(root)) throw new Error('--root must be an absolute dedicated directory');
const port = flags.get('--port') ?? '39880';
if (!/^\d+$/.test(port) || Number(port) < 1 || Number(port) > 65535) throw new Error('Port must be 1..65535');
const binary = join(repo, 'target', 'debug', process.platform === 'win32' ? 'hmesh.exe' : 'hmesh');
const nodeDir = join(root, 'node');
const hargs = ['--json', '--data-dir', nodeDir];
function run(program, args, { inherit = false, timeout = 30000 } = {}) {
  const result = spawnSync(program, args, { cwd: repo, encoding: 'utf8', windowsHide: true,
    timeout, maxBuffer: 2 * 1024 * 1024, stdio: inherit ? 'inherit' : 'pipe' });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${program} failed (exit ${result.status}). ${inherit ? '' : result.stderr ?? ''}`);
  return result.stdout;
}
function mesh(args) { return JSON.parse(run(binary, [...hargs, ...args])); }
function absoluteFile(flag, fallback) {
  const path = flags.get(flag) ?? fallback;
  if (!path || !isAbsolute(path) || !existsSync(path)) throw new Error(`Set ${flag} to an existing absolute local file`);
  return realpathSync(path);
}
function findPi() {
  for (const dir of (process.env.PATH ?? '').split(delimiter).filter(isAbsolute)) {
    const path = join(dir, 'pi');
    if (existsSync(path)) return realpathSync(path);
  }
  throw new Error('Pi is missing from PATH; pass --pi /absolute/path/to/pi');
}
function toml(value) { return JSON.stringify(value); }

let daemon;
let exited;
let stopping = false;
async function stopOwnedDaemon() {
  if (stopping || !daemon || daemon.exitCode !== null || daemon.signalCode !== null) return;
  stopping = true;
  try { mesh(['daemon', 'stop']); } catch { /* Preserve diagnostics from the startup error. */ }
  await Promise.race([exited, delay(5000)]);
  if (daemon.exitCode === null && daemon.signalCode === null) daemon.kill();
}
try {
  if (Number(process.versions.node.split('.')[0]) < 20) throw new Error('Node.js 20 or newer is required');
  if (!flags.has('--skip-build')) {
    let version;
    try { version = run('cargo', ['+stable', '--version']); }
    catch { throw new Error('Rust stable >= 1.90 is required. Install Rust with rustup, run rustup toolchain install stable --profile minimal, then retry.'); }
    const match = version.match(/cargo (\d+)\.(\d+)/);
    if (!match || Number(match[1]) !== 1 || Number(match[2]) < 90) throw new Error('Install Rust stable >= 1.90 before starting');
    run('cargo', ['+stable', 'build', '--locked', '-p', 'hmesh-cli'], { inherit: true, timeout: 600000 });
  }
  if (!existsSync(binary)) throw new Error('Build hmesh-cli before using --skip-build');
  // Resolve Mac configuration before starting a daemon. Keys stay in the existing
  // local file; only a path reference is written into the dedicated Agent config.
  let piConfig;
  let agentName;
  if (process.platform === 'darwin') {
    const executable = resolvePi(absoluteFile('--pi', flags.has('--pi') ? undefined : findPi()));
    const models = absoluteFile('--models', join(homedir(), '.pi', 'agent', 'models.json'));
    const definition = JSON.parse(readFileSync(models, 'utf8'));
    if (!definition.providers?.deepseek?.models?.some(model => model.id === 'deepseek-v4-flash')) {
      throw new Error('Selected models.json does not define deepseek/deepseek-v4-flash; choose the existing Pi file with --models');
    }
    const defaultAuth = join(homedir(), '.pi', 'agent', 'auth.json');
    const auth = credentialSource({ explicit: flags.has('--credential-source') ? absoluteFile('--credential-source') : undefined, auth: defaultAuth, models, envKey: process.env.DEEPSEEK_API_KEY });
    if (!auth && !process.env.DEEPSEEK_API_KEY) throw new Error('Configure the local Pi DeepSeek API key or DEEPSEEK_API_KEY first');
    const project = join(root, 'project');
    const adapter = [`executable = ${toml(executable)}`, `node_executable = ${toml(process.execPath)}`,
      `models_file = ${toml(models)}`, 'provider = "deepseek"', 'model = "deepseek-v4-flash"',
      `env_allowlist = ${auth ? '[]' : '["DEEPSEEK_API_KEY"]'}`];
    if (auth) adapter.push(`credential_source = ${toml(auth)}`);
    piConfig = ['[agent]', 'name = "mac-pi-flash"', 'harness = "pi"', 'project_id = "tailnet-demo"',
      'mode = "managed"', '[project]', `root = ${toml(project)}`, 'allowed_tools = ["read", "write", "edit"]',
      'artifact_paths = ["out/health.json"]', 'require_remote_task_approval = true', '[adapter]', ...adapter, ''].join('\n');
  }
  if (piConfig) {
    agentName = configName(piConfig);
    piConfig = piConfig.replace('name = "mac-pi-flash"', `name = ${toml(agentName)}`);
  }
  mkdirSync(root, { recursive: true, mode: 0o700 });
  const init = ['init', '--name', process.platform === 'darwin' ? 'mac-pi' : 'windows-tailnet', '--port', port, '--network', 'tailscale'];
  if (flags.has('--tailscale-executable')) init.push('--tailscale-executable', absoluteFile('--tailscale-executable'));
  const card = mesh(init);
  if (new URL(card.endpoint).port !== port) throw new Error('Existing instance uses a different port; use a fresh --root');
  if (piConfig) {
    mkdirSync(join(root, 'project'), { recursive: true, mode: 0o700 });
    const configPath = join(root, `${agentName}.toml`);
    if (existsSync(configPath) && readFileSync(configPath, 'utf8') !== piConfig) throw new Error('Existing Agent config differs; use a fresh --root to preserve prior registrations');
    if (!existsSync(configPath)) writeFileSync(configPath, piConfig, { flag: 'wx', mode: 0o600 });
  }
  // A second invocation refuses the daemon lock; it never adopts another process.
  daemon = spawn(binary, [...hargs, 'daemon', 'start'], { cwd: repo, stdio: ['ignore', 'inherit', 'inherit'], windowsHide: true });
  exited = new Promise((resolveExit, reject) => { daemon.once('error', reject); daemon.once('exit', (code, signal) => resolveExit({ code, signal })); });
  const readyUntil = Date.now() + 20000;
  let ready = false;
  while (Date.now() < readyUntil) {
    await delay(100);
    if (daemon.exitCode !== null || daemon.signalCode !== null) throw new Error('Daemon exited during startup; read its diagnostic above');
    try { if (mesh(['daemon', 'status']).pid === daemon.pid) { ready = true; break; } } catch { /* Wait only for this owned daemon. */ }
  }
  if (!ready) throw new Error('Daemon did not become ready within 20 seconds');
  let agent;
  if (piConfig) {
    agent = mesh(['agents', 'list']).find(item => item.name === agentName && item.project_ids.includes('tailnet-demo'));
    if (!agent) agent = mesh(['agent', 'register', '--config', join(root, `${agentName}.toml`)]);
  }
  const receipt = { node: card, agent, data_dir: nodeDir, project: piConfig ? join(root, 'project') : undefined };
  writeFileSync(join(root, 'startup.json'), JSON.stringify(receipt, null, 2), { mode: 0o600 });
  console.log(JSON.stringify(receipt, null, 2));
  console.log('Keep this terminal open. Pair by comparing both Node fingerprints; project grants and remote task approvals remain local. Ctrl+C stops only this daemon. No model has been called.');
  if (agent) console.log(`Local model verification (optional, billed): ${binary} --data-dir ${JSON.stringify(nodeDir)} agent verify ${agent.agent_id} --allow-model`);
  process.on('SIGINT', stopOwnedDaemon);
  process.on('SIGTERM', stopOwnedDaemon);
  const result = await exited;
  if (!stopping && result.code !== 0) throw new Error(`Daemon exited: ${JSON.stringify(result)}`);
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
} finally {
  await stopOwnedDaemon();
}
