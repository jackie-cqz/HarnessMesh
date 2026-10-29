#!/usr/bin/env node
// Opt-in real Tailnet-address sockets on ONE computer; no model or Mac claim.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { createPrivateKey, randomUUID } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import https from 'node:https';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';

if (process.argv.length !== 3 || process.argv[2] !== '--run-local-network') {
  console.error('Opt in: node scripts/check-tailnet-local.mjs --run-local-network (requires active local Tailscale)');
  process.exit(2);
}
const repo = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const binary = join(repo, 'target', 'debug', process.platform === 'win32' ? 'hmesh.exe' : 'hmesh');
const root = join(repo, '.hmesh-v0', `tailnet-check-${randomUUID()}`);
const nodes = [];
let report;
function mesh(dir, args, expectFailure = false) {
  const result = spawnSync(binary, ['--json', '--data-dir', dir, ...args], { cwd: repo, encoding: 'utf8', windowsHide: true, timeout: 30000 });
  if (result.error) throw result.error;
  if (expectFailure) { assert.notEqual(result.status, 0); return; }
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout);
}
async function start(name) {
  const dir = join(root, name);
  mesh(dir, ['init', '--name', name, '--network', 'tailscale', '--port', '0']);
  const child = spawn(binary, ['--data-dir', dir, 'daemon', 'start'], { cwd: repo, stdio: 'ignore', windowsHide: true });
  const exited = new Promise((resolveExit, reject) => { child.once('error', reject); child.once('exit', resolveExit); });
  const node = { dir, child, exited };
  nodes.push(node);
  for (let i = 0; i < 100; i++) {
    await delay(100);
    assert.equal(child.exitCode, null, 'Daemon failed to start');
    if (!existsSync(join(dir, 'daemon.json'))) continue;
    node.card = mesh(dir, ['node', 'show']);
    const status = mesh(dir, ['daemon', 'status']);
    assert.equal(status.pid, child.pid);
    assert.equal(status.network, 'tailscale');
    return node;
  }
  throw new Error('Daemon readiness timed out');
}
function request(node, path, owner = false) {
  return new Promise((resolveResponse, reject) => {
    const cert = readFileSync(join(node.dir, 'certificate.pem'));
    const identity = owner ? {
      cert, key: createPrivateKey({ key: readFileSync(join(node.dir, 'identity.pk8')), format: 'der', type: 'pkcs8' }).export({ format: 'pem', type: 'pkcs8' }),
      headers: { Authorization: `Bearer ${readFileSync(join(node.dir, 'owner.token'), 'utf8')}` },
    } : {};
    const req = https.get(`${node.card.endpoint}${path}`, { ca: cert, minVersion: 'TLSv1.3', ...identity }, response => {
      response.resume();
      response.on('end', () => resolveResponse(response.statusCode));
    });
    req.setTimeout(5000, () => req.destroy(new Error('HTTPS probe deadline exceeded')));
    req.on('error', reject);
  });
}
try {
  mkdirSync(root, { recursive: true, mode: 0o700 });
  const a = await start('tailnet-a');
  const b = await start('tailnet-b');
  assert.equal(await request(b, '/hmesh/v1/agents'), 401);
  assert.equal(await request(b, '/local/status', true), 403);
  mesh(a.dir, ['pair', 'request', '--peer', b.card.endpoint, '--expect-fingerprint', '0'.repeat(64)], true);
  for (const [origin, target] of [[a, b], [b, a]]) {
    const pending = mesh(origin.dir, ['pair', 'request', '--peer', target.card.endpoint, '--expect-fingerprint', target.card.fingerprint]);
    mesh(target.dir, ['pair', 'approve', pending.request_id, '--expect-fingerprint', origin.card.fingerprint]);
  }
  for (const origin of [a, b]) {
    const peers = mesh(origin.dir, ['peers', 'list']);
    assert.equal(peers.length, 1);
    assert.equal(peers[0].registry_readable, true);
  }
  mesh(b.dir, ['pair', 'revoke', a.card.node_id]);
  assert.equal(mesh(a.dir, ['peers', 'list'])[0].registry_readable, false);
  report = { scope: 'one-machine-two-daemons-real-tailnet-ip-tls', ssh_used: false, model_called: false,
    mac_tested: false, passed: true, checks: ['exact-ip-bind-and-san', 'local-owner-api', 'anonymous-denied',
      'owner-api-denied-on-tailnet-with-owner-credentials', 'wrong-pin-denied', 'bidirectional-pairing-and-mtls', 'revoked-peer-denied'],
    endpoints: [a.card.endpoint, b.card.endpoint], node_ids: [a.card.node_id, b.card.node_id] };
} finally {
  for (const node of nodes) {
    if (node.child.exitCode === null && node.child.signalCode === null) {
      try { mesh(node.dir, ['daemon', 'stop']); } catch { /* Kill only this spawned child if necessary. */ }
      await Promise.race([node.exited, delay(5000)]);
      if (node.child.exitCode === null && node.child.signalCode === null) {
        node.child.kill();
        await Promise.race([node.exited, delay(5000)]);
      }
    }
    assert.ok(node.child.exitCode !== null || node.child.signalCode !== null, 'Owned test daemon did not exit');
  }
}
report.cleanup = 'both-owned-daemons-exited';
writeFileSync(join(root, 'report.json'), JSON.stringify(report, null, 2), { mode: 0o600 });
console.log(JSON.stringify({ ...report, report: join(root, 'report.json') }, null, 2));
