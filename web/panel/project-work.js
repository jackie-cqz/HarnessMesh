'use strict';

const projectWork = { generation: 0, originGeneration: 0, nodes: [], origin: null, executor: null, currentCommit: null, directoryRead: null, originRead: null };
const hasProjectWork = () => state.runtime?.capabilities?.project_collaboration === true;
const platformLabel = os => ({ macos: 'macOS', windows: 'Windows', linux: 'Linux' })[os] || os || '平台未知';
const projectDiagnostics = {
  LEGACY_DSH_UNSUPPORTED: '旧 DSH 登记没有 ACP 接口，请在执行端接入支持 ACP 的 Agent。',
  LEGACY_PI_ADAPTER: '旧 Pi 登记使用原生协议，请在执行端改用 pi-acp 并重新登记。',
  PROJECT_UNAVAILABLE: '执行端访问不到登记的项目目录。',
  EXECUTABLE_UNAVAILABLE: '执行端缺少 ACP 可执行程序或对应运行时，请检查登记配置。',
  ACP_PROBE_REQUIRED: '执行端需要重新登记或更新 Agent，以检查 ACP 握手。',
  NO_REGISTERED_PROJECTS: '这台电脑还没有登记项目。',
};
const projectDiagnostic = item => projectDiagnostics[item.code] || item.action || item.code;


async function loadProjectDirectory() {
  const generation = ++projectWork.generation;
  const projectSelect = $('#discover-project');
  projectSelect.replaceChildren();
  $('#discover-targets').replaceChildren();
  $('#discover-note').textContent = '正在读取项目与执行端…';
  projectWork.nodes = [];
  const local = data('node', {}).node;
  if (!local) return;
  const peers = data('peers').filter(peer => peer.status === 'trusted').map(peer => peer.card);
  const queue = [local, ...peers];
  const results = new Map();
  const read = async () => {
    while (queue.length && generation === projectWork.generation) {
      const node = queue.shift();
      const localNode = node.node_id === local.node_id;
      try {
        const info = unwrap(await api('/discovery' + (localNode ? '' : `?peer_id=${node.node_id}`)));
        results.set(node.node_id, { ...info, name: node.display_name, local: localNode });
      } catch (error) {
        results.set(node.node_id, { node_id: node.node_id, name: node.display_name, issue: { action: problem(error) }, projects: [] });
      }
    }
  };
  await Promise.all([read(), read(), read()]);
  if (generation !== projectWork.generation || !$('#compose-dialog').open) return;
  projectWork.nodes = [local, ...peers].map(node => results.get(node.node_id)).filter(Boolean);
  const ids = [...new Set(projectWork.nodes.flatMap(node => node.projects.map(project => project.project_id)))].sort();
  option(projectSelect, '', '选择项目');
  for (const id of ids) option(projectSelect, id, id);
  if (ids.includes($('#compose-project').value)) projectSelect.value = $('#compose-project').value;
  $('#discover-note').textContent = projectWork.nodes.filter(node => node.issue).map(node => `${node.name}：${projectDiagnostic(node.issue)}`).join(' · ');
  if (!ids.length && !$('#discover-note').textContent) $('#discover-note').textContent = '接入一个项目后即可选择执行端。';
  renderProjectTargets();
  await (projectWork.originRead = readOriginGit());
}

