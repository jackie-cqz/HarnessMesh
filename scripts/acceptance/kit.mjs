// Copy a credential-free per-run Windows worker kit. Transfer the entire directory unchanged.
import { copyFileSync, existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { assert, fileHash, flags, readJson, validatePlan } from './common.mjs';
export function makeKit(runDir, output) {
  const planFile = join(resolve(runDir), 'plan.json'); const p = validatePlan(readJson(planFile));
  output = resolve(output); assert(!existsSync(output), 'Worker kit output must be a new directory');
  mkdirSync(output, { recursive: true, mode: 0o700 });
  const here = dirname(fileURLToPath(import.meta.url));
  for (const name of ['worker.mjs', 'checks.mjs', 'common.mjs', 'start-worker.ps1']) copyFileSync(join(here, name), join(output, name));
  copyFileSync(planFile, join(output, 'plan.json'));
  writeFileSync(join(output, 'README.txt'), `HarnessMesh fixed-action acceptance worker\nRun ID: ${p.run_id}\nPlan SHA-256: ${fileHash(planFile)}\n\nNo credentials are included. Review the scripts before running.\nFrom PowerShell, provide your Windows repository and the registered tailnet-reverse project root:\n\npwsh -File .\\start-worker.ps1 -Repo C:\\path\\HarnessMesh -ProjectRoot C:\\registered\\tailnet-reverse\n\nThe worker only binds loopback and is exposed to the exact Mac node through HarnessMesh mTLS. Keep it running until the Mac coordinator completes; it revokes its service automatically after finish or lease expiry. No SSH, arbitrary commands or automatic policy changes. Preserve plan.json bytes unchanged.\n`);
  return { output, run_id: p.run_id, plan_sha256: fileHash(planFile) };
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try { const f = flags(process.argv.slice(2), ['--run-dir', '--out']); assert(f['--run-dir'] && f['--out'], 'Usage: kit.mjs --run-dir RUN --out NEW_DIR'); console.log(JSON.stringify(makeKit(f['--run-dir'], f['--out']), null, 2)); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
}
