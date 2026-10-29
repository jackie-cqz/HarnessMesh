[CmdletBinding()]
param([ValidateSet('build','build-status','prepare','services','integrate','direct-route','reintegrate','snapshot','restart-final','resume-final','restart-services','final-services','final-integrate','status','cut-tunnel','check-tunnel','collect','revoke','cleanup')][string]$Action='status',[switch]$RunRealHarness,[ValidateSet('direct','original-proxy')][string]$ModelNetwork='direct')
$ErrorActionPreference='Stop'
$workspace=Split-Path -Parent $PSScriptRoot
$pointer=Join-Path $workspace '.hmesh-v0/managed-active.txt'
$binary=Join-Path $workspace 'target/debug/hmesh.exe'
function Assert($Condition,[string]$Message){if(-not $Condition){throw $Message}}
function Program([string]$File,[string[]]$Arguments,[int]$Timeout=75000,[switch]$AllowFailure){
    $start=[Diagnostics.ProcessStartInfo]::new();$start.FileName=$File;$start.UseShellExecute=$false;$start.CreateNoWindow=$true
    $start.RedirectStandardOutput=$true;$start.RedirectStandardError=$true
    foreach($argument in $Arguments){$start.ArgumentList.Add($argument)}
    $process=[Diagnostics.Process]::new();$process.StartInfo=$start;[void]$process.Start()
    $stdout=$process.StandardOutput.ReadToEndAsync();$stderr=$process.StandardError.ReadToEndAsync()
    if(-not $process.WaitForExit($Timeout)){$process.Kill($true);throw 'Observation deadline exceeded; no model task was resubmitted'}
    $output=$stdout.GetAwaiter().GetResult();$errorText=$stderr.GetAwaiter().GetResult()
    if($AllowFailure){return @{exit=$process.ExitCode;stdout=$output;stderr=$errorText}}
    Assert ($process.ExitCode -eq 0) ($errorText+$output);$output
}
function Save{[IO.File]::WriteAllText($recordPath,($record|ConvertTo-Json -Depth 35))}
function Local([string[]]$Arguments){Program $binary (@('--json','--data-dir',$record.node_dir)+$Arguments)|ConvertFrom-Json -Depth 35}
function Remote([string]$Operation,$Body=@{}){
    $payload=[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes(($Body|ConvertTo-Json -Depth 25 -Compress)))
    Program 'ssh.exe' @('-F',$record.ssh_config,'-T','-o','ConnectTimeout=10','hmesh-trial',('python3 '+$record.remote_root+'/managed-smoke-remote.py '+$Operation+' '+$payload))|ConvertFrom-Json -Depth 35
}
function Owner([string[]]$Arguments){Remote 'cli' @{arguments=$Arguments}}
function Config([string]$Name,$Value){$file=Join-Path $record.root ($Name+'.json');[IO.File]::WriteAllText($file,($Value|ConvertTo-Json -Depth 25));return $file}
function Resource([string]$Kind,[string]$Id){@(Local @($Kind,'list'))|Where-Object {$_.id -eq $Id}|Select-Object -First 1}
function Wait-Resource([string]$Kind,[string]$Id,[scriptblock]$Condition){
    for($i=0;$i -lt 80;$i++){$value=Resource $Kind $Id;if(& $Condition $value){return $value};Start-Sleep -Milliseconds 250}
    throw ('Resource not ready: '+($value|ConvertTo-Json -Depth 8 -Compress))
}
function Client-Check{
    $file=Join-Path $record.client 'contract-check.mjs';Assert (Test-Path -LiteralPath $file) 'DSH has not produced the client test'
    $result=Program 'node.exe' @($file,$record.endpoint) -AllowFailure
    # Full model-authored diagnostics stay private. Public evidence records only
    # deterministic exit/hash and the fixed owner contract result.
    [IO.File]::WriteAllText((Join-Path $record.root ('client-check-'+[guid]::NewGuid().ToString('N')+'.json')),($result|ConvertTo-Json))
    return @{exit=$result.exit;script_sha256=(Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant()}
}
if($Action -eq 'build'){
    Assert (-not(Test-Path -LiteralPath $pointer)) 'Managed trial exists; inspect its saved state instead of replacing it'
    $oldRoot=[IO.File]::ReadAllText((Join-Path $workspace '.hmesh-v0/todo-active.txt'))
    $old=Get-Content -LiteralPath (Join-Path $oldRoot 'state.json') -Raw|ConvertFrom-Json
    $token=[guid]::NewGuid().ToString('N');$root=Join-Path $workspace ('.hmesh-v0/managed-'+$token)
    [IO.Directory]::CreateDirectory($root)|Out-Null
    $recordPath=Join-Path $root 'state.json'
    $record=@{root=$root;remote_root=('/mnt/data2/home/cqz/.harnessmesh-smoke/managed-'+$token);ssh_config=(Join-Path $root 'ssh.conf');build_root=$old.build_root;template_root=$old.remote_root;template_local=$oldRoot;node_dir=(Join-Path $root 'node');client=(Join-Path $root 'client')}
    Copy-Item -LiteralPath $old.ssh_config -Destination $record.ssh_config
    Save;[IO.File]::WriteAllText($pointer,$root)
    $source=Join-Path $root 'source.zip'
    $files=@(git -C $workspace ls-files --cached --others --exclude-standard)|Where-Object {$_ -notmatch '^\.hmesh' -and $_ -ne ''}
    Add-Type -AssemblyName System.IO.Compression
    $stream=[IO.File]::Open($source,[IO.FileMode]::CreateNew)
    $zip=[IO.Compression.ZipArchive]::new($stream,[IO.Compression.ZipArchiveMode]::Create)
    try{foreach($relative in $files){[IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip,(Join-Path $workspace $relative),$relative.Replace('\','/'))|Out-Null}}finally{$zip.Dispose();$stream.Dispose()}
    $mkdir="python3 -c 'from pathlib import Path;Path(`"$($record.remote_root)`").mkdir(mode=448)'"
    [void](Program 'ssh.exe' @('-F',$record.ssh_config,'-T','hmesh-trial',$mkdir))
    [void](Program 'scp.exe' @('-F',$record.ssh_config,$source,('hmesh-trial:'+$record.remote_root+'/source.zip')))
    [void](Program 'scp.exe' @('-F',$record.ssh_config,(Join-Path $PSScriptRoot 'managed-smoke-remote.py'),('hmesh-trial:'+$record.remote_root+'/managed-smoke-remote.py')))
    $record.build=Remote 'build-start' @{build_root=$record.build_root;template_root=$record.template_root};Save
    Write-Output 'Isolated Orion offline build started.';exit 0
}
Assert (Test-Path -LiteralPath $pointer) 'Start with build'
$root=[IO.File]::ReadAllText($pointer);$recordPath=Join-Path $root 'state.json'
$record=Get-Content -LiteralPath $recordPath -Raw|ConvertFrom-Json -AsHashtable
switch($Action){
    'build-status'{Remote 'build-status'}
    'prepare'{
        Assert $RunRealHarness 'prepare calls real Pi/DSH models; pass -RunRealHarness'
        $checks=Remote 'build-status';Assert ($checks.checks.Count -eq 2 -and @($checks.checks|Where-Object {$_.exit -ne 0}).Count -eq 0) 'Linux checks incomplete/failed'
        Assert (-not $record.local_card -and -not $record.remote) 'Nodes already prepared; do not replace model tasks'
        $record.remote=Remote 'prepare' @{model_network=$ModelNetwork};$record.local_card=Local @('init','--name','windows-managed-checkpoint','--port','39180');Save
        if(-not $env:DEEPSEEK_API_KEY){$env:DEEPSEEK_API_KEY=[Environment]::GetEnvironmentVariable('DEEPSEEK_API_KEY','User')}
        Assert ([bool]$env:DEEPSEEK_API_KEY) 'DSH owner environment is unavailable'
        $process=Start-Process -FilePath $binary -ArgumentList @('--data-dir',('"'+$record.node_dir+'"'),'daemon','start') -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $root 'daemon.out') -RedirectStandardError (Join-Path $root 'daemon.err')
        $record.daemon=@{pid=$process.Id;start_ticks=$process.StartTime.ToUniversalTime().Ticks;path=$process.Path};Save
        for($i=0;$i -lt 100;$i++){if(Test-Path -LiteralPath (Join-Path $record.node_dir 'daemon.json')){break};Start-Sleep -Milliseconds 50}
        [void](Remote 'start');[void](Local @('daemon','status'))
        $record.tunnel_id=[guid]::NewGuid().ToString()
        $spec=@{id=$record.tunnel_id;project_id=$record.remote.project;name='orion-control';ssh_executable=(Get-Command ssh.exe).Source;hostname='10.212.253.98';user='cqz';port=22;identity_file=(Join-Path $env:USERPROFILE '.ssh/id_cqz_orion');known_hosts_file=(Join-Path $env:USERPROFILE '.ssh/known_hosts');forwards=@(@{direction='local';bind_port=39181;target_port=39181});lease_seconds=3600}
        $file=Config 'tunnel' $spec;[void](Local @('tunnel','add','--config',$file));Save
        [void](Wait-Resource 'tunnel' $record.tunnel_id {param($v)$v.status.process -eq 'running'})
        Start-Sleep -Seconds 1
        $pair=Local @('pair','request','--peer',$record.remote.card.endpoint,'--expect-fingerprint',$record.remote.card.fingerprint);$record.pair=$pair;Save
        [void](Owner @('pair','approve',$pair.request_id,'--expect-fingerprint',$record.local_card.fingerprint))
        [void](Local @('pair','approve',$pair.request_id,'--expect-fingerprint',$record.remote.card.fingerprint))
        $record.pi=Owner @('agent','register','--config',($record.remote_root+'/agent.toml'))
        $record.pi_verification=Owner @('agent','verify',$record.pi.agent_id,'--allow-model');Save
        Copy-Item -LiteralPath (Join-Path $workspace 'examples/ssh-todo/client') -Destination $record.client -Recurse
        $proxyFile=Join-Path $record.client 'serve.mjs'
        $proxySource=[IO.File]::ReadAllText($proxyFile)
        Assert ($proxySource.Contains('const PROXY_TIMEOUT_MS = 10_000;')) 'Unexpected frontend timeout configuration'
        [IO.File]::WriteAllText($proxyFile,$proxySource.Replace('const PROXY_TIMEOUT_MS = 10_000;','const PROXY_TIMEOUT_MS = 45_000;'))
        $record.client_original=@{}
        foreach($name in @('index.html','styles.css','app.js','serve.mjs','client.test.mjs','api-contract.json')){$record.client_original[$name]=(Get-FileHash -LiteralPath (Join-Path $record.client $name) -Algorithm SHA256).Hash.ToLowerInvariant()}
        $record.known_hosts_before=(Get-FileHash -LiteralPath (Join-Path $env:USERPROFILE '.ssh/known_hosts') -Algorithm SHA256).Hash.ToLowerInvariant()
        $rows=[IO.File]::ReadAllLines((Join-Path $record.template_local 'dsh.toml'))|ForEach-Object{if($_ -match '^name='){'name="windows-managed-client"'}elseif($_ -match '^project_id='){'project_id="managed-client"'}elseif($_ -match '^root='){'root='+($record.client.Replace('\','/')|ConvertTo-Json -Compress)}elseif($_ -match '^artifact_paths='){'artifact_paths=["contract-check.mjs"]'}else{$_}}
        $file=Join-Path $root 'dsh.toml';[IO.File]::WriteAllLines($file,$rows)
        $record.dsh=Local @('agent','register','--config',$file)
        $prompt='Use only read/write/edit in this registered client project. Read api-contract.json. Create contract-check.mjs using Node built-ins only. Accept base URL in process.argv[2], use fetch with 45-second timeouts and node:assert/strict. Assert GET /api/health returns status 200 and JSON status="ok", service="orion-todo-api", node="Orion". Then POST a unique todo, PATCH it completed=true, and GET the list to assert the record is present and completed. Print a single JSON {"passed":true,"checks":4} only after all assertions pass; failures must exit nonzero. Never change the contract or existing UI/source files, start processes or call network yourself. Keep final response brief.'
        $record.dsh_task=Local @('agent','verify',$record.dsh.agent_id,'--allow-model','--prompt',$prompt);Save
        [void](Remote 'config-cli' @{command='policy';peer=$record.local_card.node_id;agent=$record.pi.agent_id})
        Write-Output 'Managed control tunnel and pairing ready; real Pi verification and DSH client authoring submitted.'
    }
    'services'{
        Assert (-not $record.service_id) 'Services already published; inspect state'
        Assert ((Owner @('task','show',$record.pi_verification.task_id)).status -eq 'SUCCEEDED') 'Pi verification unresolved'
        Assert ((Local @('task','show',$record.dsh_task.task_id)).status -eq 'SUCCEEDED') 'DSH client authoring unresolved'
        $record.service_id=[guid]::NewGuid().ToString();$spec=Remote 'service-spec' @{id=$record.service_id;peer=$record.local_card.node_id};Save
        [void](Remote 'config-cli' @{command='publish';spec=$spec})
        for($i=0;$i -lt 50;$i++){$cards=@(Local @('service','list','--peer',$record.remote.card.node_id));if($cards[0].health -eq 'healthy'){break};Start-Sleep -Milliseconds 250}
        Assert ($cards[0].health -eq 'healthy') 'Backend HTTP transport not healthy'
        $record.connection_id=[guid]::NewGuid().ToString();Save
        $file=Config 'connection' @{id=$record.connection_id;peer_id=$record.remote.card.node_id;service_id=$record.service_id;local_port=18081;lease_seconds=3600}
        [void](Local @('service','connect','--config',$file))
        $v=Wait-Resource 'connection' $record.connection_id {param($v)$v.status.endpoint};$record.endpoint=$v.status.endpoint
        $record.client_before=Client-Check;Assert ($record.client_before.exit -ne 0) 'Injected missing-field defect was not detected by DSH client'
        $health=Invoke-RestMethod ($record.endpoint+'/api/health');Assert (-not $health.PSObject.Properties['node']) 'Expected injected node-field defect'
        $record.before_transport=@{status=$health.status;service=$health.service;node_present=$false}
        $record.frontend_id=[guid]::NewGuid().ToString();Save
        $file=Config 'frontend-service' @{id=$record.frontend_id;project_id='managed-client';name='managed-todo-client';target_port=5174;allowed_peer_ids=@();allowed_restart_peer_ids=@();lease_seconds=3600;health_path='/';process=@{executable=(Get-Command node.exe).Source;cwd=$record.client;args=@((Join-Path $record.client 'serve.mjs'));env=@{PORT='5174';UPSTREAM=$record.endpoint}}}
        [void](Local @('service','publish','--config',$file))
        [void](Wait-Resource 'service' $record.frontend_id {param($v)$v.status.http_health -eq 'healthy'})
        Save;Write-Output 'Managed frontend/backend running. Frozen missing-node contract fails as expected.'
    }
    'integrate'{
        Assert $RunRealHarness 'integration may submit up to two real repair tasks; pass -RunRealHarness'
        Assert (-not $record.integration_id) 'Integration already submitted; query its saved ID'
        $contract=[IO.File]::ReadAllText((Join-Path $record.client 'api-contract.json'))
        $sha=[Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($contract))).ToLowerInvariant()
        $record.integration_id=[guid]::NewGuid().ToString();Save
        $spec=@{id=$record.integration_id;connection_id=$record.connection_id;backend_peer_id=$record.remote.card.node_id;backend_agent_id=$record.pi.agent_id;backend_project_id=$record.remote.project;backend_service_id=$record.service_id;contract=$contract;contract_sha256=$sha;contract_revision=1;tests=@(@{name='health-fields';path='/api/health';expected_status=200;equals=@{'/status'='ok';'/service'='orion-todo-api';'/node'='Orion'}},@{name='initial-todos';path='/api/todos';expected_status=200;equals=@{'/todos'=@()}});max_fix_cycles=2;repair_timeout_seconds=300;repair_max_output_bytes=8388608}
        $file=Config 'integration' $spec;$record.integration=Local @('integration','start','--config',$file,'--allow-model');Save
        Write-Output ('Integration submitted: '+$record.integration_id)
    }
    'status'{
        if($record.integration_id){Local @('integration','show',$record.integration_id)|Select-Object state,cycles,repair_task_ids,error}
        if($record.pi_verification){Owner @('task','show',$record.pi_verification.task_id)|Select-Object task_id,status,error}
        if($record.dsh_task){Local @('task','show',$record.dsh_task.task_id)|Select-Object task_id,status,error}
        if($record.tunnel_id){Resource 'tunnel' $record.tunnel_id}
    }
    'snapshot'{
        $snapshot=@{integration=(Local @('integration','show',$record.integration_id));dsh=(Local @('task','show',$record.dsh_task.task_id));pi_verification=(Owner @('task','show',$record.pi_verification.task_id));repairs=@();remote=(Remote 'evidence')}
        foreach($id in $snapshot.integration.repair_task_ids){$snapshot.repairs+=@(Local @('task','show',$id))}
        $record.pre_timeout_fix=$snapshot;Save
        Write-Output 'Pre-timeout-fix integration/task/artifact evidence preserved.'
    }
    'restart-final'{
        Assert $RunRealHarness 'Client budget correction calls DSH; pass -RunRealHarness'
        Assert (-not $record.final_budget_task) 'Final budget task already submitted; query its existing UUID'
        Assert $record.cleanup_utc 'Stop previous isolated Daemons before relaunch'
        $checks=Remote 'build-status';Assert ($checks.checks.Count -eq 2 -and @($checks.checks|Where-Object {$_.exit -ne 0}).Count -eq 0) 'Final Linux build/checks unresolved'
        if(-not $env:DEEPSEEK_API_KEY){$env:DEEPSEEK_API_KEY=[Environment]::GetEnvironmentVariable('DEEPSEEK_API_KEY','User')}
        $process=Start-Process -FilePath $binary -ArgumentList @('--data-dir',('"'+$record.node_dir+'"'),'daemon','start') -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $root 'daemon-final.out') -RedirectStandardError (Join-Path $root 'daemon-final.err')
        $record.daemon=@{pid=$process.Id;start_ticks=$process.StartTime.ToUniversalTime().Ticks;path=$process.Path};Save
        for($i=0;$i -lt 100;$i++){if(Test-Path -LiteralPath (Join-Path $record.node_dir 'daemon.json')){break};Start-Sleep -Milliseconds 50}
        [void](Remote 'restart-owned')
        $record.resource_history=@{service=$record.service_id;connection=$record.connection_id;frontend=$record.frontend_id;tunnel=$record.tunnel_id}
        $tunnel=Get-Content -LiteralPath (Join-Path $root 'tunnel.json') -Raw|ConvertFrom-Json -AsHashtable
        $tunnel.id=[guid]::NewGuid().ToString();$record.tunnel_id=$tunnel.id;Save
        $file=Config 'tunnel-final' $tunnel;[void](Local @('tunnel','add','--config',$file));[void](Wait-Resource 'tunnel' $record.tunnel_id {param($v)$v.status.process -eq 'running'});Start-Sleep -Seconds 1
        $record.final_fault=Remote 'final-fault' @{task_id=$record.pre_timeout_fix.repairs[0].task_id;expected_sha256=$record.pre_timeout_fix.remote.files.'server.mjs'.sha256}
        $record.integration_history=@($record.integration_history)+@($record.pre_timeout_fix.integration)
        $record.client_v1=@{before=$record.client_before;original=$record.client_original;dsh_task=$record.dsh_task}
        foreach($name in @('contract-check.mjs','serve.mjs')){Copy-Item -LiteralPath (Join-Path $record.client $name) -Destination (Join-Path $root ($name+'.before-budget'))}
        $record.final_budget_task=Local @('agent','verify',$record.dsh.agent_id,'--allow-model','--prompt','Use read and edit only. In contract-check.mjs replace the single timeout numeric literal 10_000 with 45_000. In serve.mjs replace the single PROXY_TIMEOUT_MS value 10_000 with 45_000. Preserve every other byte and all HTTP assertions, paths, messages and behavior. This aligns the client budgets with an owner-configured 30-second service operation on slow durable storage. Do not modify any other file or run commands. Finish briefly.');Save
        Write-Output 'Final binaries restarted. Same missing-field defect injected; DSH budget-only correction submitted.'
    }
    'final-services'{
        Assert ((Local @('task','show',$record.final_budget_task.task_id)).status -eq 'SUCCEEDED') 'DSH budget task unresolved'
        foreach($name in @('contract-check.mjs','serve.mjs')){
            $old=[IO.File]::ReadAllText((Join-Path $root ($name+'.before-budget')))
            $current=[IO.File]::ReadAllText((Join-Path $record.client $name))
            Assert ($old.Replace('10_000','45_000') -ceq $current) ('DSH changed more than the timeout: '+$name)
        }
        $record.budget_only_changes_verified=$true;$record.dsh_task=$record.final_budget_task
        $record.client_original['serve.mjs']=(Get-FileHash -LiteralPath (Join-Path $record.client 'serve.mjs') -Algorithm SHA256).Hash.ToLowerInvariant()
        $record.service_id=$null;$record.connection_id=$null;$record.frontend_id=$null;Save
        & $PSCommandPath -Action services
    }
    'resume-final'{
        Assert $RunRealHarness 'Client budget correction calls DSH; pass -RunRealHarness'
        Assert (-not $record.final_budget_task) 'A final task already exists; do not resubmit'
        [void](Local @('daemon','status'));[void](Owner @('daemon','status'))
        $record.final_fault=Remote 'final-fault' @{task_id=$record.pre_timeout_fix.repairs[0].task_id;expected_sha256=$record.pre_timeout_fix.remote.files.'server.mjs'.sha256}
        $record.integration_history=@($record.integration_history)+@($record.pre_timeout_fix.integration)
        $record.client_v1=@{before=$record.client_before;original=$record.client_original;dsh_task=$record.dsh_task}
        foreach($name in @('contract-check.mjs','serve.mjs')){Copy-Item -LiteralPath (Join-Path $record.client $name) -Destination (Join-Path $root ($name+'.before-budget'))}
        $record.final_budget_task=Local @('agent','verify',$record.dsh.agent_id,'--allow-model','--prompt','Use read and edit only. In contract-check.mjs replace the single timeout numeric literal 10_000 with 45_000. In serve.mjs replace the single PROXY_TIMEOUT_MS value 10_000 with 45_000. Preserve every other byte and all HTTP assertions, paths, messages and behavior. This aligns the client budgets with an owner-configured 30-second service operation on slow durable storage. Do not modify any other file or run commands. Finish briefly.');Save
        Write-Output 'Same missing-field defect re-injected; one DSH budget-only correction submitted.'
    }
    'restart-services'{
        Assert $record.budget_only_changes_verified 'DSH budget-only file changes have not been independently verified'
        Assert (-not(Test-Path -LiteralPath (Join-Path $record.node_dir 'daemon.json'))) 'Prior Windows daemon not stopped'
        $checks=Remote 'build-status';Assert ($checks.checks.Count -eq 2 -and @($checks.checks|Where-Object {$_.exit -ne 0}).Count -eq 0) 'Final Linux checks unresolved'
        if(-not $env:DEEPSEEK_API_KEY){$env:DEEPSEEK_API_KEY=[Environment]::GetEnvironmentVariable('DEEPSEEK_API_KEY','User')}
        $process=Start-Process -FilePath $binary -ArgumentList @('--data-dir',('"'+$record.node_dir+'"'),'daemon','start') -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $root 'daemon-connect-budget.out') -RedirectStandardError (Join-Path $root 'daemon-connect-budget.err')
        $record.daemon=@{pid=$process.Id;start_ticks=$process.StartTime.ToUniversalTime().Ticks;path=$process.Path};$record.cleanup=$null;$record.cleanup_utc=$null;Save
        for($i=0;$i -lt 100;$i++){if(Test-Path -LiteralPath (Join-Path $record.node_dir 'daemon.json')){break};Start-Sleep -Milliseconds 50}
        [void](Remote 'restart-owned')
        Assert ((Local @('task','show',$record.final_budget_task.task_id)).status -eq 'SUCCEEDED') 'Prior DSH task not successful'
        $record.partial_resource_history=@{service=$record.service_id;connection=$record.connection_id;tunnel=$record.tunnel_id}
        $tunnel=Get-Content -LiteralPath (Join-Path $root 'tunnel.json') -Raw|ConvertFrom-Json -AsHashtable
        $tunnel.id=[guid]::NewGuid().ToString();$record.tunnel_id=$tunnel.id;Save
        $file=Config 'tunnel-total-budget' $tunnel;[void](Local @('tunnel','add','--config',$file));[void](Wait-Resource 'tunnel' $record.tunnel_id {param($v)$v.status.process -eq 'running'});Start-Sleep -Seconds 1
        $record.service_id=$null;$record.connection_id=$null;$record.frontend_id=$null;Save
        & $PSCommandPath -Action services
    }
    'final-integrate'{
        Assert $RunRealHarness 'Final fixed-budget integration calls a real model; pass -RunRealHarness'
        Assert (-not $record.final_integration_submitted) 'Final run already submitted; query its saved UUID'
        $record.integration_id=$null;$record.final_integration_submitted=$true;Save
        & $PSCommandPath -Action integrate -RunRealHarness
    }
    'direct-route'{
        Assert $RunRealHarness 'Direct route verification calls a real model; pass -RunRealHarness'
        Assert (-not $record.direct) 'Direct route already prepared; query the existing task'
        $prior=Local @('integration','show',$record.integration_id)
        Assert ($prior.state -eq 'needs_input') 'Inspect a terminal previous integration before changing the owner route'
        foreach($id in $prior.repair_task_ids){Assert ((Local @('task','show',$id)).status -eq 'FAILED') 'Prior repair task outcome is unresolved'}
        $record.integration_history=@($prior);$record.pi_initial=$record.pi;$record.pi_verification_initial=$record.pi_verification;Save
        $record.direct=Remote 'direct-agent';Save
        $record.pi=$record.direct.agent;$record.pi_verification=$record.direct.verification;Save
        Write-Output 'New isolated Pi agent uses direct official API connectivity; previous failed task/run preserved.'
    }
    'reintegrate'{
        Assert $RunRealHarness 'New integration may submit at most two real repair tasks; pass -RunRealHarness'
        Assert (-not $record.direct_integration_submitted) 'Direct integration already submitted; query its saved UUID'
        Assert ((Owner @('task','show',$record.pi_verification.task_id)).status -eq 'SUCCEEDED') 'Direct Pi verification unresolved'
        [void](Remote 'config-cli' @{command='policy';peer=$record.local_card.node_id;agent=$record.pi.agent_id})
        $spec=Get-Content -LiteralPath (Join-Path $root 'integration.json') -Raw|ConvertFrom-Json -AsHashtable
        $record.integration_id=[guid]::NewGuid().ToString();$record.direct_integration_submitted=$true;Save
        $spec.id=$record.integration_id;$spec.backend_agent_id=$record.pi.agent_id
        $file=Config 'integration-direct' $spec
        $record.integration=Local @('integration','start','--config',$file,'--allow-model');Save
        Write-Output ('Known failed prior run retained; independent direct-route acceptance: '+$record.integration_id)
    }
    'cut-tunnel'{
        $v=Resource 'tunnel' $record.tunnel_id;Assert ($v.status.process -eq 'running') 'Tunnel not running'
        $process=Get-CimInstance Win32_Process -Filter ('ProcessId='+$v.status.pid)
        Assert ($process.ExecutablePath -eq (Get-Command ssh.exe).Source -and $process.CommandLine.Contains((Join-Path $record.node_dir 'managed-ssh-empty.conf'))) 'Refusing to stop an unowned SSH process'
        $record.tunnel_before=$v;$record.cut_utc=[DateTime]::UtcNow.ToString('o');Save
        Stop-Process -Id $v.status.pid -ErrorAction Stop
        $offlineFailed=$false
        try{[void](Invoke-RestMethod ($record.endpoint+'/api/health') -TimeoutSec 1)}catch{$offlineFailed=$true}
        $record.offline_request_failed=$offlineFailed;Save
        Write-Output 'Only this managed trial SSH child stopped; supervisor owns reconnection.'
    }
    'check-tunnel'{
        $v=Wait-Resource 'tunnel' $record.tunnel_id {param($v)$v.status.starts -gt $record.tunnel_before.status.starts -and $v.status.process -eq 'running'}
        for($i=0;$i -lt 40;$i++){try{$health=Invoke-RestMethod ($record.endpoint+'/api/health') -TimeoutSec 2;if($health.node -eq 'Orion'){break}}catch{};Start-Sleep -Milliseconds 250}
        Assert ($health.node -eq 'Orion') 'Managed endpoint did not recover'
        $record.tunnel_after=$v;$record.restored_utc=[DateTime]::UtcNow.ToString('o');Save
        Write-Output 'Same service endpoint recovered through automatic SSH process restart.'
    }
    'collect'{
        $record.integration=Local @('integration','show',$record.integration_id);Assert ($record.integration.state -eq 'verified') 'Integration not verified'
        $record.pi_verification=Owner @('task','show',$record.pi_verification.task_id)
        $record.dsh_task=Local @('task','show',$record.dsh_task.task_id)
        Assert ($record.pi_verification.status -eq 'SUCCEEDED' -and $record.dsh_task.status -eq 'SUCCEEDED') 'Harness verification/authoring did not succeed'
        $record.client_after=Client-Check;Assert ($record.client_after.exit -eq 0 -and $record.client_after.script_sha256 -eq $record.client_before.script_sha256) 'Frozen DSH client test changed or failed'
        $record.remote_evidence=Remote 'evidence';Assert ($record.remote_evidence.tests.exit -eq 0) 'Backend unit tests failed'
        Assert $record.remote_evidence.credential_files_unchanged 'Pi credential/model files changed'
        foreach($name in $record.client_original.Keys){Assert ((Get-FileHash -LiteralPath (Join-Path $record.client $name) -Algorithm SHA256).Hash.ToLowerInvariant() -eq $record.client_original[$name]) ('DSH changed existing client file: '+$name)}
        Assert ((Get-FileHash -LiteralPath (Join-Path $env:USERPROFILE '.ssh/known_hosts') -Algorithm SHA256).Hash.ToLowerInvariant() -eq $record.known_hosts_before) 'known_hosts changed'
        Assert ($record.remote_evidence.files.'api-contract.json'.sha256 -eq $record.remote_evidence.before.contract_sha256) 'Model changed the backend contract'
        Assert ($record.remote_evidence.files.'server.test.mjs'.sha256 -eq (Get-FileHash -LiteralPath (Join-Path $workspace 'examples/ssh-todo/server/server.test.mjs') -Algorithm SHA256).Hash.ToLowerInvariant()) 'Model changed backend tests'
        $record.repairs=@();foreach($id in $record.integration.repair_task_ids){$record.repairs+=@(Local @('task','show',$id))}
        $record.prior_repairs=@();foreach($prior in $record.integration_history){foreach($id in $prior.repair_task_ids){$record.prior_repairs+=@(Local @('task','show',$id))}}
        $record.repair_events=@{};foreach($task in (@($record.repairs)+@($record.prior_repairs))){$record.repair_events[$task.task_id]=@(Local @('task','events',$task.task_id)|Where-Object {$_.event_type -eq 'output_summary' -or $_.payload.type -eq 'tool_event'}|Select-Object seq,event_type,payload)}
        $record.windows_binary_sha256=(Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant()
        Save
        $check=Program 'node.exe' @((Join-Path $PSScriptRoot 'check-todo.mjs'),'http://127.0.0.1:5174',(Join-Path $root 'managed-http.json'),'','45000') -Timeout 180000
        [void]($check|ConvertFrom-Json)
        $record.client_unit_tests=Program 'node.exe' @('--test',(Join-Path $record.client 'client.test.mjs')) -Timeout 90000 -AllowFailure
        Assert ($record.client_unit_tests.exit -eq 0) 'Client unit tests failed'
        $http=Get-Content -LiteralPath (Join-Path $root 'managed-http.json') -Raw|ConvertFrom-Json
        $record.persistence_restart=Owner @('service','restart',$record.service_id)
        for($i=0;$i -lt 40;$i++){$cards=@(Local @('service','list','--peer',$record.remote.card.node_id));$card=$cards|Where-Object {$_.id -eq $record.service_id};if($card.revision -eq $record.persistence_restart.revision -and $card.health -eq 'healthy'){break};Start-Sleep -Milliseconds 250}
        Assert ($card.revision -eq $record.persistence_restart.revision -and $card.health -eq 'healthy') 'Explicit persistence restart unresolved'
        [void](Program 'node.exe' @((Join-Path $PSScriptRoot 'check-todo.mjs'),'http://127.0.0.1:5174',(Join-Path $root 'managed-persistence.json'),$http.todo.id,'45000') -Timeout 90000)
        $record.final_data=Remote 'data-evidence';Save
        Write-Output 'Real repair, frozen DSH client, backend/client tests, frontend HTTP and managed-restart persistence passed.'
    }
    'revoke'{
        [void](Owner @('service','revoke',$record.service_id))
        [void](Wait-Resource 'connection' $record.connection_id {param($v)$v.status.state -eq 'closed'})
        $blocked=$false;try{[void](Invoke-RestMethod ($record.endpoint+'/api/health') -TimeoutSec 2)}catch{$blocked=$true}
        Assert $blocked 'Revoked service remained accessible';$record.revoke_passed=$true;Save
        Write-Output 'Remote service revoked; local endpoint closed.'
    }
    'cleanup'{
        # Owner APIs stop only their managed resources and tracked process trees.
        [void](Remote 'stop');[void](Local @('daemon','stop'))
        for($i=0;$i -lt 60;$i++){
            $remoteState=Remote 'cleanup-status'
            $localProcess=Get-Process -Id $record.daemon.pid -ErrorAction SilentlyContinue
            $localAlive=$localProcess -and $localProcess.StartTime.ToUniversalTime().Ticks -eq $record.daemon.start_ticks
            if(-not $localAlive -and -not $remoteState.owned_daemon_alive -and $remoteState.enabled_resources -eq 0){break}
            Start-Sleep -Milliseconds 250
        }
        Assert (-not $localAlive -and -not $remoteState.owned_daemon_alive -and $remoteState.enabled_resources -eq 0) 'Cleanup not yet confirmed'
        $record.cleanup=@{local_daemon_alive=[bool]$localAlive;remote=$remoteState}
        $record.original_application_after=Invoke-RestMethod 'http://127.0.0.1:5173/api/health' -TimeoutSec 5
        Assert ($record.original_application_after.status -eq 'ok' -and $record.original_application_after.node -eq 'Orion') 'Original Todo application is unavailable'
        $record.cleanup_utc=[DateTime]::UtcNow.ToString('o');Save
        Write-Output 'Isolated authoring daemons/services/tunnel stopped; original Todo session unchanged.'
    }
}
