// Execute an extracted native package without Cargo/Node on the child's PATH.
import{spawn,spawnSync}from'node:child_process';
import{mkdtempSync,readdirSync,readFileSync,mkdirSync}from'node:fs';
import{tmpdir}from'node:os';
import{join,resolve}from'node:path';
import{createHash}from'node:crypto';
import{createInterface}from'node:readline';
import{once}from'node:events';
import{flags,assert}from'./acceptance/common.mjs';
const f=flags(process.argv.slice(2),['--archive','--archive-dir']);
if(!f['--archive']&&f['--archive-dir']){const dir=resolve(f['--archive-dir']),files=readdirSync(dir).filter(n=>n.endsWith('.zip')||n.endsWith('.tar.gz'));assert(files.length===1,'Expected one archive');f['--archive']=join(dir,files[0]);}
assert(f['--archive'],'Expected --archive FILE or --archive-dir DIR');
const root=mkdtempSync(join(tmpdir(),'hmesh-package-')),unpack=join(root,'unpacked');mkdirSync(unpack);
const win=process.platform==='win32';
const extracted=win?spawnSync('powershell.exe',['-NoProfile','-NonInteractive','-File',resolve('scripts/package-windows.ps1'),'-Mode','extract','-Source',resolve(f['--archive']),'-Destination',unpack],{encoding:'utf8'}):spawnSync('tar',['-xzf',resolve(f['--archive']),'-C',unpack],{encoding:'utf8'});
if(extracted.status!==0){
 // Extraction runs before any Node identity, credential or panel URL exists.
 // Preserve the tool's actual failure instead of guessing at a runtime defect.
 const bounded=value=>String(value||'').replace(/#access=[^\s"']+/g,'#access=[REDACTED]').slice(-4000);
 throw Error('Archive extraction failed: '+JSON.stringify({status:extracted.status,signal:extracted.signal,error_code:extracted.error?.code,stderr:bounded(extracted.stderr),stdout:bounded(extracted.stdout)}));
}
const dirs=readdirSync(unpack);assert(dirs.length===1,'Unexpected archive entries');const dir=join(unpack,dirs[0]),manifest=JSON.parse(readFileSync(join(dir,'manifest.json')));
for(const[name,digest]of Object.entries(manifest.files)){assert(/^[A-Za-z0-9._-]+$/.test(name),'Unexpected bundle filename');assert(createHash('sha256').update(readFileSync(join(dir,name))).digest('hex')===digest,'Bundle hash mismatch');}
assert(readdirSync(dir).length===Object.keys(manifest.files).length+2,'Unexpected runtime data in package');
const childEnv={...process.env};for(const key of Object.keys(childEnv))if(key.toLowerCase()==='path')delete childEnv[key];childEnv.PATH=win?join(process.env.SystemRoot||'C:\\Windows','System32'):'/usr/bin:/bin';
const binary=join(dir,'hmesh'+(win?'.exe':'')),version=spawnSync(binary,['--version'],{encoding:'utf8',env:childEnv});assert(version.status===0&&version.stdout.trim()===manifest.version,'Standalone CLI did not run');
const preflight=spawnSync(binary,['--json','upgrade','check','--bundle',dir],{encoding:'utf8',env:childEnv});assert(preflight.status===0&&JSON.parse(preflight.stdout).passed===true,'Packaged upgrade preflight failed');
// Keep failed startup actionable without publishing the panel capability URL.
const redact=text=>text.replace(/#access=[^\s"']+/g,'#access=[REDACTED]').slice(-6000);
async function checkStartup(command,args,dataDir,label){
let child,origin,headers;try{
 let output='';
 child=spawn(command,args,{stdio:['ignore','pipe','pipe'],env:childEnv,windowsHide:true});
 child.stdout.on('data',data=>{output=(output+data.toString()).slice(-12000);});
 child.stderr.on('data',data=>{output=(output+data.toString()).slice(-12000);});
 const lines=createInterface({input:child.stdout});
 const url=await Promise.race([new Promise((resolve,reject)=>{lines.on('line',line=>{const match=line.match(/http:\/\/127\.0\.0\.1:\d+\/#access=[a-f0-9]+/);if(match)resolve(match[0]);});child.once('error',reject);child.once('close',(code,signal)=>reject(Error(label+' exited before readiness (code='+code+', signal='+signal+'): '+redact(output))));}),new Promise((_,reject)=>{const t=setTimeout(()=>reject(Error(label+' startup timeout: '+redact(output))),30000);t.unref();})]);lines.close();
 const u=new URL(url),token=new URLSearchParams(u.hash.slice(1)).get('access');origin=u.origin;headers={Authorization:'Bearer '+token};
 const snapshot=await fetch(u.origin+'/api/overview',{headers}).then(r=>r.json());assert(snapshot.node?.data?.running===true&&snapshot.agents?.data?.length===0,'Fresh package snapshot invalid');
 const exit=once(child,'exit');const stopped=await fetch(u.origin+'/api/panel/stop',{method:'POST',headers:{...headers,origin:u.origin,'content-type':'application/json'},body:'{}'});assert(stopped.status===200,'Packaged stop failed');
 await Promise.race([exit,new Promise((_,reject)=>{const t=setTimeout(()=>reject(Error('Packaged stop deadline')),20000);t.unref();})]);assert(child.exitCode===0,'Packaged panel failed shutdown');
 const ownStatus=spawnSync(binary,['--json','--data-dir',dataDir,'daemon','status'],{encoding:'utf8',env:childEnv});assert(ownStatus.status!==0,'Owned Daemon remained reachable');
}finally{if(child&&child.exitCode===null){if(origin)try{await fetch(origin+'/api/panel/stop',{method:'POST',headers:{...headers,origin,'content-type':'application/json'},body:'{}'});}catch{}child.kill();}}
}
await checkStartup(binary,['--data-dir',join(root,'direct instance'),'panel','--start-daemon','--port','0'],join(root,'direct instance'),'Standalone panel');
 const launcher=win?join(process.env.SystemRoot||'C:\\Windows','System32','WindowsPowerShell','v1.0','powershell.exe'):'/bin/sh';
 const args=win?['-NoProfile','-NonInteractive','-File',join(dir,'start-windows.ps1'),'-DataDir',join(root,'fresh instance'),'-Port','0']:[join(dir,process.platform==='darwin'?'start-mac.command':'start.sh'),join(root,'fresh instance'),'0'];
await checkStartup(launcher,args,join(root,'fresh instance'),'Packaged launcher');
console.log(JSON.stringify({passed:true,platform:process.platform,arch:process.arch,model_calls:0,checks:['archive-extraction','manifest-hashes','no-instance-material','standalone-cli','standalone-panel','upgrade-preflight','actual-launcher','fresh-panel-and-daemon','authenticated-snapshot','owned-shutdown']}));
