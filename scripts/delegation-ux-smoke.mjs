// Real headless browser and two disposable loopback TLS Nodes. Explicit fixture only.
// Never reads a personal Harness configuration, connects to Tailnet or calls a model.
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { Mesh, assert, until } from './acceptance/common.mjs';

const { chromium } = await import(process.env.HMESH_PLAYWRIGHT_MODULE || 'playwright');
const root = mkdtempSync(join(tmpdir(), 'hmesh-delegation-ux-'));
const suffix = process.platform === 'win32' ? '.exe' : '';
const binary = resolve('target/debug/hmesh' + suffix);
const fixture = resolve('target/debug/hmesh-fixture' + suffix);
const children = [], nodes = [], checks = [];
let browser, stage = 'setup';

function start(args, stdout = 'ignore') {
  const child = spawn(binary, args, { stdio: ['ignore', stdout, 'ignore'], windowsHide: true });
  children.push(child);
  return child;
}

async function makeNode(name) {
  const dir = join(root, name);
  const initialized = spawnSync(binary, ['--json', '--data-dir', dir, 'init', '--name', name, '--port', '0'], { encoding: 'utf8', timeout: 30000 });
  assert(initialized.status === 0, 'Temporary Node initialization failed');
  const mesh = new Mesh(binary, dir);
  const node = { dir, mesh };
  nodes.push(node);
  start(['--data-dir', dir, 'daemon', 'start']);
  node.card = await until(() => {
    try { mesh.call(['daemon', 'status']); return mesh.call(['node', 'show']); }
    catch { return null; }
  });
  return node;
}

async function makePanel(node) {
  const child = start(['--json', '--data-dir', node.dir, 'panel', '--port', '0'], 'pipe');
  const lines = createInterface({ input: child.stdout });
  let timer;
  try {
    const [line] = await Promise.race([
      once(lines, 'line'),
      once(child, 'exit').then(() => { throw Error('Temporary panel exited before readiness'); }),
      new Promise((_, reject) => { timer = setTimeout(() => reject(Error('Temporary panel readiness deadline exceeded')), 20000); timer.unref(); }),
    ]);
    const ready = JSON.parse(line);
    assert(ready.scope === 'loopback-owner-panel', 'Wrong panel scope');
    return ready;
  } finally { clearTimeout(timer); lines.close(); }
}

function pass(name) { checks.push({ name, status: 'pass' }); }
async function confirm(page) {
  await page.locator('#confirm-dialog').waitFor();
  await page.locator('#confirm-accept').click();
}

async function fixtureSucceeded(mesh, id) {
  return until(() => {
    const task = mesh.call(['task', 'show', id]);
    if (['FAILED', 'UNKNOWN', 'CANCELLED', 'EXPIRED', 'REJECTED'].includes(task.status)) throw Error('Explicit fixture task ended ' + task.status);
    return task.status === 'SUCCEEDED' ? task : null;
  });
}

function counts(origin, worker) {
  return {
    submitted: origin.mesh.call(['task', 'list']).submitted.length,
    executions: worker.mesh.call(['task', 'list']).executions.length,
  };
}

function sameCounts(actual, expected, message) {
  assert(actual.submitted === expected.submitted && actual.executions === expected.executions, message);
}

async function preflight(page, expectedReady) {
  await page.locator('#compose-check').click();
  await page.locator(`#compose-preflight[data-ready="${expectedReady}"]`).waitFor();
}

async function openFromList(page, id) {
  await page.locator('[data-view="tasks"]').click();
  await page.locator('[data-filter="all"]').click();
  await page.locator('#refresh').click();
  const row = page.locator(`#task-list tr[data-task-id="${id}"]`);
  await row.locator('.task-title').click();
  await page.locator('#task-detail .detail-id').filter({ hasText: id }).waitFor();
}

async function panelApi(page, path) {
  return page.evaluate(async path => unwrap(await api(path)), path);
}

