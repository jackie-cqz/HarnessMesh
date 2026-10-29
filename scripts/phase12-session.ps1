[CmdletBinding()]
param([ValidateSet('prepare','grant-windows','status','forward','reverse','cut','restore','check-reverse','parent','approve-child','collect','cleanup')][string]$Action='status')
$ErrorActionPreference='Stop'
$workspace=Split-Path -Parent $PSScriptRoot
$sessionRoot=[IO.File]::ReadAllText((Join-Path $workspace '.hmesh-v0/todo-active.txt'))
$sessionPath=Join-Path $sessionRoot 'state.json'
$session=Get-Content -LiteralPath $sessionPath -Raw|ConvertFrom-Json -AsHashtable
$recordPath=Join-Path $sessionRoot 'phase12.json'
$binary=Join-Path $workspace 'target/debug/hmesh.exe'
function Assert($Condition,[string]$Message){if(-not $Condition){throw $Message}}
function Program([string]$File,[string[]]$Arguments,[int]$Timeout=75000){
    $start=[Diagnostics.ProcessStartInfo]::new();$start.FileName=$File;$start.UseShellExecute=$false;$start.CreateNoWindow=$true
    $start.RedirectStandardOutput=$true;$start.RedirectStandardError=$true
    foreach($argument in $Arguments){$start.ArgumentList.Add($argument)}
    $process=[Diagnostics.Process]::new();$process.StartInfo=$start;[void]$process.Start()
    $stdout=$process.StandardOutput.ReadToEndAsync();$stderr=$process.StandardError.ReadToEndAsync()
    if(-not $process.WaitForExit($Timeout)){$process.Kill($true);throw 'Observation timed out; no task was resubmitted'}
    $output=$stdout.GetAwaiter().GetResult();$errorText=$stderr.GetAwaiter().GetResult()
    Assert ($process.ExitCode -eq 0) ($errorText+$output);$output
}
function Save{[IO.File]::WriteAllText($recordPath,($record|ConvertTo-Json -Depth 24))}
function Save-Session{[IO.File]::WriteAllText($sessionPath,($session|ConvertTo-Json -Depth 20))}
function Local([string[]]$Arguments){Program $binary (@('--json','--data-dir',$session.node_dir)+$Arguments)|ConvertFrom-Json}
function SSH-Json([string]$Helper,[string]$Operation,$Body){
    $payload=[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes(($Body|ConvertTo-Json -Depth 16 -Compress)))
    Program 'ssh.exe' @('-F',$session.ssh_config,'-T','-o','ConnectTimeout=10','hmesh-trial',('python3 '+$session.remote_root+'/'+$Helper+' '+$Operation+' '+$payload))|ConvertFrom-Json
}
function Remote([string]$Operation,$Body=@{}){SSH-Json 'todo-remote.py' $Operation $Body}
function Owner([string[]]$Arguments){Remote 'cli' @{arguments=$Arguments}}
function Phase([string]$Operation,$Body=@{}){$Body.token=$record.token;SSH-Json 'phase12-remote.py' $Operation $Body}
function Events([string]$Id,[bool]$OnOrion=$false){
    $all=@();$cursor=0
    do{$eventArgs=@('task','events',$Id,'--after',([string]$cursor));$page=if($OnOrion){@(Owner $eventArgs)}else{@(Local $eventArgs)};if($page.Count){$all+=$page;$cursor=$page[-1].seq}}while($page.Count)
    return ,$all
}
function Artifact([string]$Relative){
    $path=Join-Path $record.worker_root $Relative;$data=[IO.File]::ReadAllText($path)
    @{json=($data|ConvertFrom-Json);sha256=(Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant();bytes=(Get-Item -LiteralPath $path).Length}
}
function Ready($Task,[bool]$OnOrion){$state=if($OnOrion){Owner @('task','show',$Task.task_id)}else{Local @('task','show',$Task.task_id)};Assert ($state.status -eq 'SUCCEEDED') ('Model/task unresolved: '+($state|Select-Object task_id,status,error|ConvertTo-Json -Compress))}
function Start-MeshTunnel{
    $tunnelArgs=@('-F',('"'+$session.ssh_config+'"'),'-N','-T','-o','ExitOnForwardFailure=yes','-o','ServerAliveInterval=15','-o','ServerAliveCountMax=3','-L','127.0.0.1:38881:127.0.0.1:38881','-R','127.0.0.1:38880:127.0.0.1:38880','hmesh-trial')
    $process=Start-Process ssh.exe -ArgumentList $tunnelArgs -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $sessionRoot 'phase12-tunnel.out') -RedirectStandardError (Join-Path $sessionRoot 'phase12-tunnel.err')
    $session.mesh_tunnel=@{pid=$process.Id;start_ticks=$process.StartTime.ToUniversalTime().Ticks;path=$process.Path};Save-Session
    Start-Sleep -Seconds 2;Assert (-not $process.HasExited) 'Mesh tunnel did not start'
}
function Set-WindowsGrant{
    $policyPath=Join-Path $session.node_dir 'policy.toml';$beforePath=Join-Path $record.root 'before-policy.toml'
    if(-not(Test-Path -LiteralPath $beforePath)){Copy-Item -LiteralPath $policyPath -Destination $beforePath}
    $policy=[regex]::Replace([IO.File]::ReadAllText($beforePath),'(?m)^grant\s*=\s*\[\s*\]\s*\r?\n','')
    $policy+="`n[[grant]]`npeer=`"$($session.remote_card.node_id)`"`nagent=`"$($record.dsh.agent_id)`"`nproject=`"$($record.dsh_project)`"`nactions=[`"agent.read`",`"task.submit`"]`nrequire_approval=true`n"
    $policyFile=Join-Path $record.root 'policy.toml';[IO.File]::WriteAllText($policyFile,$policy);[void](Local @('policy','--file',$policyFile))
    $record.policy_after_sha256=(Get-FileHash -LiteralPath $policyPath -Algorithm SHA256).Hash;Save
}
if($Action -eq 'prepare'){
    Assert (-not(Test-Path -LiteralPath $recordPath)) 'Phase run exists; do not replace its evidence'
    $record=@{token=[guid]::NewGuid().ToString('N')}
    $record.root=Join-Path $sessionRoot ('phase12-'+$record.token);[IO.Directory]::CreateDirectory($record.root)|Out-Null
    $record.worker_root=Join-Path $record.root 'dsh-worker';[IO.Directory]::CreateDirectory($record.worker_root)|Out-Null
    $record.dsh_project='phase12-dsh-'+$record.token
    $record.reverse_expected_path='reverse.json'
    $rows=[IO.File]::ReadAllLines((Join-Path $sessionRoot 'dsh.toml'))|ForEach-Object{if($_ -match '^name='){'name="windows-phase12-dsh"'}elseif($_ -match '^project_id='){'project_id="'+$record.dsh_project+'"'}elseif($_ -match '^root='){'root='+($record.worker_root.Replace('\','/')|ConvertTo-Json -Compress)}elseif($_ -match '^artifact_paths='){'artifact_paths=["reverse.json"]'}else{$_}}
    $config=Join-Path $record.root 'dsh.toml';[IO.File]::WriteAllLines($config,$rows)
    $record.dsh=Local @('agent','register','--config',$config);$record.dsh_verification=Local @('agent','verify',$record.dsh.agent_id,'--allow-model');Save
    [void](Program 'scp.exe' @('-F',$session.ssh_config,(Join-Path $PSScriptRoot 'phase12-remote.py'),('hmesh-trial:'+$session.remote_root+'/phase12-remote.py')))
    $record.remote=Phase 'init' @{windows_node=$session.local_card.node_id;dsh_agent=$record.dsh.agent_id;dsh_project=$record.dsh_project};Save
    Set-WindowsGrant
    Write-Output 'Fresh workers, native-MCP parent and per-project approval grants prepared.';exit 0
}
$record=Get-Content -LiteralPath $recordPath -Raw|ConvertFrom-Json -AsHashtable
switch($Action){
    'grant-windows'{Set-WindowsGrant;Write-Output 'Scoped Windows grant installed; existing model verification reused.'}
    'status'{
        foreach($task in @(@{label='DSH verify';task=$record.dsh_verification;remote=$false},@{label='Pi verify';task=$record.remote.worker.verification;remote=$true},@{label='MCP verify';task=$record.remote.parent.verification;remote=$true},@{label='forward';task=$record.forward;remote=$false},@{label='reverse';task=$record.reverse;remote=$true},@{label='parent';task=$record.parent;remote=$true})){
            if($task.task){try{$value=if($task.remote){Owner @('task','show',$task.task.task_id)}else{Local @('task','show',$task.task.task_id)};[pscustomobject]@{kind=$task.label;task_id=$value.task_id;status=$value.status;error=$value.error}}catch{[pscustomobject]@{kind=$task.label;status='unreachable';error=$_.Exception.Message}}}
        }
    }
    'forward'{
        Ready $record.remote.worker.verification $true
        Assert (-not $record.forward_id -or $record.forward) 'A forward submission has an unresolved outcome; inspect its saved request ID instead of generating another'
        if($record.forward){$prior=Local @('task','show',$record.forward.task_id);Assert ($prior.status -in @('CANCELLED','FAILED')) 'Existing forward must be inspected, not resubmitted';$record.forward_history=@($record.forward_history|Where-Object {$_})+@($prior)}
        $record.forward_id=[guid]::NewGuid().ToString();$record.forward_nonce=[guid]::NewGuid().ToString();Save
        $record.forward=Local @('task','send',$record.remote.worker.agent.agent_id,'--peer',$session.remote_card.node_id,'--project',$record.remote.worker.project,'--request-id',$record.forward_id,'--timeout','300','--prompt',('Use only write/read tools in this test project. Write out/forward.json with exactly {"status":"ok","direction":"windows-to-orion","nonce":"'+$record.forward_nonce+'"}. Read it back and finish. Do not execute commands or access unrelated paths.'));Save
        Assert ($record.forward.status -eq 'PENDING_APPROVAL') 'Forward approval boundary missing';[void](Owner @('task','approve',$record.forward.task_id))
        $record.forward|Select-Object task_id,status
    }
    'reverse'{
        Ready $record.forward $false
        Assert (-not $record.reverse_id -or $record.reverse) 'A reverse submission has an unresolved outcome; inspect its saved request ID'
        Ready $record.dsh_verification $false;Assert (-not $record.reverse) 'Reverse already submitted'
        $record.reverse_id=[guid]::NewGuid().ToString();$record.reverse_nonce=[guid]::NewGuid().ToString()
        $record.reverse_arguments=@('task','send',$record.dsh.agent_id,'--peer',$session.local_card.node_id,'--project',$record.dsh_project,'--request-id',$record.reverse_id,'--timeout','300','--prompt',('Use only write/read tools in this isolated project. Write reverse.json directly at project root with exactly {"status":"ok","direction":"orion-to-windows","nonce":"'+$record.reverse_nonce+'"}. Read the saved file back, check all three fields carefully, then finish with a short summary. No commands, network calls, credentials or unrelated paths.'))
        Save;$record.reverse=Owner $record.reverse_arguments;Save
        Assert ($record.reverse.status -eq 'PENDING_APPROVAL') 'Reverse approval boundary missing';[void](Local @('task','approve',$record.reverse.task_id))
        $record.reverse|Select-Object task_id,status
    }
    'cut'{
        Assert (-not $record.cut_utc) 'This cut already happened; inspect the recorded task'
        $task=Local @('task','show',$record.reverse.task_id);Assert ($task.status -eq 'RUNNING') 'Cut requires observed RUNNING; do not claim an in-flight interruption'
        $record.before_cut=$task;$record.before_cut_events=Events $task.task_id;$record.before_cut_cursor=$record.before_cut_events[-1].seq;Save
        $process=Get-Process -Id $session.mesh_tunnel.pid -ErrorAction SilentlyContinue
        Assert ($process -and $process.StartTime.ToUniversalTime().Ticks -eq $session.mesh_tunnel.start_ticks) 'Wrong tunnel process'
        $record.tunnel_cut_utc=[DateTime]::UtcNow.ToString('o');Save
        $process.Kill();[void]$process.WaitForExit(10000)
        [void](Remote 'stop' @{component='daemon'});$record.origin_daemon_stopped=$true;$record.cut_utc=[DateTime]::UtcNow.ToString('o');Save
        Write-Output 'Mesh tunnel cut and Orion origin Daemon stopped; Windows execution continues.'
    }
    'restore'{
        Assert (-not $record.restored_utc) 'Already restored; inspect status rather than replacing the tunnel record'
        $record.completed_offline=Local @('task','show',$record.reverse.task_id);Assert ($record.completed_offline.status -eq 'SUCCEEDED') 'Execution has not succeeded offline'
        $expected=if($record.reverse_expected_path){$record.reverse_expected_path}else{'out/reverse.json'}
        $relative=if(Test-Path -LiteralPath (Join-Path $record.worker_root $expected)){$expected}else{'reverse.json'}
        $record.reverse_artifact_path=$relative;$record.reverse_artifact_before=Artifact $relative
        if($relative -ne $expected){$record.initial_reverse_artifact_path_mismatch=$true;[void](Local @('agent','artifacts',$record.dsh.agent_id,'--path',$relative))}
        Save
        try{[void](Owner @('daemon','status'))}catch{[void](Remote 'daemon-start')}
        Start-MeshTunnel;$record.restored_utc=[DateTime]::UtcNow.ToString('o');Save
        Write-Output 'Origin Daemon restarted with existing identity/database; tunnel restored.'
    }
    'check-reverse'{
        $record.reverse_result=Owner @('task','show',$record.reverse.task_id);Assert ($record.reverse_result.status -eq 'SUCCEEDED') 'Recovered origin did not observe success'
        $replay=Events $record.reverse.task_id $true;$localEvents=Events $record.reverse.task_id
        Assert (($replay.seq -join ',') -eq ($localEvents.seq -join ',')) 'Recovered event stream has gaps'
        $record.replayed_after_cut=@($replay|Where-Object seq -gt $record.before_cut_cursor).Count
        $duplicate=Owner $record.reverse_arguments
        Assert ($duplicate.task_id -eq $record.reverse.task_id -and $duplicate.last_attempt_id -eq $record.completed_offline.last_attempt_id) 'Duplicate request created another attempt'
        $after=Events $record.reverse.task_id;$artifact=Artifact $record.reverse_artifact_path
        Assert ($after.Count -eq $localEvents.Count -and $artifact.sha256 -eq $record.reverse_artifact_before.sha256) 'Duplicate request changed execution evidence'
        Assert ($artifact.json.nonce -eq $record.reverse_nonce) 'Reverse artifact nonce mismatch'
        $record.reverse_verification=Local @('task','verify',$record.reverse.task_id,'--artifact',$record.reverse_artifact_path,'--expect-json',(@{status='ok';direction='orion-to-windows';nonce=$record.reverse_nonce}|ConvertTo-Json -Compress))
        Assert $record.reverse_verification.passed 'Actual reverse artifact failed independent verification'
        $record.reverse_event_count=$replay.Count;$record.reverse_dedup=$true;$record.reverse_artifact=$artifact;Save
        [IO.File]::WriteAllText((Join-Path $record.root 'reverse-events.json'),($replay|ConvertTo-Json -Depth 20))
        Write-Output 'Reverse execution, offline completion, origin restart, replay and deduplication passed.'
    }
    'parent'{
        Ready $record.remote.parent.verification $true;Assert (-not $record.parent) 'MCP parent already submitted'
        Assert (-not $record.parent_request_id) 'Parent submission outcome unresolved; inspect its saved request ID'
        [void](Local @('agent','artifacts',$record.dsh.agent_id,'--path','child.json'))
        $record.child_request_id=[guid]::NewGuid().ToString();$record.child_nonce=[guid]::NewGuid().ToString();$record.parent_request_id=[guid]::NewGuid().ToString();Save
        $goal='Use the native harnessmesh MCP tools yourself. First call mesh_list_peers and mesh_list_agents. Select the allowed Windows DSH worker. Submit exactly ONE task with stable request_id '+$record.child_request_id+', timeout_seconds=300, max_output_bytes=1048576. Child goal: use only write/read in its project, write child.json directly at project root with exactly {"status":"ok","origin":"pi-native-mcp","nonce":"'+$record.child_nonce+'"}, read it back, and include the nonce in a concise final answer. Do not use bash or commands. Independent owner approval will be handled externally; you cannot approve it. Poll mesh_get_task with wait_seconds=10 (continue during PENDING_APPROVAL). After SUCCEEDED, consume event pages using next_after until an empty page, find its artifact sha256, and write out/delegated.json with child_task_id, child_status, nonce, child_sha256. Only mark child_status SUCCEEDED if observed. Do not simulate tool calls, choose another UUID, edit the child project yourself, or access credentials. End concisely.'
        $record.parent=Local @('task','send',$record.remote.parent.agent.agent_id,'--peer',$session.remote_card.node_id,'--project',$record.remote.parent.project,'--request-id',$record.parent_request_id,'--timeout','900','--max-output-bytes','33554432','--prompt',$goal);Save
        Assert ($record.parent.status -eq 'PENDING_APPROVAL') 'Parent approval boundary missing';[void](Owner @('task','approve',$record.parent.task_id))
        $record.parent|Select-Object task_id,status
    }
    'approve-child'{
        $links=@(Phase 'delegations' @{parent=$record.parent.task_id});Assert ($links.Count -eq 1) 'Parent has not delegated exactly one child yet'
        $record.child_before_approval=Local @('task','show',$links[0].task_id)
        Assert ($record.child_before_approval.status -eq 'PENDING_APPROVAL' -and -not $record.child_before_approval.last_attempt_id) 'Child executed without independent approval'
        Save;[void](Local @('task','approve',$links[0].task_id));$record.child_id=$links[0].task_id;Save
        Write-Output 'Observed pending child with no attempt; independently approved on Windows.'
    }
    'collect'{
        Ready $record.forward $false;Ready $record.parent $true
        $record.forward_result=Local @('task','show',$record.forward.task_id);$record.forward_artifact=Phase 'artifact' @{which='forward'}
        Assert ($record.forward_artifact.json.nonce -eq $record.forward_nonce) 'Forward artifact nonce mismatch'
        $record.parent_result=Owner @('task','show',$record.parent.task_id);$record.parent_events=Events $record.parent.task_id $true
        $record.child_result=Local @('task','show',$record.child_id);Assert ($record.child_result.status -eq 'SUCCEEDED') 'Child did not succeed'
        $record.child_artifact=Artifact 'child.json';$record.parent_artifact=Phase 'artifact' @{which='parent'}
        Assert ($record.child_artifact.json.nonce -eq $record.child_nonce -and $record.parent_artifact.json.nonce -eq $record.child_nonce -and $record.parent_artifact.json.child_task_id -eq $record.child_id -and $record.parent_artifact.json.child_status -eq 'SUCCEEDED' -and $record.parent_artifact.json.child_sha256 -eq $record.child_artifact.sha256) 'Delegated artifact evidence mismatch'
        $record.forward_verification=Owner @('task','verify',$record.forward.task_id,'--artifact','out/forward.json','--expect-json',(@{status='ok';direction='windows-to-orion';nonce=$record.forward_nonce}|ConvertTo-Json -Compress))
        $record.child_verification=Local @('task','verify',$record.child_id,'--artifact','child.json','--expect-json',(@{status='ok';origin='pi-native-mcp';nonce=$record.child_nonce}|ConvertTo-Json -Compress))
        Assert ($record.forward_verification.passed -and $record.child_verification.passed) 'Independent owner artifact assertions failed'
        $record.native_mesh_tool_names=@($record.parent_events|Where-Object {$_.payload.type -eq 'tool_event' -and $_.payload.name -like 'mcp__harnessmesh__*'}|ForEach-Object {$_.payload.name}|Sort-Object -Unique)
        Assert ($record.native_mesh_tool_names.Count -ge 3) 'Real native MCP calls were not observed'
        [IO.File]::WriteAllText((Join-Path $record.root 'parent-events.json'),($record.parent_events|ConvertTo-Json -Depth 20));$record.Remove('parent_events');Save
        [pscustomobject]@{forward=$record.forward_result.status;reverse=$record.reverse_result.status;parent=$record.parent_result.status;child=$record.child_result.status;native_tools=$record.native_mesh_tool_names}|ConvertTo-Json
    }
    'cleanup'{
        [void](Phase 'restore-policy')
        $policyPath=Join-Path $session.node_dir 'policy.toml';Assert ((Get-FileHash -LiteralPath $policyPath -Algorithm SHA256).Hash -eq $record.policy_after_sha256) 'Windows policy changed outside this run'
        [void](Local @('policy','--file',(Join-Path $record.root 'before-policy.toml')));$record.policies_restored=$true;Save
        Write-Output 'Pre-smoke policies restored on both nodes.'
    }
}
