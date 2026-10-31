// Prepare a fresh fixed manifest and credential-free Windows kit; never starts models.
import {copyFileSync, existsSync, mkdirSync, writeFileSync} from 'node:fs';
import {randomUUID} from 'node:crypto';
import {dirname, join, resolve} from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';
import {assert, atomicJson, fileHash, flags, hash, keys, Mesh, readJson, UUID} from '../acceptance/common.mjs';

export function prepare(configFile, output) {
  const c = readJson(configFile);
  keys(c, ['binary','node_dir','project_root','pi_config','mcp_binary','windows_node_id','windows_agent_id','windows_project_id']);
  for (const name of ['binary','node_dir','project_root','pi_config','mcp_binary']) {
    assert(typeof c[name] === 'string' && existsSync(c[name]), `Missing local ${name}`);
    c[name] = resolve(c[name]);
  }
  assert(UUID.test(c.windows_node_id) && UUID.test(c.windows_agent_id), 'Invalid Windows identity');
  assert(typeof c.windows_project_id === 'string' && c.windows_project_id.length > 0, 'Missing registered Windows project');
  const mac = new Mesh(c.binary,c.node_dir).call(['node','show']);
  const dir = resolve(output);
  assert(!existsSync(dir), 'Use a new run directory; never reopen an uncertain mutation');
  mkdirSync(dir,{recursive:true,mode:0o700});
  const run = randomUUID(), base = `.acceptance/${run}`;
  const jobs = ['recovery','mcp'].map(name => {
    const expected = {nonce:randomUUID(),source:'windows-dsh',stage:'complete'};
    const artifact = `${base}/${name}-child.json`;
    const goal = name === 'recovery'
      ? `Use write/read/edit only in this registered project. First write ${artifact} with {"stage":"started"}. Then write ${base}/recovery-note.md with a useful original 1200-word explanation of task deduplication and uncertain distributed outcomes. Finally overwrite ${artifact} with exactly ${JSON.stringify(expected)} and read it back. No shell, networking or files outside this per-run subdirectory. Keep the final response brief.`
      : `Use write then read to create ${artifact} containing exactly ${JSON.stringify(expected)}. No shell, networking or files outside this per-run subdirectory. Keep the final response brief.`;
    return {id:randomUUID(),name,goal,goal_sha256:hash(goal),artifact,expected};
  });
  const manifest = {schema:1,run_id:run,mac_node_id:mac.node_id,windows_node_id:c.windows_node_id,agent_id:c.windows_agent_id,project_id:c.windows_project_id,service_id:randomUUID(),crash_id:randomUUID(),lease_seconds:3600,jobs};
  atomicJson(join(dir,'manifest.json'),manifest);
  atomicJson(join(dir,'mac-owner.json'),{binary:c.binary,node_dir:c.node_dir,project_root:c.project_root,pi_config:c.pi_config,mcp_binary:c.mcp_binary,run_dir:dir});
  const kit = join(dir,'windows-kit'), here = dirname(fileURLToPath(import.meta.url));
  mkdirSync(join(kit,'scripts','reliability'),{recursive:true,mode:0o700});
  mkdirSync(join(kit,'scripts','acceptance'),{recursive:true,mode:0o700});
  copyFileSync(join(dir,'manifest.json'),join(kit,'manifest.json'));
  for (const name of ['guardian.mjs','supervisor.mjs']) copyFileSync(join(here,name),join(kit,'scripts','reliability',name));
  copyFileSync(join(here,'../acceptance/common.mjs'),join(kit,'scripts','acceptance','common.mjs'));
  copyFileSync(join(here,'start-windows.ps1'),join(kit,'start-windows.ps1'));
  writeFileSync(join(kit,'README.txt'),`HarnessMesh reliability run ${run}\nManifest SHA-256: ${fileHash(join(kit,'manifest.json'))}\nOnly two exact manifest tasks are approved. Review their prompts and scripts.\nRun: pwsh -File .\\start-windows.ps1 -Repo 'C:\\path\\HarnessMesh' -ProjectRoot 'C:\\registered\\tailnet-reverse'\nThe helper restarts its own process once for the intentional crash probe. No policy change or general remote command API. No credentials or Mac owner config are included.\n`);
  return {run_id:run,run_dir:dir,windows_kit:kit,manifest_sha256:fileHash(join(dir,'manifest.json'))};
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  try { const f=flags(process.argv.slice(2),['--config','--out']); assert(f['--config'] && f['--out'],'Expected --config OWNER_JSON --out NEW_RUN_DIR'); console.log(JSON.stringify(prepare(f['--config'],f['--out']),null,2)); }
  catch (e) { console.error(e.message); process.exitCode=1; }
}
