param(
    [Parameter(Mandatory=$true)][string]$Repo,
    [Parameter(Mandatory=$true)][string]$ProjectRoot,
    [string]$ProjectId = 'tailnet-reverse',
    [string]$NodeDir,
    [string]$Plan = (Join-Path $PSScriptRoot 'plan.json'),
    [int]$Port = 18105
)
$ErrorActionPreference = 'Stop'
$Repo = (Resolve-Path -LiteralPath $Repo).Path
$ProjectRoot = (Resolve-Path -LiteralPath $ProjectRoot).Path
if (-not $NodeDir) { $NodeDir = Join-Path $Repo '.hmesh-v0/tailnet-win32/node' }
$NodeDir = (Resolve-Path -LiteralPath $NodeDir).Path
$Plan = (Resolve-Path -LiteralPath $Plan).Path
$p = Get-Content -Raw -LiteralPath $Plan | ConvertFrom-Json
$binary = Join-Path $Repo 'target/debug/hmesh.exe'
if (-not (Test-Path -LiteralPath $binary)) { throw 'Build hmesh-cli in the Windows repository first.' }
$checker = Join-Path $ProjectRoot 'app-todo/contract-check.mjs'
if ((Get-FileHash -Algorithm SHA256 -LiteralPath $checker).Hash.ToLowerInvariant() -ne $p.checker_sha256) { throw 'Checker hash mismatch. Do not modify the plan to bypass this; review the artifact first.' }
$state = Join-Path $Repo ('.hmesh-v0/acceptance-worker/' + $p.run_id)
[void](New-Item -ItemType Directory -Force -Path $state)
$config = Join-Path $state 'owner.json'
@{binary=$binary;node_dir=$NodeDir;project_root=$ProjectRoot;project_id=$ProjectId;checker_file=$checker;plan_file=$Plan;state_dir=$state;listen_port=$Port} |
    ConvertTo-Json -Depth 8 | Set-Content -Encoding utf8 -LiteralPath $config
$node = (Get-Command node.exe -ErrorAction Stop).Source
& $node (Join-Path $PSScriptRoot 'worker.mjs') --config $config
if ($LASTEXITCODE -ne 0) { throw ('Acceptance worker failed with exit ' + $LASTEXITCODE) }