function renderProjectTargets() {
  const selected = $('#discover-project').value;
  const rows = [];
  for (const node of projectWork.nodes) {
    const project = node.projects.find(item => item.project_id === selected);
    for (const agent of project?.agents || []) {
      const row = el('div', 'discover-target');
      const name = el('div');
      name.append(el('strong', '', agent.name), el('div', 'muted', `${node.name} · ${platformLabel(node.platform)} · ${agent.harness}`));
      const status = el('div', 'discover-status');
      status.append(el('span', `badge ${agent.task_ready ? 'good' : 'warn'}`, agent.task_ready ? '可执行' : '暂不可派发'));
      status.append(el('span', 'muted', `${agent.verified ? '已验证' : '未验证'} · ${agent.requires_approval ? '需执行端批准' : '按执行端策略启动'}`));
      const select = button('选用', 'button secondary', async () => {
        if (composerBusy || pendingDraft || composerDraft.source_session_id) return;
        $('#compose-peer').value = node.node_id;
        await loadComposerAgents(node.node_id, agent.agent_id, selected);
        $('#compose-discovery').open = false;
        $('#compose-goal').focus();
      });
      select.disabled = !agent.task_ready || !!pendingDraft || !!composerDraft?.source_session_id;
      row.append(name, status, select);
      for (const check of (agent.checks || []).filter(item => !item.passed && item.action)) {
        row.append(el('p', 'form-note target-diagnostic', projectDiagnostic(check)));
      }
      if (!agent.task_ready && !agent.verified && agent.session_ready) row.append(el('p', 'form-note target-diagnostic', '可打开会话。接收远端任务前，执行端需要验证 Agent 能完成任务。'));
      rows.push(row);
    }
  }
  $('#discover-targets').replaceChildren(...rows);
}

async function readOriginGit() {
  const generation = ++projectWork.originGeneration;
  projectWork.origin = null;
  renderGitComparison();
  const project = $('#compose-project').value;
  const local = projectWork.nodes.find(node => node.local);
  const registered = local?.projects.find(item => item.project_id === project)?.agents || [];
  const agent = registered.find(item => item.agent_id === $('#compose-agent').value) || registered[0];
  if (agent) {
    try {
      const version = unwrap(await api(`/agents/${agent.agent_id}/git`));
      if (generation !== projectWork.originGeneration) return;
      projectWork.origin = version.current;
    } catch {
      if (generation !== projectWork.originGeneration) return;
    }
  }
  if (generation !== projectWork.originGeneration) return;
  if ($('#git-mode').value === 'pinned' && !$('#git-commit').value && projectWork.origin) {
    $('#git-commit').value = projectWork.origin.commit;
  }
  renderGitComparison();
}

function describeGit(version) {
  if (!version?.commit) return '未读取到 Git 提交';
  return `${version.branch || '分离 HEAD'} · ${version.commit} · ${version.dirty ? '有未提交改动' : '工作区干净'}`;
}

function renderGitComparison() {
  if (!hasProjectWork()) return;
  const origin = projectWork.origin;
  const executor = projectWork.executor;
  const mismatch = origin?.commit && executor?.commit && origin.commit !== executor.commit;
  if (mismatch && !$('#git-mode').value) $('#compose-git').open = true;
  const box = $('#git-comparison');
  box.replaceChildren(
    meta([['发起端', describeGit(origin)], ['执行端', describeGit(executor)]]),
    el('p', mismatch ? 'alert' : 'form-note', mismatch ? '两端提交不同。请选择使用执行端当前检出，或在独立目录检出指定提交。' : '检查执行条件会读取执行端版本。指定提交须已存在于执行端。')
  );
  $('#git-commit-label').hidden = $('#git-mode').value !== 'pinned';
}

function resetProjectWork(draft) {
  ++projectWork.originGeneration;
  projectWork.origin = draft?.origin_git || null;
  projectWork.executor = null;
  projectWork.currentCommit = null;
  const enabled = hasProjectWork();
  $('#compose-discovery').hidden = !enabled || !!draft?.source_session_id || !!pendingDraft;
  $('#compose-contract').hidden = !enabled;
  $('#compose-git').hidden = !enabled;
  $('#compose-session').hidden = !draft?.source_session_id;
  $('#compose-session').textContent = draft?.source_session_id ? `来自会话 ${short(draft.source_session_id)} · 沿用会话的项目和执行端` : '';
  $('#git-mode').value = draft?.git_version?.mode || '';
  $('#git-commit').value = draft?.git_version?.commit || '';
  $('#contract-artifact').value = draft?.acceptance_contract?.artifact || '';
  $('#contract-expected').value = draft?.acceptance_contract ? JSON.stringify(draft.acceptance_contract.expected, null, 2) : '';
  $('#compose-contract').open = !!draft?.acceptance_contract;
  for (const id of ['git-mode', 'git-commit', 'contract-artifact', 'contract-expected']) $('#' + id).disabled = !!pendingDraft;
  renderGitComparison();
  projectWork.directoryRead = enabled && !pendingDraft ? loadProjectDirectory() : null;
}

