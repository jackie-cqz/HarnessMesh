import { test } from 'node:test';
import assert from 'node:assert/strict';
import { agentConfig, agentName, selectAgents, configDigest } from './tailnet-config.mjs';

// The shape `hmesh --json agent catalog` prints (fields the script reads).
const row = (kind, command, setup_name = kind) => ({ kind, command, setup_name });
const catalog = { agents: [row('opencode', 'opencode'), row('claude-code', 'claude-agent-acp'), row('codex', 'codex-acp'),
  row('pi', 'pi-acp'), row('dsh', 'dsh', 'dsh-acp'), row('acp', '')] };

const detected = [
  { harness: 'opencode', command: 'opencode', executable: '/usr/local/bin/opencode' },
  { harness: 'claude-code', command: 'claude-agent-acp', executable: '/usr/local/bin/claude-agent-acp' },
  { harness: 'dsh', command: 'dsh', executable: '/usr/local/bin/dsh' },
];

test('every detected agent is registered unless --agents narrows the list', () => {
  assert.deepEqual(selectAgents(detected, undefined, catalog).map(a => a.harness), ['opencode', 'claude-code', 'dsh']);
  assert.deepEqual(selectAgents(detected, 'claude-code', catalog).map(a => a.harness), ['claude-code']);
});

test('asking for an agent that is not installed fails instead of installing it', () => {
  assert.throws(() => selectAgents(detected, 'codex', catalog), /does not install agents/);
  assert.throws(() => selectAgents(detected, 'dsh-tui', catalog), /Unknown agent type/);
});

test('configuration names the node and agent type and quotes paths', () => {
  const config = agentConfig({ node: 'mac-a', harness: 'claude-code', executable: '/opt/a "b"/claude-agent-acp', project: '/tmp/p', catalog });
  assert.match(config, /^name = "mac-a-claude-code"$/m);
  assert.match(config, /^harness = "claude-code"$/m);
  assert.match(config, /^executable = "\/opt\/a \\"b\\"\/claude-agent-acp"$/m);
  assert.doesNotMatch(config, /credential|models_file|provider/);
  assert.equal(configDigest(config), configDigest(config));
  assert.notEqual(configDigest(config), configDigest(config.replace('/tmp/p', '/tmp/q')));
  const dsh = agentConfig({ node: 'mac-a', ...selectAgents(detected, 'dsh', catalog)[0], project: '/tmp/p', catalog });
  assert.match(dsh, /^name = "mac-a-dsh-acp"$/m);
  assert.match(dsh, /^harness = "dsh"$/m);
});

test('an agent added to the catalog is registered like the others', () => {
  const later = { agents: [...catalog.agents, row('example-agent', 'example-acp')] };
  const found = [...detected, { harness: 'example-agent', command: 'example-acp', executable: '/usr/local/bin/example-acp' }];
  assert.deepEqual(selectAgents(found, 'example-agent', later).map(a => a.harness), ['example-agent']);
  assert.match(agentConfig({ node: 'mac-a', ...selectAgents(found, 'example-agent', later)[0], project: '/tmp/p', catalog: later }), /^name = "mac-a-example-agent"$/m);
  // Not in this hmesh's catalog: refused, not guessed.
  assert.throws(() => selectAgents(found, 'example-agent', catalog), /Unknown agent type/);
  assert.throws(() => agentName('mac-a', 'acp', catalog), /Unsupported agent type/);
  // Forgetting the catalog says so instead of failing on undefined.
  assert.throws(() => agentConfig({ node: 'mac-a', harness: 'dsh', executable: '/x', project: '/tmp/p' }), /Pass the agent catalog/);
});
