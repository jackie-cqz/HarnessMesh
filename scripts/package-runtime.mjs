// Allowlisted native packaging. Never copies instance data or Harness credentials.
import{spawnSync}from'node:child_process';
import{mkdirSync,copyFileSync,writeFileSync,readFileSync,existsSync,readdirSync,chmodSync}from'node:fs';
import{join,resolve,basename}from'node:path';
import{createHash}from'node:crypto';
import{flags,assert}from'./acceptance/common.mjs';
const f=flags(process.argv.slice(2),['--out','--binary-dir']),out=resolve(f['--out']||'target/bundles'),bins=resolve(f['--binary-dir']||'target/release');
const win=process.platform==='win32',suffix=win?'.exe':'',tag=process.platform+'-'+process.arch;
const packageVersion=spawnSync(join(bins,'hmesh'+suffix),['--version'],{encoding:'utf8'});assert(packageVersion.status===0,'Build CLI first');const versionTag=packageVersion.stdout.trim().replace(/^hmesh\s+/, '');assert(/^[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?$/.test(versionTag),'Invalid binary version');
mkdirSync(out,{recursive:true});const stage=join(out,'harnessmesh-'+versionTag+'-'+tag);assert(!existsSync(stage),'Use a fresh output directory');mkdirSync(stage,{mode:0o700});
for(const name of ['hmesh','hmesh-mcp']){const source=join(bins,name+suffix);assert(existsSync(source),'Build CLI and MCP first');copyFileSync(source,join(stage,name+suffix));if(!win)chmodSync(join(stage,name),0o755);}
const version=spawnSync(join(stage,'hmesh'+suffix),['--version'],{encoding:'utf8'});assert(version.status===0,'Packaged CLI cannot execute');
const revision=spawnSync('git',['rev-parse','HEAD'],{encoding:'utf8'});assert(revision.status===0,'Missing source commit');const dirty=spawnSync('git',['diff','--quiet']).status!==0;
writeFileSync(join(stage,'README.txt'),'HarnessMesh native runtime\n\nMac: open start-mac.command. Windows: run start-windows.ps1. Linux: ./start.sh.\nEach launcher accepts a Node data directory as its first argument (Windows: -DataDir PATH), default ./data within this package. Empty dedicated directories initialize as local loopback Nodes. Existing instances retain their identity, policy and credentials. Open the full access link printed in the terminal; keep it private. Close through the panel or Ctrl+C. Only a Daemon started by this panel is stopped; already-running Daemons are preserved.\n\nNo Rust, npm or frontend build is required to run this CLI/panel. Harnesses, their Node runtimes, model credentials and Tailscale are configured separately on execution computers. No credentials are bundled. This is an unsigned native developer build. Login startup is opt-in via hmesh startup install; inspect hmesh startup plan first. Before changing binaries use hmesh upgrade prepare --bundle PACKAGE_DIRECTORY --backup-dir NEW_PRIVATE_BACKUP with the Daemon offline. There are no automatic updates. Use your OS/CPU package. Checksums detect file changes; they are not a publisher signature.\n\nTask submission requires approval. Artifact checks use the execution owner allowlist. Unknown submissions must be inspected/recovered using their original request ID. Do not create replacement tasks to retry side effects.\n\nSource: https://github.com/jackie-cqz/HarnessMesh\n');
writeFileSync(join(stage,'NOTICE.txt'),'HarnessMesh\nSource license identifier: Apache-2.0 (Cargo.toml).\nhttps://github.com/jackie-cqz/HarnessMesh\nThis development bundle contains no personal instance or Harness configuration.\n');
if(win){
 const launcher=[
  "param([string]$DataDir=(Join-Path $PSScriptRoot 'data'),[int]$Port=38900)",
  "$ErrorActionPreference='Stop'",
  "$manifest=Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'manifest.json')|ConvertFrom-Json",
  'foreach($item in $manifest.files.PSObject.Properties){',
  "  if($item.Name -notmatch '^[A-Za-z0-9._-]+$'){throw 'Unsupported bundle path'}",
  '  $stream=[System.IO.File]::OpenRead((Join-Path $PSScriptRoot $item.Name))',
  '  $sha=[System.Security.Cryptography.SHA256]::Create()',
  "  try{$actual=([System.BitConverter]::ToString($sha.ComputeHash($stream))).Replace('-','').ToLowerInvariant()}",
  '  finally{$sha.Dispose();$stream.Dispose()}',
  "  if($actual -ne $item.Value){throw ('Bundle checksum failed: '+$item.Name)}",
  '}',
  "& (Join-Path $PSScriptRoot 'hmesh.exe') --data-dir $DataDir panel --start-daemon --port $Port",
  'exit $LASTEXITCODE',''
 ].join('\n');writeFileSync(join(stage,'start-windows.ps1'),launcher);
}else{
 const launcher=['#!/bin/sh','set -eu','bundle_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)','cd "$bundle_dir"','if command -v shasum >/dev/null 2>&1; then shasum -a 256 -c SHA256SUMS; else sha256sum -c SHA256SUMS; fi','if [ "$#" -gt 0 ]; then mesh_data="$1"; else mesh_data="$bundle_dir/data"; fi','mesh_port=38900','if [ "$#" -gt 1 ]; then mesh_port="$2"; fi','exec "$bundle_dir/hmesh" --data-dir "$mesh_data" panel --start-daemon --port "$mesh_port"',''].join('\n');
 writeFileSync(join(stage,process.platform==='darwin'?'start-mac.command':'start.sh'),launcher,{mode:0o755});
}
const files=Object.fromEntries(readdirSync(stage).sort().map(name=>[name,createHash('sha256').update(readFileSync(join(stage,name))).digest('hex')]));
writeFileSync(join(stage,'manifest.json'),JSON.stringify({schema:1,database_schema:8,version:version.stdout.trim(),commit:revision.stdout.trim(),dirty,platform:process.platform,arch:process.arch,files},null,2)+'\n');
writeFileSync(join(stage,'SHA256SUMS'),[...Object.entries(files),['manifest.json',createHash('sha256').update(readFileSync(join(stage,'manifest.json'))).digest('hex')]].map(([name,hash])=>hash+'  '+name).join('\n')+'\n');
const archive=stage+(win?'.zip':'.tar.gz');
const zipped=win?spawnSync('powershell.exe',['-NoProfile','-NonInteractive','-File',resolve('scripts/package-windows.ps1'),'-Mode','create','-Source',stage,'-Destination',archive],{encoding:'utf8'}):spawnSync('tar',['-czf',archive,'-C',out,basename(stage)],{encoding:'utf8'});
assert(zipped.status===0,'Archive creation failed');console.log(JSON.stringify({archive,directory:stage,platform:tag,commit:revision.stdout.trim(),dirty,credential_files:0}));
