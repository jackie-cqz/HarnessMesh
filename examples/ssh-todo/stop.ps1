$ErrorActionPreference='Stop'
$controller=Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) 'scripts/todo-session.ps1'
& $controller -Action stop
