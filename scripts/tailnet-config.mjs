import { createHash } from 'node:crypto';

// `catalog` is what `hmesh --json agent catalog` prints: the agents this
// hmesh knows, each with the command it looks for and the name `hmesh setup`
// gives it. Only agents with a command of their own are registered here.
function known(catalog, harness) {
  return catalog.agents.find(agent => agent.kind === harness && agent.command);
}

/** `<node>-<kind>`, as `hmesh setup` names agents (DSH's ACP agent is `-dsh-acp`). */
export function agentName(node, harness, catalog) {
  const agent = known(catalog, harness);
  if (!agent) throw new Error(`Unsupported agent type: ${harness}`);
  return `${node}-${agent.setup_name}`;
}

/** Agent configuration (TOML) for one installed ACP agent command. */
export function agentConfig({ node, harness, executable, project, projectId = 'tailnet-demo', catalog }) {
  const toml = value => JSON.stringify(value);
  return ['[agent]', `name = ${toml(agentName(node, harness, catalog))}`, `harness = ${toml(harness)}`, `project_id = ${toml(projectId)}`,
    '[project]', `root = ${toml(project)}`, 'allowed_tools = ["read", "write", "edit"]',
    '[adapter]', `executable = ${toml(executable)}`, ''].join('\n');
}

/** Pick detected agents, optionally limited to an explicit comma-separated list. */
export function selectAgents(detected, only, catalog) {
  const wanted = only ? only.split(',').map(s => s.trim()).filter(Boolean) : null;
  for (const name of wanted ?? []) if (!known(catalog, name)) throw new Error(`Unknown agent type in --agents: ${name}`);
  const chosen = detected.filter(agent => !wanted || wanted.includes(agent.harness));
  const missing = (wanted ?? []).filter(name => !chosen.some(agent => agent.harness === name));
  if (missing.length) throw new Error(`Not installed on this computer: ${missing.join(', ')} (HarnessMesh does not install agents)`);
  return chosen;
}

/** Stable short digest so an edited configuration is never silently reused. */
export function configDigest(config) {
  return createHash('sha256').update(config).digest('hex').slice(0, 12);
}
