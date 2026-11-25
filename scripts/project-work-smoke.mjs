// Two disposable loopback mTLS nodes, Git repositories, and a fixture ACP agent.
import {execFileSync, spawn} from 'node:child_process';
import {mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {once} from 'node:events';
import {createServer} from 'node:net';
import {createInterface} from 'node:readline';
import {Mesh, assert, until} from './acceptance/common.mjs';
import {chromium} from 'playwright';

const root = mkdtempSync(join(tmpdir(), 'hmesh-project-work-'));
const bin = resolve(process.env.HMESH_TEST_BIN_DIR || 'target/debug');
const hm = join(bin, 'hmesh');
const fixture = join(bin, 'hmesh-fixture');
const git = execFileSync('which', ['git'], {encoding: 'utf8'}).trim();
const children = [];
let browser, page;
let stage = 'start';
function start(args, stdout = 'ignore') {
  const child = spawn(hm, args, {stdio: ['ignore', stdout, 'ignore']});
  children.push(child);
  return child;
}
function repo(path) {
  mkdirSync(path);
  execFileSync(git, ['init', '-q', '-b', 'review'], {cwd: path});
  return (...args) => execFileSync(git, ['-c', 'user.name=Fixture', '-c', 'user.email=test@example.invalid', ...args], {cwd: path, encoding: 'utf8'}).trim();
}
async function startNode(node) {
  node.child = start(['--data-dir', node.dir, 'daemon', 'start']);
  node.card = await until(() => {
    try { node.mesh.call(['daemon', 'status']); return node.mesh.call(['node', 'show']); } catch { return null; }
  });
  return node;
}
async function makeNode(name) {
  const dir = join(root, name);
  mkdirSync(dir);
  const listener = createServer().listen(0, '127.0.0.1');
  await once(listener, 'listening');
  const port = listener.address().port;
  listener.close();
  await once(listener, 'close');
  const mesh = new Mesh(hm, dir);
  mesh.call(['init', '--name', name, '--port', String(port)]);
  return startNode({dir, mesh});
}
async function panel(node) {
  const child = start(['--json', '--data-dir', node.dir, 'panel', '--port', '0'], 'pipe');
  const lines = createInterface({input: child.stdout});
  let deadline;
  try {
    const [line] = await Promise.race([
      once(lines, 'line'),
      once(child, 'exit').then(() => { throw Error('Panel exited before startup'); }),
      new Promise((_, reject) => { deadline = setTimeout(() => reject(Error('Panel startup timeout')), 15000); }),
    ]);
    return JSON.parse(line);
  } finally { clearTimeout(deadline); lines.close(); }
}
function register(node, name, project) {
  const path = join(root, `${name}.toml`);
  writeFileSync(path, ['[agent]', `name="${name}"`, 'harness="acp"', 'project_id="project-review"',
    '[project]', `root=${JSON.stringify(project)}`, 'allowed_tools=["read","write","edit"]',
    'artifact_paths=["out/health.json"]', `git_executable=${JSON.stringify(git)}`,
    '[adapter]', `executable=${JSON.stringify(fixture)}`, ''].join('\n'));
  return node.mesh.call(['agent', 'register', '--config', path]);
}
const throughPanel = (page, path) => page.evaluate(async path => unwrap(await api(path)), path);

try {
  const originProject = join(root, 'origin-project');
  const originGit = repo(originProject);
  writeFileSync(join(originProject, 'code.txt'), 'reviewed source\n');
  originGit('add', '.'); originGit('commit', '-qm', 'reviewed commit');
  const pinned = originGit('rev-parse', 'HEAD');
  const workerProject = join(root, 'worker-project');
  execFileSync(git, ['clone', '-q', originProject, workerProject]);
  const workerGit = (...args) => execFileSync(git, ['-c', 'user.name=Fixture', '-c', 'user.email=test@example.invalid', ...args], {cwd: workerProject, encoding: 'utf8'}).trim();
  writeFileSync(join(workerProject, 'code.txt'), 'executor version\n');
  workerGit('add', '.'); workerGit('commit', '-qm', 'executor version');
  const workerHead = workerGit('rev-parse', 'HEAD');
  const origin = await makeNode('source'), worker = await makeNode('executor');
  register(origin, 'source-agent', originProject);
  const agent = register(worker, 'executor-agent', workerProject);
  const verified = worker.mesh.call(['agent', 'verify', agent.agent_id, '--allow-model', '--prompt', 'fixture:reply']);
  await until(() => worker.mesh.call(['task', 'show', verified.task_id]).status === 'SUCCEEDED');
  const pairing = origin.mesh.call(['pair', 'request', '--peer', worker.card.endpoint, '--expect-fingerprint', worker.card.fingerprint]);
  worker.mesh.call(['pair', 'approve', pairing.request_id, '--expect-fingerprint', origin.card.fingerprint]);
  origin.mesh.call(['pair', 'approve', pairing.request_id, '--expect-fingerprint', worker.card.fingerprint]);
  const panelInfo = await panel(origin);
  browser = await chromium.launch({headless: true, ...(process.env.HMESH_CHROME ? {executablePath: process.env.HMESH_CHROME} : {})});
  page = await browser.newPage({viewport: {width: 1280, height: 920}});
  const errors = [];
  page.on('pageerror', e => errors.push(e.message));
  await page.goto(panelInfo.url);
  await page.locator('#local-name').filter({hasText: 'source'}).waitFor({state:'attached'});

  stage = 'project-first discovery and mismatch';
  await page.locator('[data-view="tasks"]').click();
  await page.locator('#new-task').click();
  await page.locator('#discover-project').selectOption('project-review');
  const target = page.locator('.discover-target').filter({hasText: 'executor-agent'});
  await target.getByRole('button', {name: '选用'}).click();
  await page.waitForFunction(id=>document.querySelector('#compose-agent').value===id, agent.agent_id);
  await page.locator('#git-comparison').filter({hasText:pinned}).waitFor();
  await page.locator('#compose-goal').fill('fixture:create');
  await page.locator('#compose-check').click();
  await page.locator('#git-comparison').getByText(/两端提交不同/).waitFor();
  assert((await page.locator('#git-comparison').textContent()).includes(workerHead), 'Executor commit missing');
  await page.locator('#git-mode').selectOption('current');
  await page.locator('#compose-submit').click();
  await page.locator('#confirm-description').filter({hasText:workerHead}).waitFor();
  await page.locator('#confirm-cancel').click();
  assert(origin.mesh.call(['task','list']).submitted.length === 0, 'Version review reserved a task');
  await page.locator('#close-compose').click();

  stage = 'convert a remote session and preserve the contract';
  const session = origin.mesh.call(['session', 'open', '--agent', agent.agent_id, '--peer', worker.card.node_id, '--message', 'fixture:reply']);
  await page.locator('[data-view="sessions"]').click();
  await page.locator('.session-row').filter({hasText: 'fixture:reply'}).click();
  await page.locator('#conversation-stream').getByText(/build is green/).waitFor();
  worker.mesh.call(['peers', 'access', origin.card.node_id, 'confirm']);
  await page.getByRole('button', {name: '作为任务执行'}).click();
  assert(await page.locator('#compose-peer').isDisabled(), 'Session did not lock the execution target');
  await page.locator('#compose-goal').fill('fixture:create');
  await page.waitForFunction(id=>document.querySelector('#compose-agent').value===id, agent.agent_id);
  await page.locator('#git-comparison').filter({hasText:pinned}).waitFor();
  await page.locator('#git-mode').selectOption('pinned');
  await page.locator('#git-commit').fill(pinned);
  await page.locator('#compose-contract summary').click();
  await page.locator('#contract-artifact').fill('out/health.json');
  await page.locator('#contract-expected').fill('{"status":"ok"}');
  await page.locator('#compose-submit').click();
  await page.locator('#confirm-description').filter({hasText:pinned}).waitFor();
  await page.locator('#confirm-accept').click();
  await page.locator('#task-detail').getByText('约定的验收条件').waitFor();
  const id = await page.locator('#task-detail .detail-id').textContent();
  const task = worker.mesh.call(['task', 'show', id]);
  assert(task.status === 'PENDING_APPROVAL' && task.request.source_session_id === session.session_id, 'Task lost approval or source session');
  assert(task.request.origin_git.commit === pinned && task.request.git_version.commit === pinned, 'Version contract was lost');
  assert(task.request.acceptance_contract.expected.status === 'ok', 'Acceptance was lost');
  await page.locator('[data-close="task-dialog"]').click();

  stage = 'retain last confirmed status during outage';
  await page.locator('[data-view="inbox"]').click();
  await page.locator('[data-inbox-filter="execution"]').click();
  const row = page.locator(`[data-key="task:${id}"]`);
  await row.getByText('等待执行端审批').waitFor();
  worker.mesh.call(['daemon', 'stop']);
  await until(() => worker.child.exitCode !== null);
  await page.locator('#inbox-refresh').click();
  await row.getByText(/当前读取失败/).waitFor();
  assert(await row.getByText('等待执行端审批').count() === 1, 'Read failure changed the executor result');
  await startNode(worker);
  worker.mesh.call(['task', 'approve', id]);
  let latest;
  try {
    await until(() => {
      latest = worker.mesh.call(['task', 'show', id]);
      if (['FAILED','UNKNOWN','CANCELLED','EXPIRED','REJECTED'].includes(latest.status)) {
        throw Error(`${latest.status}: ${latest.error?.code || ''}: ${latest.error?.message || ''}`);
      }
      return latest.status === 'SUCCEEDED';
    }, 30, 'Pinned task did not succeed');
  } catch (error) {
    const events = worker.mesh.call(['task','events',id]);
    const phases = (events.events || []).map(event=>[event.event_type,event.payload?.phase,event.payload?.status]);
    throw Error(`Pinned task ${latest?.status || 'unknown'}: ${error.message} (${JSON.stringify(phases)})`);
  }
  assert(workerGit('rev-parse', 'HEAD') === workerHead && readFileSync(join(workerProject, 'code.txt'), 'utf8') === 'executor version\n', 'Pinned run changed original checkout');
  assert(worker.mesh.call(['task', 'list']).executions.filter(task => task.task_id === id).length === 1, 'Recovery duplicated the task');

  stage = 'read result, patch, and verify from original session after reload';
  await page.reload();
  await page.locator('.session-row').filter({hasText: 'fixture:reply'}).click();
  await page.locator('#session-work').getByRole('button', {name: 'fixture:create'}).click();
  await page.locator('#task-detail').getByText('执行端实际提交').waitFor();
  await page.locator('#task-detail').getByRole('button', {name: '查看 Agent 执行报告'}).click();
  await page.locator('#task-detail').getByText('Created out/health.json.').waitFor();
  await page.locator('#task-detail').getByRole('button', {name: '查看修改 diff'}).click();
  await page.locator('.workspace-diff').getByText(/diff --git/).waitFor();
  const workspace = await throughPanel(page, `/tasks/${id}/workspace?after=0`);
  assert(workspace.base_commit === pinned && !workspace.root && !workspace.path && !workspace.git, 'Patch metadata has wrong commit or exposes executor paths');
  const rejected = await page.evaluate(async id => {
    try { await api(`/tasks/${id}/verify`, {method:'POST', body:JSON.stringify({artifact:'out/health.json',expected:{harness:'fixture'}})}); return false; }
    catch (e) { return e.code === 'INVALID_INPUT'; }
  }, id);
  assert(rejected, 'A different assertion was accepted for the contracted artifact');
  await page.locator('#task-detail').getByRole('button', {name: '核验工件'}).click();
  await page.locator('#acceptance-state').getByText('核验通过').waitFor();
  assert(!errors.length, `Panel errors: ${errors.join(' | ')}`);
  console.log(JSON.stringify({ok:true, stages:['project-first', 'version-mismatch', 'session-link', 'approval', 'outage-reconnect', 'pinned-worktree', 'report-diff-verification']}));
} catch (error) {
  console.error(`Project work failed at ${stage}: ${error.message}`);
  if(page&&stage.includes('mismatch')) {
    for(const id of ['git-comparison','compose-preflight','compose-error']) console.error(id, await page.locator('#'+id).textContent().catch(()=>''));
  }
  process.exitCode = 1;
} finally {
  await browser?.close();
  for (const child of children.reverse()) {
    if (child.exitCode === null && child.signalCode === null) {
      child.kill('SIGTERM');
      await Promise.race([once(child, 'exit'), new Promise(resolve => setTimeout(resolve, 2500).unref())]);
    }
  }
  rmSync(root, {recursive: true, force: true});
}
