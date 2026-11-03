// Real CLI + private temporary Node. No Harness/model configuration or shell RPC.
import{spawn,spawnSync}from'node:child_process';
import{mkdtempSync,renameSync,readFileSync,writeFileSync,existsSync}from'node:fs';
import{tmpdir}from'node:os';
import{join,resolve}from'node:path';
import{once}from'node:events';
import{assert,until,flags}from'./acceptance/common.mjs';
const f=flags(process.argv.slice(2).filter(a=>a!=='--startup-install'),['--binary']);
const startup=process.argv.includes('--startup-install');
assert(!startup||process.env.GITHUB_ACTIONS==='true','Startup installation smoke is restricted to disposable CI runners');
const binary=resolve(f['--binary']||('target/debug/hmesh'+(process.platform==='win32'?'.exe':'')));
const root=mkdtempSync(join(tmpdir(),'hmesh-maintenance-')),node=join(root,'node'),backup=join(root,'backup');
let daemon,installed=false;const checks=[];
function call(args,success=true){const r=spawnSync(binary,['--json','--data-dir',node,...args],{encoding:'utf8',timeout:30000});assert(success?r.status===0:r.status!==0,'Unexpected CLI status for '+args.slice(0,2).join(' '));return r.status===0?JSON.parse(r.stdout):null;}
try{
 const identity=call(['init','--name','maintenance-fixture','--port','0']);
 daemon=spawn(binary,['--data-dir',node,'daemon','start'],{stdio:'ignore',windowsHide:true});
 await until(()=>{try{return call(['daemon','status']).running;}catch{return false;}});
 call(['backup','create','--out',backup],false);assert(!existsSync(backup),'Live backup created a destination');checks.push('live-lock-refusal');
 const done=once(daemon,'exit');call(['daemon','stop']);await done;assert(daemon.exitCode===0,'Temporary daemon did not stop');
 call(['backup','create','--out',backup]);assert(call(['backup','check','--from',backup]).passed,'Backup check failed');checks.push('offline-consistent-backup');
 call(['backup','create','--out',backup],false);checks.push('no-overwrite');
 renameSync(node,join(root,'original-preserved'));call(['backup','restore','--from',backup]);const restored=call(['node','show']);assert(restored.node_id===identity.node_id&&restored.fingerprint===identity.fingerprint,'Identity changed during restore');checks.push('same-path-identity-restore');
 const before=readFileSync(join(backup,'policy.toml'));writeFileSync(join(backup,'policy.toml'),'tampered');call(['backup','check','--from',backup],false);writeFileSync(join(backup,'policy.toml'),before);checks.push('tamper-refusal');
 if(['darwin','win32'].includes(process.platform)){
  const plan=call(['startup','plan']);assert(plan.daemon_only===true&&plan.configuration.includes('daemon'),'Startup plan invalid');assert(call(['startup','status']).installed===false,'Planning installed a job');checks.push('startup-plan-no-mutation');
  if(process.platform==='darwin'){
   const path=join(root,'login.plist');writeFileSync(path,plan.configuration);const parsed=spawnSync('/usr/bin/plutil',['-lint',path],{encoding:'utf8'});assert(parsed.status===0,'Generated LaunchAgent plist invalid');checks.push('native-plist-parser');
  }
  if(startup){
   for(let i=0;i<2;i++){
    call(['startup','install']);installed=true;assert(call(['startup','status']).installed===true,'Native startup registration not observed');call(['startup','uninstall']);installed=false;assert(call(['startup','status']).installed===false,'Startup registration remained');
   }
   checks.push('native-login-install-status-uninstall-reinstall');
  }
 }
 console.log(JSON.stringify({passed:true,platform:process.platform,model_calls:0,startup_installed_on_developer_machine:false,checks}));
}finally{
 if(installed)try{call(['startup','uninstall']);}catch{process.exitCode=1;}
 if(daemon&&daemon.exitCode===null){daemon.kill();await Promise.race([once(daemon,'exit'),new Promise(r=>setTimeout(r,3000))]);}
}
