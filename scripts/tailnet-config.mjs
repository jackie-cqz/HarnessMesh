import { createHash } from 'node:crypto';

const HARNESSES = ['opencode', 'claude-code', 'codex', 'pi'];

/** Agent configuration (TOML) for one installed ACP agent command. */
export function agentConfig({ node, harness, executable, project, projectId = 'tailnet-demo' }) {
  if (!HARNESSES.includes(harness)) throw new Error(`Unsupported agent type: ${harness}`);
  const toml = value => JSON.stringify(value);
  return ['[agent]', `name = ${toml(`${node}-${harness}`)}`, `harness = ${toml(harness)}`, `project_id = ${toml(projectId)}`,
    '[project]', `root = ${toml(project)}`, 'allowed_tools = ["read", "write", "edit"]',
    '[adapter]', `executable = ${toml(executable)}`, ''].join('\n');
}

/** Pick detected agents, optionally limited to an explicit comma-separated list. */
export function selectAgents(detected, only) {
  const wanted = only ? only.split(',').map(s => s.trim()).filter(Boolean) : null;
  for (const name of wanted ?? []) if (!HARNESSES.includes(name)) throw new Error(`Unknown agent type in --agents: ${name}`);
  const chosen = detected.filter(agent => !wanted || wanted.includes(agent.harness));
  const missing = (wanted ?? []).filter(name => !chosen.some(agent => agent.harness === name));
  if (missing.length) throw new Error(`Not installed on this computer: ${missing.join(', ')} (HarnessMesh does not install agents)`);
  return chosen;
}

/** Stable short digest so an edited configuration is never silently reused. */
export function configDigest(config) {
  return createHash('sha256').update(config).digest('hex').slice(0, 12);
}