try {
  const origin = await makeNode('ux-origin'), worker = await makeNode('ux-worker');
  const project = join(root, 'project');
  mkdirSync(project);
  const config = join(root, 'fixture-agent.toml');
  writeFileSync(config, [
    '[agent]', 'name="delegation-ux-fixture"', 'harness="pi"', 'project_id="ux-project"', 'mode="managed"',
    '[project]', 'root=' + JSON.stringify(project), 'allowed_tools=["read","write","edit"]',
    'artifact_paths=["out/health.json"]', 'require_remote_task_approval=true',
    '[adapter]', 'executable=' + JSON.stringify(fixture), 'provider="fixture"', 'model="fixture"', 'env_allowlist=[]', '',
  ].join('\n'));
  const agent = worker.mesh.call(['agent', 'register', '--config', config]);
  const pairing = origin.mesh.call(['pair', 'request', '--peer', worker.card.endpoint, '--expect-fingerprint', worker.card.fingerprint]);
  worker.mesh.call(['pair', 'approve', pairing.request_id, '--expect-fingerprint', origin.card.fingerprint]);
  origin.mesh.call(['pair', 'approve', pairing.request_id, '--expect-fingerprint', worker.card.fingerprint]);
  const policy = join(root, 'worker-policy.toml');
  writeFileSync(policy, ['[default]', 'action="deny"', '[peers]', 'default="confirm"', '[[grant]]', `peer="${origin.card.node_id}"`, `agent="${agent.agent_id}"`, 'project="ux-project"', 'actions=["agent.read","task.submit"]', 'require_approval=true', ''].join('\n'));
  worker.mesh.call(['policy', '--file', policy]);
  // --allow-model reaches only this explicitly registered test double, never a real Harness.
  const probe = worker.mesh.call(['agent', 'verify', agent.agent_id, '--allow-model', '--prompt', 'fixture:create']);
  await fixtureSucceeded(worker.mesh, probe.task_id);
  const [originReady, workerReady] = await Promise.all([makePanel(origin), makePanel(worker)]);
  browser = await chromium.launch({ headless: true, ...(process.env.HMESH_CHROME ? { executablePath: process.env.HMESH_CHROME } : {}) });
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, reducedMotion: 'reduce' });
  // Observe the browser's copy action without writing the computer's clipboard.
  await context.addInitScript(() => Object.defineProperty(navigator, 'clipboard', {
    configurable: true,
    value: { writeText: async text => { window.__copiedHandoff = text; } },
  }));
  const page = await context.newPage(), owner = await context.newPage(), errors = [];
  for (const view of [page, owner]) { view.setDefaultTimeout(20000); view.on('pageerror', error => errors.push(error.message)); }
  await page.goto(originReady.url);
  await owner.goto(workerReady.url);
  await page.locator('#local-name').filter({ hasText: 'ux-origin' }).waitFor();
  await owner.locator('#local-name').filter({ hasText: 'ux-worker' }).waitFor();

  stage = 'runtime-capabilities';
  const runtime = await panelApi(page, '/runtime');
  assert(runtime.mode === 'full' && runtime.capabilities.task_workflow && runtime.capabilities.task_preflight && runtime.capabilities.development_workflows, 'Current daemon capabilities were not detected');
  assert(runtime.cli_version && Number.isInteger(runtime.cli_schema_version) && runtime.daemon.running, 'Runtime version information missing');
  await page.locator('#runtime-summary').filter({ hasText: runtime.cli_version }).waitFor();
  pass(stage);

  stage = 'missing-artifact-preflight-has-no-side-effects';
  const before = counts(origin, worker);
  let taskPosts = 0;
  page.on('request', request => { if (new URL(request.url()).pathname === '/api/tasks' && request.method() === 'POST') taskPosts++; });
  await page.locator('#new-task').click();
  await page.locator('#compose-peer').selectOption(worker.card.node_id);
  await page.waitForFunction(id => document.querySelector('#compose-agent').value === id && !document.querySelector('#compose-agent').disabled, agent.agent_id);
  await page.locator('#compose-goal').fill('fixture:create');
  await page.locator('#compose-artifacts').fill('out/not-declared.json');
  const id = (await page.locator('#compose-binding').innerText()).match(/[0-9a-f]{8}-[0-9a-f-]{27}/g)?.at(-1);
  assert(id, 'Composer did not expose its stable request ID');
  await preflight(page, false);
  sameCounts(counts(origin, worker), before, 'Read-only preflight persisted or submitted a task');
  assert(await page.locator('#compose-submit').isDisabled(), 'Blocked preflight left submission enabled');
  assert(!await page.locator('#confirm-dialog').isVisible() && taskPosts === 0, 'Blocked preflight reached submission confirmation or task POST');
  sameCounts(counts(origin, worker), before, 'Blocked preflight created a task or attempt');
  pass(stage);

  stage = 'bridge-also-rejects-unmet-artifact-contract';
  const rejected = await page.evaluate(async draft => {
    try { await api('/tasks', { method: 'POST', body: JSON.stringify(draft) }); return { accepted: true }; }
    catch (error) { return { code: error.code, submission_attempted: error.submission_attempted }; }
  }, { request_id: id, peer_id: worker.card.node_id, agent_id: agent.agent_id, project_id: 'ux-project', goal: 'fixture:create', timeout_seconds: 600, max_output_bytes: 16777216, expected_artifacts: ['out/not-declared.json'] });
  assert(rejected.code === 'TASK_PREFLIGHT_FAILED' && rejected.submission_attempted === false, 'Bridge did not identify a known pre-dispatch rejection');
  sameCounts(counts(origin, worker), before, 'Bridge preflight rejection admitted a task');
  assert(taskPosts === 1, 'Unexpected task requests during direct bridge rejection');
  taskPosts = 0;
  pass(stage);

  stage = 'changed-input-invalidates-preflight';
  await page.locator('#compose-artifacts').fill('out/health.json');
  await page.locator('#compose-preflight[data-ready="unchecked"]').waitFor();
  await preflight(page, true);
  sameCounts(counts(origin, worker), before, 'Ready preflight created a task');
  pass(stage);

  stage = 'confirmation-rechecks-current-artifact-grant';
  assert((await page.locator('#compose-binding').innerText()).includes(id), 'Read-only preflight changed the original request ID');
  await page.locator('#compose-submit').click();
  await page.locator('#confirm-dialog').waitFor();
  worker.mesh.call(['agent', 'artifacts', agent.agent_id, '--path', 'out/other.json']);
  await confirm(page);
  await page.locator('#compose-preflight[data-ready="false"]').waitFor();
  await page.waitForFunction(() => !document.querySelector('#compose-check').disabled);
  assert(taskPosts === 0, 'A stale approval preview dispatched a task');
  sameCounts(counts(origin, worker), before, 'A stale preview created a task or attempt');
  assert(await page.locator('#compose-goal').isEnabled() && await page.locator('#compose-artifacts').isEnabled(), 'Known preflight failure froze an uncertain submission');
  assert(!(await page.locator('#pending-submission').innerText()).trim(), 'Known preflight failure created an unconfirmed request ledger');
  worker.mesh.call(['agent', 'artifacts', agent.agent_id, '--path', 'out/health.json']);
  pass(stage);

  stage = 'remote-task-contract-and-owner-handoff';
  await preflight(page, true);
  await page.locator('#compose-submit').click();
  await confirm(page);
  await page.locator('#task-detail .detail-id').filter({ hasText: id }).waitFor();
  const pending = worker.mesh.call(['task', 'show', id]);
  assert(pending.status === 'PENDING_APPROVAL' && !pending.last_attempt_id, 'Remote task bypassed receiver approval');
  assert(JSON.stringify(pending.request.expected_artifacts) === JSON.stringify(['out/health.json']), 'Submitted artifact contract was not frozen');
  assert(taskPosts === 1 && !await page.locator('#task-approve').count(), 'Origin received a remote approval action or duplicate task POST');
  await page.locator('#approval-handoff').filter({ hasText: 'ux-worker' }).waitFor();
  await page.locator('#copy-task-handoff').click();
  const copied = await page.evaluate(() => window.__copiedHandoff);
  assert(typeof copied === 'string' && copied.includes(id) && copied.includes('ux-worker'), 'Handoff did not identify the fixed task and receiver');
  const privateValues = [originReady.url, workerReady.url, ...[origin, worker].map(node => readFileSync(join(node.dir, 'owner.token'), 'utf8')), ...[originReady, workerReady].map(ready => new URLSearchParams(new URL(ready.url).hash.slice(1)).get('access'))];
  assert(!copied.includes('#access=') && !copied.includes('owner.token') && privateValues.every(value => value && !copied.includes(value)), 'Handoff exposed an owner credential or private launch link');
  assert(await page.locator('#nav-pending').isHidden(), 'Outbound approval was counted as a local owner inbox task');
  pass(stage);

  stage = 'receiver-inbox-and-approval-preflight';
  await owner.locator('#refresh').click();
  await owner.locator('#approval-inbox').getByRole('button', { name: /审阅本机待审批.*1/ }).waitFor();
  assert(await owner.locator('#nav-pending').innerText() === '1', 'Receiver did not count its own pending task');
  await owner.locator('#approval-inbox').getByRole('button', { name: /审阅本机待审批/ }).click();
  await owner.locator(`#task-list tr[data-task-id="${id}"] .task-title`).click();
  await owner.locator('#task-preflight[data-ready="true"]').waitFor();
  worker.mesh.call(['agent', 'artifacts', agent.agent_id, '--path', 'out/other.json']);
  await owner.locator('#task-approve').click();
  await owner.locator('#task-preflight[data-ready="false"]').waitFor();
  assert(!await owner.locator('#confirm-dialog').isVisible(), 'Invalid approval preflight reached confirmation');
  const stillPending = worker.mesh.call(['task', 'show', id]);
  assert(stillPending.status === 'PENDING_APPROVAL' && !stillPending.last_attempt_id, 'Invalid approval preflight started execution');
  worker.mesh.call(['agent', 'artifacts', agent.agent_id, '--path', 'out/health.json']);
  await owner.locator('#refresh-task').click();
  await owner.locator('#task-preflight[data-ready="true"]').waitFor();
  await owner.locator('#task-approve').click();
  await confirm(owner);
  const complete = await fixtureSucceeded(worker.mesh, id);
  await page.bringToFront();
  await page.locator('#acceptance-state').filter({ hasText: '待验收' }).waitFor();
  pass(stage);

  stage = 'execution-success-is-not-artifact-acceptance';
  await page.locator('[data-close="task-dialog"]').click();
  await page.locator('[data-view="tasks"]').click();
  await page.locator('#refresh').click();
  const taskRow = page.locator(`#task-list tr[data-task-id="${id}"]`);
  await taskRow.locator('.acceptance-state').filter({ hasText: '待验收' }).waitFor();
  assert((await taskRow.innerText()).includes('执行成功'), 'Task list did not separate successful execution from acceptance');
  await taskRow.locator('.task-title').click();
  pass(stage);

  stage = 'detail-read-failure-preserves-input-and-recovers';
  const expectedInput = page.getByLabel('预期 JSON', { exact: true });
  const unsentValue = '{"status":"keep-my-unsubmitted-input"}';
  await expectedInput.fill(unsentValue);
  await expectedInput.focus();
  const originalInput = await expectedInput.elementHandle();
  let interrupted = false;
  const taskDetailsUrl = `**/api/tasks/${id}`;
  const interruptOneRead = async route => {
    if (!interrupted && route.request().method() === 'GET') { interrupted = true; return route.abort('failed'); }
    return route.continue();
  };
  await page.route(taskDetailsUrl, interruptOneRead);
  await page.waitForFunction(() => !state.detailLoading);
  // Exercise the same read-only observer used by the timer, without clicking a
  // toolbar button that would itself move focus away from the active textarea.
  await page.evaluate(() => readDetail(state.detailVersion));
  await page.locator('#task-read-error').waitFor();
  assert(interrupted && await expectedInput.inputValue() === unsentValue, 'A failed detail read discarded the unsent verification draft');
  assert(await originalInput.evaluate(node => node.isConnected && node === document.activeElement), 'A failed detail read replaced the active input or stole focus');
  await page.unroute(taskDetailsUrl, interruptOneRead);
  await page.evaluate(() => readDetail(state.detailVersion));
  await page.locator('#task-read-error').waitFor({ state: 'hidden' });
  assert(await expectedInput.inputValue() === unsentValue && await originalInput.evaluate(node => node.isConnected && node === document.activeElement), 'Successful read recovery discarded the active draft or focus');
  await originalInput.dispose();
  pass(stage);

  stage = 'late-verification-preserves-active-input';
  const verification = worker.mesh.call(['task', 'verify', id, '--artifact', 'out/health.json', '--expect-json', '{"status":"ok"}']);
  assert(verification.passed && verification.source === 'completion-snapshot', 'Fixture owner verification failed');
  await page.locator('#detail-updates').waitFor();
  assert(await expectedInput.inputValue() === unsentValue, 'Background verification update replaced an active draft');
  assert(await expectedInput.evaluate(node => node === document.activeElement), 'Background update stole input focus');
  await page.locator('#apply-detail-updates').click();
  await page.locator('.verification-result > strong').filter({ hasText: '核验通过' }).waitFor();
  assert(await expectedInput.inputValue() === unsentValue, 'Applying results discarded an unsubmitted verification input');
  await page.locator('#acceptance-state').filter({ hasText: '核验通过' }).waitFor();
  assert(worker.mesh.call(['task', 'show', id]).last_attempt_id === complete.last_attempt_id, 'Late verification reran the original task');
  pass(stage);

  stage = 'terminal-details-auto-observe-new-verifications';
  await expectedInput.evaluate(node => node.blur());
  const negative = spawnSync(binary, ['--json', '--data-dir', worker.dir, 'task', 'verify', id, '--artifact', 'out/health.json', '--expect-json', '{"status":"wrong"}'], { encoding: 'utf8', timeout: 30000 });
  assert(negative.status === 5 && JSON.parse(negative.stdout).passed === false, 'Fixture negative assertion did not report its expected failing verification');
  await page.locator('.verification-result > strong').filter({ hasText: '核验未通过' }).waitFor();
  assert(await page.locator('.verification-result').count() === 2, 'Terminal detail did not observe the new verification');
  await page.locator('#acceptance-state[data-state="failed"]').waitFor();
  pass(stage);

  stage = 'refresh-also-updates-terminal-details';
  worker.mesh.call(['task', 'verify', id, '--artifact', 'out/health.json', '--expect-json', '{"status":"ok"}']);
  const detailResponse = page.waitForResponse(response => new URL(response.url()).pathname === `/api/tasks/${id}` && response.request().method() === 'GET');
  await page.locator('#refresh-task').click();
  await detailResponse;
  await page.waitForFunction(() => document.querySelectorAll('.verification-result').length === 3);
  assert(worker.mesh.call(['task', 'show', id]).last_attempt_id === complete.last_attempt_id && worker.mesh.call(['task', 'list']).executions.filter(task => task.task_id === id).length === 1, 'Observation or verification duplicated an execution');
  await page.locator('[data-close="task-dialog"]').click();
  await openFromList(page, id);
  await page.locator('#acceptance-state').filter({ hasText: '核验通过' }).waitFor();
  await owner.locator('[data-close="task-dialog"]').click();
  await owner.locator('#refresh').click();
  await owner.locator('#nav-pending').waitFor({ state: 'hidden' });
  mkdirSync(resolve('.hmesh-v0/panel'), { recursive: true });
  await page.locator('#task-dialog').evaluate(dialog => { dialog.scrollTop = 0; });
  await page.screenshot({ path: resolve('.hmesh-v0/panel/delegation-ux-desktop.png') });
  pass(stage);

  stage = 'legacy-runtime-read-only-presentation';
  const legacy = await context.newPage();
  legacy.setDefaultTimeout(20000);
  legacy.on('pageerror', error => errors.push(error.message));
  // The Rust integration test supplies an actual older TLS daemon. Here only the
  // capability payload is changed to exercise the browser's read-only presentation.
  await legacy.route('**/api/runtime', route => route.fulfill({
    status: 200, contentType: 'application/json', body: JSON.stringify({ data: {
      ...runtime,
      daemon: { ...runtime.daemon, schema_version: 6, api_features: ['panel-task-workflow-v1'] },
      capabilities: { task_workflow: true, task_preflight: false, development_workflows: false },
      mode: 'read_only',
      upgrade: { required: true, automatic: false, steps: ['保留当前实例，先保存工作，再离线备份并按兼容版本升级。'] },
    } }),
  }));
  await legacy.goto(originReady.url);
  await legacy.locator('#runtime-summary').filter({ hasText: '只读' }).waitFor();
  await legacy.locator('#local-name').filter({ hasText: 'ux-origin' }).waitFor();
  assert(await legacy.locator('#new-task').isDisabled() || await legacy.locator('#new-task').isHidden(), 'Legacy read-only mode exposed task creation');
  const development = legacy.locator('[data-view="development"]');
  assert(await development.isDisabled() || await development.isHidden(), 'Legacy mode exposed unsupported development actions');
  assert(await legacy.locator('#stop-panel').isEnabled(), 'Read-only mode disabled closing the owned panel');
  await legacy.setViewportSize({ width: 390, height: 844 });
  await legacy.evaluate(() => {
    window.scrollTo(0, 0);
    return new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
  });
  const layout = await legacy.evaluate(() => {
    const selectors = ['.sidebar', 'main', '#runtime-summary', '#runtime-info'];
    const elements = Object.fromEntries(selectors.map(selector => {
      const nodes = [...document.querySelectorAll(selector)];
      return [selector, {
        count: nodes.length,
        bounds: nodes.map(node => { const r = node.getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: r.height, position: getComputedStyle(node).position }; }),
      }];
    }));
    return { viewport: { width: innerWidth, height: innerHeight, dpr: devicePixelRatio }, scrollHeight: document.documentElement.scrollHeight, bodyScrollHeight: document.body.scrollHeight, elements };
  });
  assert(layout.elements['.sidebar'].count === 1 && layout.elements.main.count === 1 && layout.elements['#runtime-summary'].count === 1, 'Legacy runtime DOM duplicated its navigation or content');
  writeFileSync(resolve('.hmesh-v0/panel/delegation-ux-mobile-layout.json'), JSON.stringify(layout, null, 2) + '\n', { mode: 0o600 });
  assert(!await legacy.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1), 'Legacy runtime page overflows the mobile viewport');
  // Use the actual mobile viewport. The system Chrome's beyond-viewport capture
  // can repeat compositor tiles after a resize although the DOM is not repeated.
  await legacy.screenshot({ path: resolve('.hmesh-v0/panel/delegation-ux-mobile.png') });
  pass(stage);

  stage = 'responsive-task-results';
  await page.locator('[data-close="task-dialog"]').click();
  await page.setViewportSize({ width: 390, height: 844 });
  await openFromList(page, id);
  assert(!await page.evaluate(() => document.documentElement.scrollWidth > innerWidth + 1), 'Task results overflow the mobile viewport');
  assert(!errors.length, 'Browser JavaScript errors');
  pass(stage);
  const result = { status: 'passed', checks, real_model_calls: 0, fixture_runs: 2, screenshots: ['delegation-ux-desktop.png', 'delegation-ux-mobile.png'], scope: 'temporary two-node loopback mTLS and headless browser; legacy browser presentation mocked; no Windows operator claim' };
  mkdirSync(resolve('.hmesh-v0/panel'), { recursive: true });
  writeFileSync(resolve('.hmesh-v0/panel/delegation-ux-result.json'), JSON.stringify(result, null, 2) + '\n', { mode: 0o600 });
  console.log(JSON.stringify(result));
} catch (error) {
  const failure = { status: 'failed', stage, error: error.message.replace(/#access=[a-f0-9]{64}/gi, '#access=[redacted]'), diagnostics: root, checks };
  mkdirSync(resolve('.hmesh-v0/panel'), { recursive: true });
  writeFileSync(resolve('.hmesh-v0/panel/delegation-ux-failure.json'), JSON.stringify(failure, null, 2) + '\n', { mode: 0o600 });
  console.error(JSON.stringify({ status: failure.status, stage: failure.stage, error: failure.error, diagnostics: '.hmesh-v0/panel/delegation-ux-failure.json' }));
  process.exitCode = 1;
} finally {
  await browser?.close();
  for (const node of nodes) { try { node.mesh.call(['daemon', 'stop']); } catch { /* Only the disposable test Nodes are owned here. */ } }
  for (const child of children.reverse()) {
    if (child.exitCode === null && child.signalCode === null) {
      child.kill('SIGINT');
      await Promise.race([once(child, 'exit'), new Promise(resolve => setTimeout(resolve, 3000))]);
      if (child.exitCode === null && child.signalCode === null) child.kill();
    }
  }
}
