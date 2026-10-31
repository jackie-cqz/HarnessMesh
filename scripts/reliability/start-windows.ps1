param([Parameter(Mandatory=$true)][string]$Repo,[string]$ProjectRoot,[string]$NodeDir)
$ErrorActionPreference='Stop'
$Repo=(Resolve-Path -LiteralPath $Repo).Path
$manifest=Join-Path $PSScriptRoot 'manifest.json'
$m=Get-Content -Raw -LiteralPath $manifest|ConvertFrom-Json
if(-not $ProjectRoot){
  $configs=@(Get-ChildItem -LiteralPath (Join-Path $Repo '.hmesh-v0/acceptance-worker') -Filter owner.json -Recurse | ForEach-Object {Get-Content -Raw -LiteralPath $_.FullName|ConvertFrom-Json}|Where-Object {$_.project_id -eq $m.project_id})
  $roots=@($configs.project_root|Select-Object -Unique)
  if($roots.Count -ne 1){throw 'Pass -ProjectRoot with the registered tailnet-reverse root.'}
  $ProjectRoot=$roots[0]
}
if(-not $NodeDir){$NodeDir=Join-Path $Repo '.hmesh-v0/tailnet-win32/node'}
$state=Join-Path $Repo ('.hmesh-v0/reliability/'+$m.run_id)
[void](New-Item -ItemType Directory -Force -Path $state)
$config=Join-Path $state 'owner.json'
@{binary=(Join-Path $Repo 'target/debug/hmesh.exe');node_dir=(Resolve-Path -LiteralPath $NodeDir).Path;project_root=(Resolve-Path -LiteralPath $ProjectRoot).Path;project_id=$m.project_id;manifest=$manifest;state_dir=$state;port=18115}|ConvertTo-Json|Set-Content -LiteralPath $config -Encoding utf8
$node=(Get-Command node.exe).Source
& $node (Join-Path $PSScriptRoot 'scripts/reliability/supervisor.mjs') --config $config
