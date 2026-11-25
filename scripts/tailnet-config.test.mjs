import { test } from 'node:test';
import assert from 'node:assert/strict';
import { agentConfig, selectAgents, configDigest } from './tailnet-config.mjs';

const detected = [
  { harness: 'opencode', command: 'opencode', executable: '/usr/local/bin/opencode' },
  { harness: 'claude-code', command: 'claude-agent-acp', executable: '/usr/local/bin/claude-agent-acp' },
  { harness: 'dsh', command: 'dsh', executable: '/usr/local/bin/dsh' },
];

test('every detected agent is registered unless --agents narrows the list', () => {
  assert.deepEqual(selectAgents(detected).map(a => a.harness), ['opencode', 'claude-code', 'dsh']);
  assert.deepEqual(selectAgents(detected, 'claude-code').map(a => a.harness), ['claude-code']);
});

test('asking for an agent that is not installed fails instead of installing it', () => {
  assert.throws(() => selectAgents(detected, 'codex'), /does not install agents/);
  assert.throws(() => selectAgents(detected, 'dsh-tui'), /Unknown agent type/);
});

test('configuration names the node and agent type and quotes paths', () => {
  const config = agentConfig({ node: 'mac-a', harness: 'claude-code', executable: '/opt/a "b"/claude-agent-acp', project: '/tmp/p' });
  assert.match(config, /^name = "mac-a-claude-code"$/m);
  assert.match(config, /^harness = "claude-code"$/m);
  assert.match(config, /^executable = "\/opt\/a \\"b\\"\/claude-agent-acp"$/m);
  assert.doesNotMatch(config, /credential|models_file|provider/);
  assert.equal(configDigest(config), configDigest(config));
  assert.notEqual(configDigest(config), configDigest(config.replace('/tmp/p', '/tmp/q')));
  const dsh = agentConfig({ node: 'mac-a', ...selectAgents(detected, 'dsh')[0], project: '/tmp/p' });
  assert.match(dsh, /^name = "mac-a-dsh-acp"$/m);
  assert.match(dsh, /^harness = "dsh"$/m);
});
