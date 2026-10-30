import { test } from 'node:test';
import assert from 'node:assert/strict';
import { realpathSync, mkdtempSync, mkdirSync, writeFileSync, symlinkSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { resolvePi, credentialSource, configName } from './tailnet-config.mjs';
test('managed launcher resolves JS without executing the launcher', () => {
  const root = mkdtempSync(join(tmpdir(), 'hmesh-pi-'));
  try {
    mkdirSync(join(root, 'bin')); mkdirSync(join(root, 'install/releases/1.4.0/node_modules/.bin'), { recursive: true });
    writeFileSync(join(root, 'bin/pi'), '#!/bin/sh\nexit 99');
    writeFileSync(join(root, 'install/current-version'), '1.4.0\n');
    writeFileSync(join(root, 'install/releases/1.4.0/node_modules/cli.js'), '');
    symlinkSync('../cli.js', join(root, 'install/releases/1.4.0/node_modules/.bin/pi'));
    assert.equal(resolvePi(join(root, 'bin/pi')), realpathSync(join(root, 'install/releases/1.4.0/node_modules/cli.js')));
  } finally { rmSync(root, { recursive: true }); }
});
test('empty auth falls back to literal models key; explicit invalid auth fails', () => {
  const root = mkdtempSync(join(tmpdir(), 'hmesh-auth-'));
  try {
    const auth = join(root, 'auth.json'), models = join(root, 'models.json');
    writeFileSync(auth, '{}'); writeFileSync(models, JSON.stringify({providers:{deepseek:{apiKey:'fixture-secret'}}}));
    assert.equal(credentialSource({auth,models}), models);
    assert.equal(credentialSource({auth,models,envKey:'fixture-env'}), undefined);
    assert.throws(()=>credentialSource({explicit:auth,auth,models}), /Explicit credential/);
    for (const apiKey of ['!echo secret', 'DEEPSEEK_API_KEY']) {
      writeFileSync(models, JSON.stringify({providers:{deepseek:{apiKey}}}));
      assert.throws(()=>credentialSource({auth,models}), /Configure/);
    }
  } finally { rmSync(root, { recursive: true }); }
});
test('registration name is stable and changes with credential reference', () => {
  assert.equal(configName('config-a'), configName('config-a'));
  assert.notEqual(configName('config-a'), configName('config-b'));
});
test('JS entry paths and changed configuration references remain stable', () => {
  const root = mkdtempSync(join(tmpdir(), 'hmesh-entry-'));
  try {
    const file = join(root, 'cli.js'); writeFileSync(file, '');
    assert.equal(resolvePi(file), realpathSync(file));
    assert.equal(configName('credential_source = "a"'), configName('credential_source = "a"'));
    assert.notEqual(configName('credential_source = "a"'), configName('credential_source = "b"'));
  } finally { rmSync(root, { recursive: true }); }
});
