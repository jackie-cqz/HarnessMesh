[CmdletBinding()]
param(
    [switch]$RunRealHarnesses,
    [string]$DataRoot,
    [string]$EvidencePath,
    [string]$Toolchain = 'stable',
    [string]$Model = 'deepseek-v4-pro'
)
$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $workspace
$processes = @()
$nodes = @()
$harnessMetadata = @{}
$binary = Join-Path $workspace 'target\debug\hmesh.exe'
function Assert-That($Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
function Invoke-Hmesh([string]$Node, [string[]]$Arguments, [switch]$ExpectFailure) {
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $binary
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in (@('--json', '--data-dir', $Node) + $Arguments)) { $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    [void]$process.Start()
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    if (-not $process.WaitForExit(40000)) { $process.Kill($true); $process.WaitForExit(); throw 'hmesh command exceeded its observation deadline; no task was resubmitted' }
    $output = $stdout.GetAwaiter().GetResult()
    $errorOutput = $stderr.GetAwaiter().GetResult()
    if ($ExpectFailure) {
        Assert-That ($process.ExitCode -ne 0) 'Expected the request to be rejected'
        return ($errorOutput | ConvertFrom-Json)
    }
    Assert-That ($process.ExitCode -eq 0) ("hmesh command failed: " + $errorOutput)
    if ($output.Trim()) { return ($output | ConvertFrom-Json) }
}
function Wait-Task([string]$Node, [string]$TaskId) {
    $deadline = [DateTime]::UtcNow.AddSeconds(120)
    do {
        $task = Invoke-Hmesh $Node @('task', 'show', $TaskId)
        if ($task.status -in @('SUCCEEDED', 'FAILED', 'UNKNOWN', 'REJECTED', 'CANCELLED', 'EXPIRED')) { return $task }
        Start-Sleep -Milliseconds 150
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Task $TaskId is still unresolved; preserved its record and did not restart it"
}
function Start-Node([string]$Node, [string]$Name) {
    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = $listener.LocalEndpoint.Port
    $listener.Stop()
    $card = Invoke-Hmesh $Node @('init', '--name', $Name, '--port', [string]$port)
    $arguments = @('--data-dir', ('"' + $Node + '"'), 'daemon', 'start')
    $process = Start-Process -FilePath $binary -ArgumentList $arguments -WorkingDirectory $workspace -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $Node 'daemon.stdout.log') -RedirectStandardError (Join-Path $Node 'daemon.stderr.log')
    $script:processes += $process
    $script:nodes += $Node
    for ($i = 0; $i -lt 100; $i++) {
        Assert-That (-not $process.HasExited) 'Daemon exited during startup'
        if (Test-Path -LiteralPath (Join-Path $Node 'daemon.json')) { [void](Invoke-Hmesh $Node @('daemon', 'status')); return $card }
        Start-Sleep -Milliseconds 50
    }
    throw 'Daemon did not publish a listener'
}
function Write-Agent([string]$Kind, [string]$Node, [string]$Project) {
    [IO.Directory]::CreateDirectory($Project) | Out-Null
    $executable = (Get-Command ($Kind + '.cmd') -ErrorAction Stop).Source.Replace('\', '/')
    $projectText = $Project.Replace('\', '/') | ConvertTo-Json -Compress
    $executableText = $executable | ConvertTo-Json -Compress
    $provider = if ($Kind -eq 'dsh') { 'deepseek-official' } else { 'deepseek' }
    $packageName = switch ($Kind) { 'pi' { '@earendil-works/pi-coding-agent' }; 'dsh' { '@deepseek-ai/dsh' }; 'opencode' { 'opencode-ai' } }
    $packageRoot = Join-Path (Split-Path -Parent $executable) ('node_modules/' + $packageName)
    $package = Get-Content -LiteralPath (Join-Path $packageRoot 'package.json') -Raw | ConvertFrom-Json
    $relativeEntry = switch ($Kind) { 'pi' { 'dist/cli.js' }; 'dsh' { 'lib/bin.js' }; 'opencode' { 'bin/opencode.exe' } }
    $entry = Join-Path $packageRoot $relativeEntry
    Assert-That (Test-Path -LiteralPath $entry -PathType Leaf) 'Pinned native/Node entry is missing'
    $program = if ($Kind -eq 'opencode') { $entry } else { (Get-Command node.exe -ErrorAction Stop).Source }
    $arguments = switch ($Kind) {
        'pi' { @($entry, '--mode', 'rpc', '--offline', '--no-session', '--no-extensions', '--no-skills', '--no-prompt-templates', '--no-context-files', '--no-themes', '--no-approve', '--provider', $provider, '--model', $Model, '--tools', 'read,write,edit') }
        'dsh' { @($entry, '--profile', 'headless', '--patch', '<per-attempt managed.patch.yml>', '--json') }
        'opencode' { @('run', '--pure', '--format', 'json', '--dir', $Project, '--model', ($provider + '/' + $Model), '--', '<task prompt>') }
    }
    $script:harnessMetadata[$Kind] = [ordered]@{ package = $package.name; version = $package.version; configured_shim = $executable; actual_entry = $entry; entry_sha256 = (Get-FileHash -LiteralPath $entry -Algorithm SHA256).Hash.ToLowerInvariant(); program = $program; argument_template = @($arguments); provider = $provider; model = $Model; input = $(if ($Kind -eq 'pi') { 'JSON RPC prompt on stdin; EOF after agent_settled' } elseif ($Kind -eq 'dsh') { 'prompt on stdin' } else { 'literal argv after --' }) }
    $rows = @('[agent]', "name = '$Kind-p0'", "harness = '$Kind'", 'mode = "managed"', "project_id = '$Kind-project'", '[project]', "root = $projectText", 'allowed_tools = ["read", "write", "edit"]', 'artifact_paths = ["out/health.json"]', 'require_remote_task_approval = true', '[adapter]', "executable = $executableText", "provider = '$provider'", ('model = ' + ($Model | ConvertTo-Json -Compress)), 'env_allowlist = ["DEEPSEEK_API_KEY"]')
    $file = Join-Path $DataRoot ($Kind + '.toml')
    [IO.File]::WriteAllLines($file, $rows)
    $registered = Invoke-Hmesh $Node @('agent', 'register', '--config', $file)
    Assert-That ($registered.probe.version_supported -and $registered.probe.version -eq $package.version) 'Installed version differs from the approved adapter entry'
    return $registered
}
function Task-Prompt([string]$Kind, [string]$Marker) {
    return "Create out/health.json containing a JSON object with status equal to ok, harness equal to $Kind, and request_id equal to $Marker. Use the write tool, then read the file back. Work only in the registered project. Do not invoke shell commands, networking, subagents, or modify any other files."
}
function Verify-Artifact([string]$Node, [string]$TaskId, [string]$Project, [string]$Kind, [string]$Marker) {
    $expected = @{ status = 'ok'; harness = $Kind; request_id = $Marker } | ConvertTo-Json -Compress
    $verification = Invoke-Hmesh $Node @('task', 'verify', $TaskId, '--artifact', 'out/health.json', '--expect-json', $expected)
    Assert-That $verification.passed 'Deterministic daemon assertions failed'
    $file = Join-Path $Project 'out\health.json'
    $actual = Get-Content -LiteralPath $file -Raw | ConvertFrom-Json
    Assert-That ($actual.status -eq 'ok' -and $actual.harness -eq $Kind -and $actual.request_id -eq $Marker) 'Independent runner assertions failed'
    $hash = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant()
    Assert-That ($hash -eq $verification.artifact.sha256) 'Daemon artifact hash differs from the independent runner'
    return $verification
}
function Send-Remote([string]$Origin, [string]$Target, $TargetCard, $Agent, [string]$Project, [string]$Kind) {
    $requestId = [guid]::NewGuid().ToString()
    $prompt = Task-Prompt $Kind $requestId
    $arguments = @('task', 'send', $Agent.agent_id, '--peer', $TargetCard.node_id, '--project', ($Kind + '-project'), '--prompt', $prompt, '--request-id', $requestId)
    $accepted = Invoke-Hmesh $Origin $arguments
    Assert-That ($accepted.status -eq 'PENDING_APPROVAL') 'Remote task bypassed local approval'
    Assert-That ($null -eq $accepted.last_attempt_id) 'Unapproved task acquired an execution attempt'
    [void](Invoke-Hmesh $Target @('task', 'approve', $accepted.task_id))
    $completed = Wait-Task $Origin $accepted.task_id
    Assert-That ($completed.status -eq 'SUCCEEDED') 'Remote managed run did not succeed'
    $verification = Verify-Artifact $Target $completed.task_id $Project $Kind $requestId
    $shared = @(Invoke-Hmesh $Origin @('task', 'verification', $completed.task_id))
    Assert-That ($shared.Count -gt 0 -and $shared[-1].passed) 'Origin could not read authorized verification evidence'
    $events = @(Invoke-Hmesh $Origin @('task', 'events', $completed.task_id))
    $seq = 0
    foreach ($event in $events) { Assert-That ($event.seq -eq ($seq + 1)) 'Event stream contains a gap'; $seq = $event.seq }
    $cursor = [int][Math]::Floor($events.Count / 2)
    $replay = @(Invoke-Hmesh $Origin @('task', 'events', $completed.task_id, '--after', [string]$cursor))
    Assert-That ($replay.Count -eq ($events.Count - $cursor)) 'Event reconnect replay missed committed records'
    $beforeTime = (Get-Item -LiteralPath (Join-Path $Project 'out\health.json')).LastWriteTimeUtc
    $duplicate = Invoke-Hmesh $Origin $arguments
    Assert-That ($duplicate.task_id -eq $completed.task_id -and $duplicate.last_attempt_id -eq $completed.last_attempt_id) 'Duplicate request created a new task or attempt'
    $afterEvents = @(Invoke-Hmesh $Origin @('task', 'events', $completed.task_id))
    Assert-That ($afterEvents.Count -eq $events.Count) 'Duplicate delivery started additional activity'
    Assert-That ($beforeTime -eq (Get-Item -LiteralPath (Join-Path $Project 'out\health.json')).LastWriteTimeUtc) 'Duplicate delivery rewrote the artifact'
    $trace = @($events | ForEach-Object { [ordered]@{ seq = $_.seq; event_type = $_.event_type; kind = $_.payload.type; status = $_.payload.status; tool = $_.payload.name; phase = $_.payload.phase; sha256 = $_.payload.sha256 } })
    return [ordered]@{ harness = $Kind; origin_node_id = $completed.origin_node_id; execution_node_id = $completed.target_node_id; request_id = $requestId; task_id = $completed.task_id; attempt_id = $completed.last_attempt_id; status = $completed.status; verification = $verification; event_count = $events.Count; event_trace = $trace; replay_passed = $true; dedup_passed = $true }
}
try {
    foreach ($check in @('fmt', 'clippy', 'test', 'build')) {
        switch ($check) {
            'fmt' { & cargo "+$Toolchain" fmt --all --check }
            'clippy' { & cargo "+$Toolchain" clippy --locked --workspace --all-targets -- -D warnings }
            'test' { & cargo "+$Toolchain" test --locked --workspace }
            'build' { & cargo "+$Toolchain" build --locked -p hmesh-cli }
        }
        Assert-That ($LASTEXITCODE -eq 0) "$check failed"
    }
    if (-not $RunRealHarnesses) { Write-Output 'Local checks passed. No model was called. Use -RunRealHarnesses for explicit real-model smoke.'; return }
    Assert-That (-not [string]::IsNullOrWhiteSpace($env:DEEPSEEK_API_KEY)) 'Real smoke requires an explicitly authorized DEEPSEEK_API_KEY environment credential'
    if (-not $DataRoot) { $DataRoot = Join-Path $workspace ('.hmesh-v0\acceptance-' + [guid]::NewGuid().ToString('N')) }
    $DataRoot = [IO.Path]::GetFullPath($DataRoot)
    Assert-That (-not (Test-Path -LiteralPath $DataRoot)) 'Choose a fresh acceptance directory; existing identities/results will not be overwritten'
    [IO.Directory]::CreateDirectory($DataRoot) | Out-Null
    if (-not $EvidencePath) { $EvidencePath = Join-Path $workspace 'docs\evidence\latest-p0-smoke.json' }
    $a = Join-Path $DataRoot 'node-a'; $b = Join-Path $DataRoot 'node-b'
    $aCard = Start-Node $a 'acceptance-a'; $bCard = Start-Node $b 'acceptance-b'
    $pair = Invoke-Hmesh $a @('pair', 'request', '--peer', $bCard.endpoint, '--expect-fingerprint', $bCard.fingerprint)
    [void](Invoke-Hmesh $b @('pair', 'approve', $pair.request_id, '--expect-fingerprint', $aCard.fingerprint))
    [void](Invoke-Hmesh $a @('pair', 'approve', $pair.request_id, '--expect-fingerprint', $bCard.fingerprint))
    $projects = @{}; $agents = @{}; $localRuns = @()
    foreach ($kind in @('pi', 'dsh', 'opencode')) {
        $node = if ($kind -eq 'pi') { $a } else { $b }
        $projects[$kind] = Join-Path $DataRoot ('project-' + $kind)
        $agents[$kind] = Write-Agent $kind $node $projects[$kind]
        $marker = 'local-verification-' + $kind
        $created = Invoke-Hmesh $node @('agent', 'verify', $agents[$kind].agent_id, '--allow-model', '--prompt', (Task-Prompt $kind $marker))
        $task = Wait-Task $node $created.task_id
        Assert-That ($task.status -eq 'SUCCEEDED') "$kind local verification failed"
        $verification = Verify-Artifact $node $task.task_id $projects[$kind] $kind $marker
        $localRuns += [ordered]@{ harness = $kind; task_id = $task.task_id; attempt_id = $task.last_attempt_id; verification = $verification }
    }
    # Trusted connectivity alone does not authorize task submission.
    $denied = Invoke-Hmesh $a @('task', 'send', $agents['dsh'].agent_id, '--peer', $bCard.node_id, '--project', 'dsh-project', '--prompt', 'fixture-denied-no-run') -ExpectFailure
    Assert-That ($denied.error.code -eq 'PERMISSION_DENIED') 'Default-deny task policy did not apply'
    foreach ($entry in @(@($a, $bCard.node_id, @('pi')), @($b, $aCard.node_id, @('dsh', 'opencode')))) {
        $grants = @()
        foreach ($kind in $entry[2]) { $grants += @{ peer = $entry[1]; project = ($kind + '-project'); actions = @('agent.read', 'task.submit'); require_approval = $true } }
        $lines = @('[default]', 'action = "deny"')
        foreach ($grant in $grants) { $lines += @('[[grant]]', ('peer = "' + $grant.peer + '"'), ('project = "' + $grant.project + '"'), 'actions = ["agent.read", "task.submit"]', 'require_approval = true') }
        $file = Join-Path $DataRoot ([IO.Path]::GetFileName($entry[0]) + '-grants.toml')
        [IO.File]::WriteAllLines($file, $lines)
        [void](Invoke-Hmesh $entry[0] @('policy', '--file', $file))
    }
    $rejected = Invoke-Hmesh $a @('task', 'send', $agents['dsh'].agent_id, '--peer', $bCard.node_id, '--project', 'dsh-project', '--prompt', 'fixture-rejected-no-run')
    [void](Invoke-Hmesh $b @('task', 'reject', $rejected.task_id))
    $rejected = Invoke-Hmesh $a @('task', 'show', $rejected.task_id)
    Assert-That ($rejected.status -eq 'REJECTED' -and $null -eq $rejected.last_attempt_id) 'Rejected task started a run'
    $cancelled = Invoke-Hmesh $a @('task', 'send', $agents['dsh'].agent_id, '--peer', $bCard.node_id, '--project', 'dsh-project', '--prompt', 'fixture-cancelled-before-approval')
    $cancelled = Invoke-Hmesh $a @('task', 'cancel', $cancelled.task_id)
    Assert-That ($cancelled.status -eq 'CANCELLED' -and $null -eq $cancelled.last_attempt_id) 'Pre-start cancellation created an execution'
    $remoteRuns = @(
        (Send-Remote $a $b $bCard $agents['dsh'] $projects['dsh'] 'dsh'),
        (Send-Remote $b $a $aCard $agents['pi'] $projects['pi'] 'pi'),
        (Send-Remote $a $b $bCard $agents['opencode'] $projects['opencode'] 'opencode')
    )
    $afterRestart = @()
    [void](Invoke-Hmesh $a @('daemon', 'stop')); [void](Invoke-Hmesh $b @('daemon', 'stop'))
    foreach ($process in $processes) { Assert-That $process.WaitForExit(10000) 'Daemon did not stop' }
    $processes = @(); $nodes = @()
    # Restart uses the same configs, identities, fixed ports and databases.
    foreach ($node in @($a,$b)) {
        $process = Start-Process -FilePath $binary -ArgumentList @('--data-dir', ('"' + $node + '"'), 'daemon', 'start') -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $node 'restart.stdout.log') -RedirectStandardError (Join-Path $node 'restart.stderr.log')
        $processes += $process; $nodes += $node
        for ($i=0; $i -lt 100 -and -not (Test-Path -LiteralPath (Join-Path $node 'daemon.json')); $i++) { Start-Sleep -Milliseconds 50 }
        [void](Invoke-Hmesh $node @('daemon','status'))
    }
    foreach ($run in $remoteRuns) {
        $origin = if ($run.harness -eq 'pi') { $b } else { $a }
        $record = Invoke-Hmesh $origin @('task','show',$run.task_id)
        Assert-That ($record.status -eq 'SUCCEEDED' -and $record.last_attempt_id -eq $run.attempt_id) 'Restart lost or reexecuted a confirmed task'
        $afterRestart += $record.task_id
    }
    $report = [ordered]@{ schema_version = 2; date = (Get-Date -Format 'yyyy-MM-dd'); scope = 'V0 Windows two-node real Harness smoke'; status = 'passed'; os = [Environment]::OSVersion.VersionString; arch = $env:PROCESSOR_ARCHITECTURE; rust = (& rustc "+$Toolchain" --version); binary_sha256 = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant(); cargo_lock_sha256 = (Get-FileHash -LiteralPath (Join-Path $workspace 'Cargo.lock') -Algorithm SHA256).Hash.ToLowerInvariant(); harnesses = $harnessMetadata; nodes = @(@{ node_id = $aCard.node_id; fingerprint = $aCard.fingerprint; endpoint = $aCard.endpoint }, @{ node_id = $bCard.node_id; fingerprint = $bCard.fingerprint; endpoint = $bCard.endpoint }); model = $Model; local_runs = $localRuns; remote_runs = $remoteRuns; negative_checks = @{ default_deny = $true; approval_rejection_no_attempt = $true; prestart_cancel_no_attempt = $true }; restart_query_task_ids = $afterRestart; ordinary_checks = 'fmt, clippy -D warnings, workspace tests'; running_cancel = 'controlled Rust process-tree tests; native Harness abort protocols not claimed'; cross_device_or_cross_os = 'not_tested' }
    $parent = Split-Path -Parent ([IO.Path]::GetFullPath($EvidencePath)); [IO.Directory]::CreateDirectory($parent) | Out-Null
    [IO.File]::WriteAllText($EvidencePath, ($report | ConvertTo-Json -Depth 12))
    Write-Output "P0 smoke passed. Evidence: $EvidencePath"
} finally {
    foreach ($node in $nodes) { try { [void](Invoke-Hmesh $node @('daemon','stop')) } catch { Write-Warning 'Graceful test Daemon shutdown failed; stopping only this script-owned process handle.' } }
    foreach ($process in $processes) { if (-not $process.HasExited -and -not $process.WaitForExit(5000)) { $process.Kill($true); $process.WaitForExit() } }
    Pop-Location
}
