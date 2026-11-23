param(
  [Parameter(Mandatory=$true)][ValidateSet('create','extract')][string]$Mode,
  [Parameter(Mandatory=$true)][string]$Source,
  [Parameter(Mandatory=$true)][string]$Destination
)
$ErrorActionPreference='Stop'
if($Mode -eq 'create'){Compress-Archive -LiteralPath $Source -DestinationPath $Destination -CompressionLevel Optimal}
else{
  # Windows PowerShell uses .NET Framework. Some installed Archive modules call
  # the newer three-argument ExtractToDirectory overload, which is unavailable
  # in that host. The two-argument overload is supported and never overwrites.
  Add-Type -AssemblyName System.IO.Compression.FileSystem
  [System.IO.Compression.ZipFile]::ExtractToDirectory($Source,$Destination)
}