function changedProjectTarget() {
  if (!hasProjectWork()) return;
  projectWork.executor = null;
  projectWork.currentCommit = null;
  if (!pendingDraft) {
    $('#git-mode').value = '';
    $('#git-commit').value = '';
  }
  if (Array.from($('#discover-project').options).some(item => item.value === $('#compose-project').value)) {
    $('#discover-project').value = $('#compose-project').value;
    renderProjectTargets();
  }
  projectWork.originRead = readOriginGit();
}

async function settleProjectWork() {
  // Directory and target changes can both trigger an origin observation.
  await projectWork.directoryRead;
  let read;
  do { read = projectWork.originRead; await read; } while (read !== projectWork.originRead);
}

function readCollaborationDraft() {
  if (!hasProjectWork()) return {};
  const fields = {};
  if (composerDraft.source_session_id) fields.source_session_id = composerDraft.source_session_id;
  const target = projectWork.nodes.find(node => node.node_id === $('#compose-peer').value);
  if (projectWork.origin?.commit && (target?.supported || target?.local || composerDraft.source_session_id || $('#git-mode').value)) fields.origin_git = projectWork.origin;
  const mode = $('#git-mode').value;
  if (mode === 'pinned') fields.git_version = { mode, commit: $('#git-commit').value.trim() };
  if (mode === 'current' && projectWork.currentCommit) fields.git_version = { mode, expected_commit: projectWork.currentCommit };
  const artifact = $('#contract-artifact').value.trim();
  const expected = $('#contract-expected').value.trim();
  if (artifact || expected) {
    let value;
    try { value = JSON.parse(expected); } catch { throw Error('请输入要核验的 JSON 对象。'); }
    if (!artifact || !value || Array.isArray(value) || typeof value !== 'object' || !Object.keys(value).length) {
      throw Error('JSON 验收需要一个工件路径和至少一个预期字段。');
    }
    fields.acceptance_contract = { artifact, expected: value };
  }
  return fields;
}

function startTaskFromSession(session) {
  const message = [...conv.events].reverse().find(event => event.kind === 'user_message')?.payload?.text;
  openComposer({ source_session_id: session.session_id, peer_id: session.node_id,
    agent_id: session.agent_id, project_id: session.project_id, goal: message || session.title || '' });
}

function renderSessionWork() {
  const box = $('#session-work');
  const session = conv.record;
  if (!session || !hasProjectWork() || state.view !== 'sessions') {
    box.hidden = true;
    return;
  }
  const tasks = allTasks().filter(task => (task.request?.source_session_id || task._source_session_id) === session.session_id);
  box.hidden = !tasks.length;
  const rows = tasks.map(task => {
    const row = el('div', 'session-work-row');
    row.append(button(task.request?.goal?.split('\n')[0] || '查看原任务', 'text-button', () => openTask(task)), badge(task.status));
    if (task.status === 'SUCCEEDED') row.append(acceptanceBadge(task));
    return row;
  });
  box.replaceChildren(el('h3', '', '会话任务'), ...rows);
}

$('#discover-project').addEventListener('change', renderProjectTargets);
$('#git-mode').addEventListener('change', () => {
  projectWork.currentCommit = $('#git-mode').value === 'current' ? projectWork.executor?.commit || null : null;
  if ($('#git-mode').value === 'pinned' && !$('#git-commit').value && projectWork.origin) $('#git-commit').value = projectWork.origin.commit;
  renderGitComparison();
});
