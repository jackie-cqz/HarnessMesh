[CmdletBinding()]
param(
    [string]$SshHost = 'cqz_Orion',
    [Parameter(Mandatory)][string]$RemoteRoot,
    [Parameter(Mandatory)][string]$LocalRoot,
    [int]$LocalPort = 38880,
    [int]$RemotePort = 38881,
    [string]$Model = 'deepseek-v4-flash',
    [switch]$RunRealHarness
)
$ErrorActionPreference = 'Stop'
if (-not $RunRealHarness) { throw 'This smoke calls a real model. Pass -RunRealHarness to opt in.' }
if ($RemoteRoot -notmatch '^/[a-zA-Z0-9_./-]+$' -or $RemoteRoot.Contains('/../')) { throw 'Use an absolute remote trial directory without shell syntax' }
$workspace = Split-Path -Parent $PSScriptRoot
$binary = Join-Path $workspace 'target/debug/hmesh.exe'
$LocalRoot = [IO.Path]::GetFullPath($LocalRoot)
$nodeDir = Join-Path $LocalRoot 'node-windows'
$sshConfig = Join-Path $LocalRoot 'ssh.conf'
$localDaemon = $null; $tunnel = $null; $remoteStarted = $false; $report = $null
function Assert-That($Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
function Invoke-Program([string]$File, [string[]]$Arguments) {
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $File; $start.UseShellExecute = $false; $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true; $start.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::new(); $process.StartInfo = $start
    [void]$process.Start()
    $stdout = $process.StandardOutput.ReadToEndAsync(); $stderr = $process.StandardError.ReadToEndAsync()
    if (-not $process.WaitForExit(45000)) { $process.Kill($true); $process.WaitForExit(); throw 'Observation command timed out; no task was resubmitted' }
    $output = $stdout.GetAwaiter().GetResult(); $diagnostic = $stderr.GetAwaiter().GetResult()
    Assert-That ($process.ExitCode -eq 0) ("Command failed: " + $diagnostic)
    return $output
}
function Local([string[]]$Arguments) {
    return (Invoke-Program $binary (@('--json','--data-dir',$nodeDir) + $Arguments) | ConvertFrom-Json)
}
function Remote([string]$Action, $Body = @{}) {
    $payload = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes(($Body | ConvertTo-Json -Depth 12 -Compress)))
    $command = 'python3 ' + $RemoteRoot + '/ssh-node.py ' + $Action + ' ' + $payload
    return (Invoke-Program 'ssh.exe' @('-F',$sshConfig,'-T','-o','BatchMode=yes','-o','ConnectTimeout=10','hmesh-trial',$command) | ConvertFrom-Json)
}
function Wait-Task([string]$TaskId, [switch]$OnRemote) {
    $deadline = [DateTime]::UtcNow.AddSeconds(150)
    do {
        $task = if ($OnRemote) { Remote 'cli' @{arguments=@('task','show',$TaskId)} } else { Local @('task','show',$TaskId) }
        if ($task.status -in @('SUCCEEDED','FAILED','UNKNOWN','CANCELLED','REJECTED','EXPIRED')) { return $task }
        Start-Sleep -Milliseconds 500
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Task $TaskId remains unresolved; its existing record is preserved"
}
try {
    Assert-That (-not (Test-Path -LiteralPath $nodeDir)) 'Use a fresh Windows trial instance'
    [IO.Directory]::CreateDirectory($LocalRoot) | Out-Null
    $resolved = Invoke-Program 'ssh.exe' @('-G',$SshHost)
    $settings = @{}
    foreach ($line in $resolved.Split("`n")) { if ($line -match '^(hostname|user|port|identityfile|proxyjump|proxycommand) (.+)$' -and -not $settings.ContainsKey($Matches[1])) { $settings[$Matches[1]] = $Matches[2].Trim() } }
    Assert-That ($settings.hostname -and $settings.user -and $settings.identityfile) 'SSH alias must resolve a host, user and identity file'
    Assert-That (-not $settings.proxyjump -and -not $settings.proxycommand) 'This first smoke expects a directly reachable SSH alias'
    foreach ($value in $settings.Values) { Assert-That (-not $value.Contains("`r") -and -not $value.Contains("`n") -and -not $value.Contains('"')) 'Invalid SSH option value' }
    $rows = @('Host hmesh-trial',('    HostName ' + $settings.hostname),('    User ' + $settings.user),('    Port ' + $settings.port),('    IdentityFile "' + $settings.identityfile.Replace('\','/') + '"'),'    StrictHostKeyChecking yes','    BatchMode yes')
    [IO.File]::WriteAllLines($sshConfig,$rows)
    [void](Invoke-Program 'scp.exe' @('-F',$sshConfig,(Join-Path $PSScriptRoot 'ssh-node.py'),('hmesh-trial:' + $RemoteRoot + '/ssh-node.py')))
    $remote = Remote 'init' @{port=$RemotePort;model=$Model}
    Assert-That ($remote.cargo_lock_sha256 -eq (Get-FileHash -LiteralPath (Join-Path $workspace 'Cargo.lock') -Algorithm SHA256).Hash.ToLowerInvariant()) 'Linux and Windows Cargo.lock differ'
    $localCard = Local @('init','--name','windows-ssh-smoke','--port',[string]$LocalPort)
    $localDaemon = Start-Process -FilePath $binary -ArgumentList @('--data-dir',('"'+$nodeDir+'"'),'daemon','start') -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $nodeDir 'daemon.stdout.log') -RedirectStandardError (Join-Path $nodeDir 'daemon.stderr.log')
    for ($i=0; $i -lt 100; $i++) { Assert-That (-not $localDaemon.HasExited) 'Windows Daemon exited during startup'; if (Test-Path -LiteralPath (Join-Path $nodeDir 'daemon.json')) { break }; Start-Sleep -Milliseconds 50 }
    [void](Local @('daemon','status'))
    $remoteStarted = $true; $started = Remote 'start'
    $forward = "127.0.0.1:${RemotePort}:127.0.0.1:${RemotePort}"
    $reverse = "127.0.0.1:${LocalPort}:127.0.0.1:${LocalPort}"
    $tunnel = Start-Process -FilePath 'ssh.exe' -ArgumentList @('-F',('"'+$sshConfig+'"'),'-N','-T','-o','ExitOnForwardFailure=yes','-o','ServerAliveInterval=15','-o','ServerAliveCountMax=3','-L',$forward,'-R',$reverse,'hmesh-trial') -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $LocalRoot 'tunnel.stdout.log') -RedirectStandardError (Join-Path $LocalRoot 'tunnel.stderr.log')
    Start-Sleep -Seconds 2
    Assert-That (-not $tunnel.HasExited) 'Dedicated SSH tunnel failed; inspect its private log'
    $pair = Local @('pair','request','--peer',$remote.card.endpoint,'--expect-fingerprint',$remote.card.fingerprint)
    [void](Remote 'cli' @{arguments=@('pair','approve',$pair.request_id,'--expect-fingerprint',$localCard.fingerprint)})
    [void](Local @('pair','approve',$pair.request_id,'--expect-fingerprint',$remote.card.fingerprint))
    $agent = Remote 'cli' @{arguments=@('agent','register','--config',($RemoteRoot+'/agent.toml'))}
    Assert-That ($agent.probe.version -eq '1.4.0' -and $agent.probe.version_supported) 'Remote Pi version does not match the verified protocol'
    Write-Output 'SSH/mTLS pairing passed. Running Orion owner model verification.'
    $ownerCheck = Remote 'cli' @{arguments=@('agent','verify',$agent.agent_id,'--allow-model')}
    $ownerCheck = Wait-Task $ownerCheck.task_id -OnRemote
    Assert-That ($ownerCheck.status -eq 'SUCCEEDED') ('Orion owner verification failed: ' + ($ownerCheck.error | ConvertTo-Json -Compress))
    $policy = "[default]`naction = `"deny`"`n[[grant]]`npeer = `"$($localCard.node_id)`"`nagent = `"$($agent.agent_id)`"`nproject = `"ssh-smoke`"`nactions = [`"agent.read`", `"task.submit`"]`nrequire_approval = true`n"
    # This policy contains only IDs and permissions, never credentials.
    $policyBase64 = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($policy))
    $writePolicy = "python3 -c 'import base64,pathlib;pathlib.Path(`"$RemoteRoot/policy-grant.toml`").write_bytes(base64.b64decode(`"$policyBase64`"))'"
    [void](Invoke-Program 'ssh.exe' @('-F',$sshConfig,'-T','hmesh-trial',$writePolicy))
    [void](Remote 'cli' @{arguments=@('policy','--file',($RemoteRoot+'/policy-grant.toml'))})
    $visible = @(Local @('agents','list','--peer',$remote.card.node_id))
    Assert-That ($visible.Count -eq 1 -and $visible[0].availability -eq 'idle') 'Windows cannot see the authorized remote Agent'
    $requestId = [guid]::NewGuid().ToString()
    $prompt = "Create out/health.json containing exactly a JSON object with status equal to ok, harness equal to pi, model equal to $Model, and request_id equal to $requestId. Use the write tool, then read the file back. Work only in the registered project. Do not invoke shell commands, networking, subagents, or modify other files."
    $arguments = @('task','send',$agent.agent_id,'--peer',$remote.card.node_id,'--project','ssh-smoke','--request-id',$requestId,'--timeout','120','--prompt',$prompt)
    $task = Local $arguments
    Assert-That ($task.status -eq 'PENDING_APPROVAL' -and $null -eq $task.last_attempt_id) 'Remote task bypassed owner approval'
    [void](Remote 'cli' @{arguments=@('task','approve',$task.task_id)})
    Write-Output ("Cross-machine model task started: " + $task.task_id)
    $task = Wait-Task $task.task_id
    Assert-That ($task.status -eq 'SUCCEEDED') ('Cross-machine task did not succeed: ' + ($task.error | ConvertTo-Json -Compress))
    $expected = @{status='ok';harness='pi';model=$Model;request_id=$requestId} | ConvertTo-Json -Compress
    $verification = Remote 'cli' @{arguments=@('task','verify',$task.task_id,'--artifact','out/health.json','--expect-json',$expected)}
    Assert-That $verification.passed 'Deterministic remote verification failed'
    $artifact = Remote 'artifact'
    Assert-That ($artifact.json.status -eq 'ok' -and $artifact.json.request_id -eq $requestId -and $artifact.json.model -eq $Model -and $artifact.sha256 -eq $verification.artifact.sha256) 'Independent SSH content/hash verification failed'
    $shared = @(Local @('task','verification',$task.task_id)); Assert-That $shared[-1].passed 'Windows could not read task verification through the Mesh'
    $events = @(Local @('task','events',$task.task_id)); $seq=0
    foreach ($event in $events) { Assert-That ($event.seq -eq ($seq+1)) 'Committed event sequence has a gap'; $seq=$event.seq }
    $cursor=[int][Math]::Floor($events.Count/2)
    $replay=@(Local @('task','events',$task.task_id,'--after',[string]$cursor)); Assert-That ($replay.Count -eq $events.Count-$cursor) 'Event replay missed records'
    $duplicate=Local $arguments
    Assert-That ($duplicate.task_id -eq $task.task_id -and $duplicate.last_attempt_id -eq $task.last_attempt_id) 'Duplicate request created another execution'
    $after=@(Local @('task','events',$task.task_id)); Assert-That ($after.Count -eq $events.Count) 'Duplicate request produced additional events'
    $afterArtifact=Remote 'artifact'; Assert-That ($afterArtifact.sha256 -eq $artifact.sha256) 'Duplicate request changed the remote artifact'
    Assert-That (-not (Test-Path -LiteralPath (Join-Path $nodeDir 'out/health.json'))) 'Remote artifact was misrouted into the Windows instance'
    $trace=@($events | ForEach-Object { [ordered]@{seq=$_.seq;event_type=$_.event_type;kind=$_.payload.type;status=$_.payload.status;tool=$_.payload.name;phase=$_.payload.phase;sha256=$_.payload.sha256} })
    $report=[ordered]@{schema_version=1;date=(Get-Date -Format 'yyyy-MM-dd');scope='Windows to Orion Linux over SSH with end-to-end HTTPS/mTLS';status='passed';ssh_host=$SshHost;local_os=[Environment]::OSVersion.VersionString;remote_os=$remote.os;remote_arch=$remote.arch;model=$Model;provider='deepseek';pi_version=$agent.probe.version;windows_binary_sha256=(Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant();linux_binary_sha256=$remote.binary_sha256;cargo_lock_sha256=(Get-FileHash -LiteralPath (Join-Path $workspace 'Cargo.lock') -Algorithm SHA256).Hash.ToLowerInvariant();origin_node_id=$task.origin_node_id;execution_node_id=$task.target_node_id;request_id=$requestId;task_id=$task.task_id;attempt_id=$task.last_attempt_id;owner_verification_task_id=$ownerCheck.task_id;verification=$verification;independent_artifact=$artifact;event_trace=$trace;dedup_passed=$true;replay_passed=$true;tailscale='not_tested';reverse_task_delegation='not_tested';native_harness_abort='not_tested'}
    $report.source_archive_sha256 = $remote.source_archive_sha256
    if (Test-Path -LiteralPath (Join-Path $LocalRoot 'vendor.sha256')) {
        $report.vendor_archive_sha256 = [IO.File]::ReadAllText((Join-Path $LocalRoot 'vendor.sha256'))
    }
} finally {
    $cleanup=@{}
    if ($remoteStarted) { try { $cleanup.remote=Remote 'stop' } catch { $cleanup.remote=@{error=$_.Exception.Message} } }
    if ($localDaemon) {
        try { [void](Local @('daemon','stop')) } catch { Write-Warning 'Graceful local stop failed; stopping only this script-owned process handle' }
        if (-not $localDaemon.WaitForExit(10000)) { $localDaemon.Kill($true); $localDaemon.WaitForExit() }
        $cleanup.windows_daemon_stopped=$localDaemon.HasExited
    }
    if ($tunnel) { if (-not $tunnel.HasExited) { $tunnel.Kill(); $tunnel.WaitForExit() }; $cleanup.dedicated_tunnel_stopped=$tunnel.HasExited }
    if ($report) {
        $report.cleanup=$cleanup
        Assert-That ($cleanup.remote.daemon_stopped -and $cleanup.remote.personal_auth_unchanged -and $cleanup.windows_daemon_stopped -and $cleanup.dedicated_tunnel_stopped) 'Trial cleanup or private-auth integrity check failed'
        $evidence=Join-Path $workspace 'docs/evidence/latest-ssh-smoke.json'
        [IO.File]::WriteAllText($evidence,($report | ConvertTo-Json -Depth 14))
        Write-Output "Real cross-machine smoke passed. Evidence: $evidence"
    }
}
