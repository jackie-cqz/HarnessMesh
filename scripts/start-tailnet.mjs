#!/usr/bin/env node
// Run on each device locally. No SSH, sudo, system installation or model call.
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, realpathSync, writeFileSync } from 'node:fs';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';

import { agentConfig, agentName, selectAgents, configDigest } from './tailnet-config.mjs';

const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const flags = new Map();
const valued = new Set(['--root', '--port', '--name', '--project', '--agents', '--tailscale-executable']);
for (let i = 2; i < process.argv.length; i++) {
  const flag = process.argv[i];
  if (valued.has(flag) && process.argv[i + 1] && !process.argv[i + 1].startsWith('--')) flags.set(flag, process.argv[++i]);
  else if (flag === '--skip-build') flags.set(flag, true);
  else if (flag === '--help') {
    console.log('node scripts/start-tailnet.mjs [--root ABSOLUTE_DIR] [--port 39880] [--name NODE_NAME] [--project ABSOLUTE_DIR] [--agents opencode,claude-code,codex,pi,dsh] [--tailscale-executable ABSOLUTE_PATH] [--skip-build]\nRegisters every ACP agent already installed on this computer (or the ones --agents lists). Nothing is installed and no model is called.');
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
  mkdirSync(root, { recursive: true, mode: 0o700 });
  const nodeName = flags.get('--name') ?? (process.platform === 'darwin' ? 'mac' : process.platform === 'win32' ? 'windows' : 'linux');
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,47}$/.test(nodeName)) throw new Error('--name may use letters, digits, dot, dash and underscore');
  const project = flags.has('--project') ? flags.get('--project') : join(root, 'project');
  if (!isAbsolute(project)) throw new Error('--project must be an absolute directory');
  // Only agents already installed here; HarnessMesh never installs one.
  const detected = JSON.parse(run(binary, ['--json', 'agent', 'detect']));
  const agents = selectAgents(detected.installed, flags.get('--agents'));
  if (!agents.length) console.log(`No ACP agent found on PATH. Install one yourself, e.g. ${detected.not_found.map(a => a.install).filter(Boolean).join(' | ')}`);
  const init = ['init', '--name', nodeName, '--port', port, '--network', 'tailscale'];
  if (flags.has('--tailscale-executable')) init.push('--tailscale-executable', absoluteFile('--tailscale-executable'));
  const card = mesh(init);
  if (new URL(card.endpoint).port !== port) throw new Error('Existing instance uses a different port; use a fresh --root');
  mkdirSync(project, { recursive: true, mode: 0o700 });
  const configs = agents.map(agent => {
    const config = agentConfig({ node: nodeName, harness: agent.harness, executable: agent.executable, project });
    const path = join(root, `${nodeName}-${agent.harness}-${configDigest(config)}.toml`);
    if (!existsSync(path)) writeFileSync(path, config, { flag: 'wx', mode: 0o600 });
    return { path, name: agentName(nodeName, agent.harness) };
  });
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
  const registered = [];
  for (const { path, name } of configs) {
    // Registration runs only an ACP initialize handshake; no session, no model.
    const existing = mesh(['agents', 'list']).find(item => item.name === name);
    if (existing?.probe?.protocol_probe === 'legacy_dsh_headless') {
      throw new Error(`${name} is a historical headless registration. To keep its ID, run hmesh agent update ${existing.agent_id} --config ${JSON.stringify(path)} in this instance, then restart.`);
    }
    registered.push(existing ?? mesh(['agent', 'register', '--config', path]));
  }
  const receipt = { node: card, agents: registered.map(a => ({ agent_id: a.agent_id, name: a.name, harness: a.harness, acp: a.probe?.protocol_probe })), data_dir: nodeDir, project };
  writeFileSync(join(root, 'startup.json'), JSON.stringify(receipt, null, 2), { mode: 0o600 });
  console.log(JSON.stringify(receipt, null, 2));
  console.log("Keep this terminal open. Pair by comparing both Node fingerprints. Paired computers see this node's agents and can open sessions with them right away; use `hmesh peers access <peer> confirm` to approve each one here first. Ctrl+C stops only this daemon. No model has been called.");
  console.log(`Open the panel: ${binary} --data-dir ${JSON.stringify(nodeDir)} panel`);
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
