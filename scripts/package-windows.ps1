param(
  [Parameter(Mandatory=$true)][ValidateSet('create','extract')][string]$Mode,
  [Parameter(Mandatory=$true)][string]$Source,
  [Parameter(Mandatory=$true)][string]$Destination
)
$ErrorActionPreference='Stop'
if($Mode -eq 'create'){Compress-Archive -LiteralPath $Source -DestinationPath $Destination -CompressionLevel Optimal}
else{Expand-Archive -LiteralPath $Source -DestinationPath $Destination}
