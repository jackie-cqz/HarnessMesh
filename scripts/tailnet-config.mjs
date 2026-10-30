import { existsSync, readFileSync, realpathSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { createHash } from 'node:crypto';

export function resolvePi(path) {
  const executable = realpathSync(path);
  if (/\.(m?js|cjs)$/.test(executable)) return executable;
  // Resolve only the known managed installer layout; never execute shell text.
  const agent = dirname(dirname(executable));
  const versionFile = join(agent, 'install', 'current-version');
  if (existsSync(versionFile)) {
    const version = readFileSync(versionFile, 'utf8').trim();
    if (!/^[A-Za-z0-9_+-][A-Za-z0-9._+-]*$/.test(version)) throw new Error('Invalid managed Pi version');
    const entry = realpathSync(join(agent, 'install', 'releases', version, 'node_modules', '.bin', 'pi'));
    if (/\.(m?js|cjs)$/.test(entry)) return entry;
  }
  throw new Error('Pass --pi pointing to the actual Pi JavaScript entry file');
}

export function credentialSource({ explicit, auth, models, envKey }) {
  const usable = path => {
    const d = JSON.parse(readFileSync(path, 'utf8'));
    return d.deepseek?.type === 'api_key' && typeof d.deepseek.key === 'string' && d.deepseek.key.length > 0;
  };
  if (explicit) {
    if (!usable(explicit)) throw new Error('Explicit credential source must contain a DeepSeek API key');
    return explicit;
  }
  if (existsSync(auth) && usable(auth)) return auth;
  if (envKey) return undefined;
  const key = JSON.parse(readFileSync(models, 'utf8')).providers?.deepseek?.apiKey;
  if (typeof key === 'string' && key && !key.startsWith('!') && !/^[A-Z_][A-Z0-9_]*$/.test(key) && !/[\r\n\0]/.test(key)) return models;
  throw new Error('Configure a DeepSeek API key in auth.json, models.json or DEEPSEEK_API_KEY');
}

export function configName(config) {
  return `mac-pi-flash-${createHash('sha256').update(config).digest('hex').slice(0, 12)}`;
}
