[CmdletBinding()]
param([ValidateSet('prepare','generate','status','collect','test','start','stop-mesh','restart-mesh','repair-backend','stop','restart-backend')][string]$Action='status')
$ErrorActionPreference='Stop'
$workspace=Split-Path -Parent $PSScriptRoot
$pointer=Join-Path $workspace '.hmesh-v0/todo-active.txt'
$binary=Join-Path $workspace 'target/debug/hmesh.exe'
$nodeExe='C:/Program Files/nodejs/node.exe'
function Assert-That($Condition,[string]$Message){if(-not $Condition){throw $Message}}
function Program([string]$File,[string[]]$Arguments,[int]$Timeout=45000){
    $start=[Diagnostics.ProcessStartInfo]::new();$start.FileName=$File;$start.UseShellExecute=$false;$start.CreateNoWindow=$true
    $start.RedirectStandardOutput=$true;$start.RedirectStandardError=$true
    foreach($argument in $Arguments){$start.ArgumentList.Add($argument)}
    $process=[Diagnostics.Process]::new();$process.StartInfo=$start;[void]$process.Start()
    $stdout=$process.StandardOutput.ReadToEndAsync();$stderr=$process.StandardError.ReadToEndAsync()
    if(-not $process.WaitForExit($Timeout)){$process.Kill($true);$process.WaitForExit();throw 'Observation deadline exceeded; task was not resubmitted'}
    $output=$stdout.GetAwaiter().GetResult();$errorText=$stderr.GetAwaiter().GetResult()
    Assert-That ($process.ExitCode -eq 0) ($errorText+$output)
    return $output
}
function Save-State{[IO.File]::WriteAllText((Join-Path $sessionRoot 'state.json'),($state|ConvertTo-Json -Depth 18))}
function Local([string[]]$Arguments){return(Program $binary (@('--json','--data-dir',$state.node_dir)+$Arguments)|ConvertFrom-Json)}
function Remote([string]$Operation,$Body=@{}){
    $payload=[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes(($Body|ConvertTo-Json -Depth 16 -Compress)))
    $command='python3 '+$state.remote_root+'/todo-remote.py '+$Operation+' '+$payload
    return(Program 'ssh.exe' @('-F',$state.ssh_config,'-T','-o','BatchMode=yes','-o','ConnectTimeout=10','hmesh-trial',$command) 75000|ConvertFrom-Json)
}
function Record-Process($Process){return @{pid=$Process.Id;start_ticks=$Process.StartTime.ToUniversalTime().Ticks;path=$Process.Path}}
function Stop-Owned($Record){
    if(-not $Record){return}
    $process=Get-Process -Id $Record.pid -ErrorAction SilentlyContinue
    if($process){Assert-That ($process.StartTime.ToUniversalTime().Ticks -eq $Record.start_ticks) 'PID reused; refusing to stop it';$process.Kill();[void]$process.WaitForExit(10000)}
}
function Start-Tunnel([string]$Name,[string[]]$Extra){
    $arguments=@('-F',('"'+$state.ssh_config+'"'),'-N','-T','-o','ExitOnForwardFailure=yes','-o','ServerAliveInterval=15','-o','ServerAliveCountMax=3')+$Extra+@('hmesh-trial')
    $process=Start-Process -FilePath 'ssh.exe' -ArgumentList $arguments -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $sessionRoot ($Name+'.stdout.log')) -RedirectStandardError (Join-Path $sessionRoot ($Name+'.stderr.log'))
    $state[$Name]=Record-Process $process;Save-State
    Start-Sleep -Seconds 2;Assert-That (-not $process.HasExited) ($Name+' failed to start')
}
if($Action -eq 'prepare'){
    Assert-That (-not(Test-Path -LiteralPath $pointer)) 'An active Todo session already exists; inspect/stop it before preparing another'
    $prior=[IO.File]::ReadAllText((Join-Path $workspace '.hmesh-v0/active-ssh-root.txt'))
    $build=Get-Content -LiteralPath (Join-Path $prior 'remote-setup.json') -Raw|ConvertFrom-Json
    $token=[guid]::NewGuid().ToString('N')
    $sessionRoot=Join-Path $workspace ('.hmesh-v0/todo-'+$token);[IO.Directory]::CreateDirectory($sessionRoot)|Out-Null
    $remoteRoot='/mnt/data2/home/cqz/.harnessmesh-smoke/todo-'+$token
    $client=Join-Path $workspace 'examples/ssh-todo/client';[IO.Directory]::CreateDirectory($client)|Out-Null
    Copy-Item -LiteralPath (Join-Path $workspace 'examples/ssh-todo/api-contract.json') -Destination (Join-Path $client 'api-contract.json')
    $state=@{session=$token;remote_root=$remoteRoot;build_root=$build.root;ssh_config=(Join-Path $sessionRoot 'ssh.conf');node_dir=(Join-Path $sessionRoot 'node-windows');client=$client}
    Copy-Item -LiteralPath (Join-Path $prior 'ssh.conf') -Destination $state.ssh_config
    [IO.File]::WriteAllText($pointer,$sessionRoot);Save-State
    $command="python3 -c 'from pathlib import Path;Path(`"$remoteRoot`").mkdir(mode=448)'"
    [void](Program 'ssh.exe' @('-F',$state.ssh_config,'-T','hmesh-trial',$command))
    [void](Program 'scp.exe' @('-F',$state.ssh_config,(Join-Path $PSScriptRoot 'todo-remote.py'),('hmesh-trial:'+$remoteRoot+'/todo-remote.py')))
    $remote=Remote 'init' @{build_root=$build.root;contract=[IO.File]::ReadAllText((Join-Path $workspace 'examples/ssh-todo/api-contract.json'))}
    $state.remote_card=$remote.card;$state.local_card=Local @('init','--name','windows-todo','--port','38880');Save-State
    $process=Start-Process -FilePath $binary -ArgumentList @('--data-dir',('"'+$state.node_dir+'"'),'daemon','start') -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $state.node_dir 'daemon.stdout.log') -RedirectStandardError (Join-Path $state.node_dir 'daemon.stderr.log')
    $state.daemon=Record-Process $process;Save-State
    for($i=0;$i -lt 100;$i++){if(Test-Path -LiteralPath (Join-Path $state.node_dir 'daemon.json')){break};Start-Sleep -Milliseconds 50}
    [void](Remote 'daemon-start')
    Start-Tunnel 'mesh_tunnel' @('-L','127.0.0.1:38881:127.0.0.1:38881','-R','127.0.0.1:38880:127.0.0.1:38880')
    $pair=Local @('pair','request','--peer',$state.remote_card.endpoint,'--expect-fingerprint',$state.remote_card.fingerprint)
    [void](Remote 'cli' @{arguments=@('pair','approve',$pair.request_id,'--expect-fingerprint',$state.local_card.fingerprint)})
    [void](Local @('pair','approve',$pair.request_id,'--expect-fingerprint',$state.remote_card.fingerprint))
    $state.pi=Remote 'cli' @{arguments=@('agent','register','--config',($remoteRoot+'/agent.toml'))}
    $dsh=(Get-Command dsh.cmd -ErrorAction Stop).Source.Replace('\','/')
    $rows=@('[agent]','name="windows-todo-dsh"','harness="dsh"','mode="managed"','project_id="todo-client"','[project]',('root='+($client.Replace('\','/')|ConvertTo-Json -Compress)),'allowed_tools=["read","write","edit"]','artifact_paths=["index.html","styles.css","app.js","serve.mjs","client.test.mjs","package.json","README.md"]','require_remote_task_approval=true','[adapter]',('executable='+($dsh|ConvertTo-Json -Compress)),'provider="deepseek-official"','model="deepseek-v4-pro"','env_allowlist=["DEEPSEEK_API_KEY"]')
    $config=Join-Path $sessionRoot 'dsh.toml';[IO.File]::WriteAllLines($config,$rows)
    $state.dsh=Local @('agent','register','--config',$config)
    $state.pi_verification=Remote 'cli' @{arguments=@('agent','verify',$state.pi.agent_id,'--allow-model')}
    Save-State
    [pscustomobject]@{session=$token;pi=$state.pi.agent_id;dsh=$state.dsh.agent_id;pi_verification_task=$state.pi_verification.task_id}
    exit 0
}
Assert-That (Test-Path -LiteralPath $pointer) 'Prepare a Todo session first'
$sessionRoot=[IO.File]::ReadAllText($pointer);$state=Get-Content -LiteralPath (Join-Path $sessionRoot 'state.json') -Raw|ConvertFrom-Json -AsHashtable
switch($Action){
    'generate'{
        Assert-That (-not $state.frontend_task -and -not $state.backend_task) 'Generation already submitted; inspect status rather than resubmitting'
        $prompt=[IO.File]::ReadAllText((Join-Path $workspace 'examples/ssh-todo/frontend-task.md'))
        $state.frontend_task=Local @('agent','verify',$state.dsh.agent_id,'--allow-model','--prompt',$prompt);Save-State
        for($i=0;$i -lt 60;$i++){$check=Remote 'cli' @{arguments=@('task','show',$state.pi_verification.task_id)};if($check.status -eq 'SUCCEEDED'){break};Assert-That ($check.status -notin @('FAILED','UNKNOWN')) ('Pi authentication failed: '+($check.error|ConvertTo-Json -Compress));Start-Sleep -Seconds 1}
        Assert-That ($check.status -eq 'SUCCEEDED') 'Pi model verification is unresolved'
        [void](Remote 'grant' @{peer=$state.local_card.node_id;agent=$state.pi.agent_id})
        $prompt=[IO.File]::ReadAllText((Join-Path $workspace 'examples/ssh-todo/backend-task.md'))
        $id=[guid]::NewGuid().ToString();$state.backend_task=Local @('task','send',$state.pi.agent_id,'--peer',$state.remote_card.node_id,'--project','todo-backend','--request-id',$id,'--timeout','600','--prompt',$prompt);Save-State
        Assert-That ($state.backend_task.status -eq 'PENDING_APPROVAL') 'Backend task bypassed approval'
        [void](Remote 'cli' @{arguments=@('task','approve',$state.backend_task.task_id)})
        [pscustomobject]@{frontend_task=$state.frontend_task.task_id;backend_task=$state.backend_task.task_id}
    }
    'status'{
        foreach($kind in @('frontend','backend')){if($state[$kind+'_task']){$task=Local @('task','show',$state[$kind+'_task'].task_id);[pscustomobject]@{kind=$kind;task_id=$task.task_id;status=$task.status;error=$task.error}}}
    }
    'restart-mesh'{
        Assert-That $state.mesh_stopped 'Stop authoring Daemons before restarting'
        $process=Start-Process -FilePath $binary -ArgumentList @('--data-dir',('"'+$state.node_dir+'"'),'daemon','start') -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $state.node_dir 'restart.stdout.log') -RedirectStandardError (Join-Path $state.node_dir 'restart.stderr.log')
        $state.daemon=Record-Process $process;Save-State
        for($i=0;$i -lt 100;$i++){if(Test-Path -LiteralPath (Join-Path $state.node_dir 'daemon.json')){break};Start-Sleep -Milliseconds 50}
        [void](Remote 'daemon-start')
        Start-Tunnel 'mesh_tunnel' @('-L','127.0.0.1:38881:127.0.0.1:38881','-R','127.0.0.1:38880:127.0.0.1:38880')
        $state.mesh_stopped=$false;Save-State
    }
    'repair-backend'{
        $prior=Local @('task','show',$state.backend_task.task_id)
        Assert-That ($prior.status -in @('FAILED','SUCCEEDED')) 'Inspect the unresolved original task before issuing a repair'
        $state.backend_history=@($state.backend_history|Where-Object {$_})+@($prior)
        $reason=if($prior.error){$prior.error.message}else{'The runner explicitly requested a separate follow-up.'}
        $prompt=[IO.File]::ReadAllText((Join-Path $workspace 'examples/ssh-todo/backend-task.md'))+"`nPrevious task outcome: $reason. Keep existing files and complete only missing or incorrect deliverables. This is a separate explicitly submitted attempt with a bounded stdout quota; the adapter's separate native message limit still applies. Keep explanations concise."
        $id=[guid]::NewGuid().ToString();$state.backend_task=Local @('task','send',$state.pi.agent_id,'--peer',$state.remote_card.node_id,'--project','todo-backend','--request-id',$id,'--timeout','600','--max-output-bytes','33554432','--prompt',$prompt);Save-State
        [void](Remote 'cli' @{arguments=@('task','approve',$state.backend_task.task_id)})
        $state.backend_task|Select-Object task_id,status
    }
    'collect'{
        # A terminal run can leave complete artifacts even when an output guard stops
        # it. Preserve that failure; artifact export is not a claim of task success.
        foreach($kind in @('frontend','backend')){$task=Local @('task','show',$state[$kind+'_task'].task_id);Assert-That ($task.status -in @('SUCCEEDED','FAILED')) ($kind+' is still unresolved');$state[$kind+'_result']=$task}
        $export=Remote 'export';$server=Join-Path $workspace 'examples/ssh-todo/server';[IO.Directory]::CreateDirectory($server)|Out-Null
        $hashes=@{}
        foreach($file in $export.PSObject.Properties){$path=Join-Path $server $file.Name;[IO.File]::WriteAllText($path,$file.Value.text);Assert-That ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() -eq $file.Value.sha256) ('Export hash mismatch: '+$file.Name);$hashes[$file.Name]=$file.Value.sha256}
        $state.server_hashes=$hashes
        foreach($kind in @('frontend','backend')){$events=@(Local @('task','events',$state[$kind+'_task'].task_id));[IO.File]::WriteAllText((Join-Path $sessionRoot ($kind+'-events.json')),($events|ConvertTo-Json -Depth 20))}
        Save-State;Write-Output 'Pi backend exported; DSH frontend and both task event records preserved.'
    }
    'test'{
        $backend=Remote 'test';[IO.File]::WriteAllText((Join-Path $sessionRoot 'backend-tests.json'),($backend|ConvertTo-Json -Depth 6));Assert-That ($backend.exit -eq 0) $backend.output
        $output=Program $nodeExe @('--test',(Join-Path $state.client 'client.test.mjs')) 75000
        [IO.File]::WriteAllText((Join-Path $sessionRoot 'frontend-tests.txt'),$output)
        Write-Output $backend.output;Write-Output $output
    }
    'start'{
        Assert-That (-not $state.frontend_process -and -not $state.app_tunnel) 'Application already started; use stop before starting it again'
        $state.backend_process=Remote 'backend-start';Save-State
        Start-Tunnel 'app_tunnel' @('-L','127.0.0.1:18080:127.0.0.1:8000')
        $config=@{entry=(Join-Path $state.client 'serve.mjs');cwd=$state.client;log=(Join-Path $sessionRoot 'frontend.log')}|ConvertTo-Json -Compress
        $payload=[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($config))
        $launcher="const fs=require('node:fs'),{spawn}=require('node:child_process');const c=JSON.parse(Buffer.from(process.argv[1],'base64'));const env={PORT:'5173',UPSTREAM:'http://127.0.0.1:18080'};for(const k of ['PATH','SystemRoot','WINDIR','TEMP','TMP'])if(process.env[k])env[k]=process.env[k];const fd=fs.openSync(c.log,'a');const p=spawn(process.execPath,[c.entry],{cwd:c.cwd,env,detached:true,windowsHide:true,stdio:['ignore',fd,fd]});p.on('error',e=>{console.error(e.message);process.exitCode=1});p.on('spawn',()=>{console.log(JSON.stringify({pid:p.pid}));p.unref();fs.closeSync(fd)});"
        $spawned=Program $nodeExe @('-e',$launcher,$payload)|ConvertFrom-Json
        $state.frontend_process=Record-Process (Get-Process -Id $spawned.pid);Save-State
        for($i=0;$i -lt 50;$i++){try{$health=Invoke-RestMethod 'http://127.0.0.1:5173/api/health' -TimeoutSec 2;if($health.status -eq 'ok'){break}}catch{};Start-Sleep -Milliseconds 200}
        Assert-That ($health.status -eq 'ok' -and $health.node -eq 'Orion') 'Frontend cannot reach the real Orion API'
        Write-Output 'Application ready: http://127.0.0.1:5173'
    }
    'restart-backend'{[void](Remote 'stop' @{component='backend'});$state.backend_process=Remote 'backend-start';Save-State;Write-Output 'Orion backend restarted with the same JSON data file.'}
    'stop-mesh'{
        [void](Remote 'stop' @{component='daemon'})
        try{[void](Local @('daemon','stop'))}catch{}
        Stop-Owned $state.daemon;Stop-Owned $state.mesh_tunnel
        $state.mesh_stopped=$true;Save-State;Write-Output 'Authoring Daemons stopped; application services remain running.'
    }
    'stop'{
        [void](Remote 'stop' @{component='backend'});Stop-Owned $state.frontend_process;Stop-Owned $state.app_tunnel
        $state.frontend_process=$null;$state.app_tunnel=$null;$state.backend_process=$null;Save-State
        Write-Output 'Todo application stopped; code, data and authoring evidence retained.'
    }
}
