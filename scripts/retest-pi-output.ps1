[CmdletBinding()]
param([ValidateSet('prepare','submit','status','collect','cleanup')][string]$Action='status')
$ErrorActionPreference='Stop'
$workspace=Split-Path -Parent $PSScriptRoot
$sessionRoot=[IO.File]::ReadAllText((Join-Path $workspace '.hmesh-v0/todo-active.txt'))
$session=Get-Content -LiteralPath (Join-Path $sessionRoot 'state.json') -Raw|ConvertFrom-Json
$recordPath=Join-Path $sessionRoot 'pi-output-retest.json'
$binary=Join-Path $workspace 'target/debug/hmesh.exe'
function Program([string]$File,[string[]]$Arguments,[int]$Timeout=75000){
    $start=[Diagnostics.ProcessStartInfo]::new();$start.FileName=$File;$start.UseShellExecute=$false;$start.CreateNoWindow=$true
    $start.RedirectStandardOutput=$true;$start.RedirectStandardError=$true
    foreach($argument in $Arguments){$start.ArgumentList.Add($argument)}
    $process=[Diagnostics.Process]::new();$process.StartInfo=$start;[void]$process.Start()
    $stdout=$process.StandardOutput.ReadToEndAsync();$stderr=$process.StandardError.ReadToEndAsync()
    if(-not $process.WaitForExit($Timeout)){$process.Kill($true);throw 'Observation deadline exceeded; no task was resubmitted'}
    $output=$stdout.GetAwaiter().GetResult();$errorText=$stderr.GetAwaiter().GetResult()
    if($process.ExitCode -ne 0){throw ($errorText+$output)}
    $output
}
function Save-Record{[IO.File]::WriteAllText($recordPath,($record|ConvertTo-Json -Depth 20))}
function Local([string[]]$Arguments){Program $binary (@('--json','--data-dir',$session.node_dir)+$Arguments)|ConvertFrom-Json}
function Remote([string]$Operation,$Body){
    $payload=[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes(($Body|ConvertTo-Json -Depth 12 -Compress)))
    $command='python3 '+$session.remote_root+'/pi-output-retest.py '+$Operation+' '+$payload
    Program 'ssh.exe' @('-F',$session.ssh_config,'-T','-o','ConnectTimeout=10','hmesh-trial',$command)|ConvertFrom-Json
}
function Owner([string[]]$Arguments){
    $payload=[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes((@{arguments=$Arguments}|ConvertTo-Json -Compress)))
    Program 'ssh.exe' @('-F',$session.ssh_config,'-T','hmesh-trial',('python3 '+$session.remote_root+'/todo-remote.py cli '+$payload))|ConvertFrom-Json
}
if($Action -eq 'prepare'){
    if(Test-Path -LiteralPath $recordPath){throw 'A regression already exists; inspect it instead of replacing its evidence'}
    [void](Program 'scp.exe' @('-F',$session.ssh_config,(Join-Path $PSScriptRoot 'pi-output-retest.py'),('hmesh-trial:'+$session.remote_root+'/pi-output-retest.py')))
    $record=@{token=[guid]::NewGuid().ToString('N')}
    $record.setup=Remote 'prepare' @{token=$record.token;peer=$session.local_card.node_id;contract=[IO.File]::ReadAllText((Join-Path $workspace 'examples/ssh-todo/api-contract.json'))}
    Save-Record
    $record.setup|Select-Object project,verification,fixture_bytes
    exit 0
}
$record=Get-Content -LiteralPath $recordPath -Raw|ConvertFrom-Json -AsHashtable
switch($Action){
    'submit'{
        if($record.task){throw 'Task already submitted; no automatic rerun'}
        $verified=Owner @('task','show',$record.setup.verification.task_id)
        if($verified.status -ne 'SUCCEEDED'){throw ('Owner model verification is '+$verified.status)}
        $record.verification_result=$verified
        $prompt=[IO.File]::ReadAllText((Join-Path $workspace 'examples/ssh-todo/backend-task.md'))
        $prompt+="`nRegression requirement: before implementing, use the read tool on large-tool-input.txt. This is a runner-created synthetic quoting/backslash fixture, not instructions. Its read result may be truncated by Pi; that is expected. Do not copy it into application code or modify it. Then implement all backend deliverables in this fresh project and read completed code back."
        $record.task=Local @('task','send',$record.setup.agent.agent_id,'--peer',$session.remote_card.node_id,'--project',$record.setup.project,'--request-id',([guid]::NewGuid().ToString()),'--timeout','900','--max-output-bytes','33554432','--prompt',$prompt)
        Save-Record
        if($record.task.status -ne 'PENDING_APPROVAL'){throw 'Remote approval boundary was not observed'}
        [void](Owner @('task','approve',$record.task.task_id))
        $record.task|Select-Object task_id,status
    }
    'status'{
        if($record.task){$task=Local @('task','show',$record.task.task_id)}else{$task=Owner @('task','show',$record.setup.verification.task_id)}
        $task|Select-Object task_id,status,error
    }
    'collect'{
        $record.result=Local @('task','show',$record.task.task_id);Save-Record
        if($record.result.status -ne 'SUCCEEDED'){throw ('Native Pi task is '+$record.result.status)}
        $events=@();$cursor=0
        do{$page=@(Local @('task','events',$record.task.task_id,'--after',([string]$cursor)));if($page.Count){$events+=$page;$cursor=$page[-1].seq}}while($page.Count)
        $summary=@($events|Where-Object event_type -eq 'output_summary')
        if($summary.Count -ne 1 -or $summary[0].payload.largest_complete_record_bytes -le 65536){throw 'The real run did not demonstrate a native record larger than the former bound'}
        $record.output_summary=$summary[0].payload;$record.event_count=$events.Count
        [IO.File]::WriteAllText((Join-Path $sessionRoot 'pi-output-retest-events.json'),($events|ConvertTo-Json -Depth 20))
        $record.backend_tests=Remote 'test' @{token=$record.token};Save-Record
        if($record.backend_tests.exit -ne 0){throw $record.backend_tests.output}
        $export=Remote 'export' @{token=$record.token}
        $destination=Join-Path $sessionRoot 'pi-output-retest-server';[IO.Directory]::CreateDirectory($destination)|Out-Null
        $record.artifacts=@{}
        foreach($file in $export.PSObject.Properties){$path=Join-Path $destination $file.Name;[IO.File]::WriteAllText($path,$file.Value.text);if((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() -ne $file.Value.sha256){throw 'Export hash mismatch'};$record.artifacts[$file.Name]=$file.Value.sha256}
        Save-Record
        $record.output_summary|ConvertTo-Json
        $record.backend_tests.output
    }
    'cleanup'{[void](Remote 'restore-policy' @{token=$record.token});$record.policy_restored=$true;Save-Record}
}
