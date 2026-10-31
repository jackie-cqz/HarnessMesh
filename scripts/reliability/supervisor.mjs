// Restarts only this exact child once after the intentional exit code 42.
import {spawn} from 'node:child_process';
import {readFileSync,existsSync,rmSync} from 'node:fs';
import {dirname,join,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
const args=process.argv.slice(2);if(args.length!==2||args[0]!=='--config')throw Error('Expected --config OWNER_JSON');
const config=JSON.parse(readFileSync(args[1],'utf8').replace(/^\uFEFF/,''));let restarts=0,child,stopping=false;
function start(){child=spawn(process.execPath,[join(dirname(fileURLToPath(import.meta.url)),'guardian.mjs'),'--config',resolve(args[1])],{stdio:'inherit',windowsHide:true});const owned=child.pid;child.once('exit',code=>{
  if(code===42&&!stopping&&restarts===0){const lock=join(config.state_dir,'guardian.lock'),owner=join(lock,'owner.json');if(!existsSync(owner)||JSON.parse(readFileSync(owner)).pid!==owned){console.error('Cannot prove stale lock ownership');process.exitCode=1;return;}rmSync(lock,{recursive:true});restarts++;start();}
  else if(code!==0&&!stopping)process.exitCode=1;
});}
for(const sig of ['SIGINT','SIGTERM'])process.once(sig,()=>{stopping=true;child?.kill();});start();
