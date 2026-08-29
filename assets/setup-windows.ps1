param([Parameter(Mandatory=$true)][string]$Version)
$ErrorActionPreference = 'Stop'
$runtime = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'win-arm64' } else { 'win-x64' }
$destination = Join-Path $env:LOCALAPPDATA 'tmux-agent-workbench'
$stage = Join-Path $env:TEMP ("tmux-agent-workbench-" + [guid]::NewGuid())
New-Item -ItemType Directory -Force -Path $destination, $stage | Out-Null
$archive = Join-Path $stage 'wb-client.zip'
$url = "https://github.com/lukewang1024/tmux-agent-workbench/releases/download/v$Version/wb-client-$runtime.zip"
Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $archive
Invoke-WebRequest -UseBasicParsing -Uri "$url.sha256" -OutFile "$archive.sha256"
$expected = ((Get-Content "$archive.sha256") -split '\s+')[0].ToLowerInvariant()
$actual = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLowerInvariant()
if ($expected -ne $actual) { throw 'Windows companion checksum mismatch' }
Expand-Archive -Force -Path $archive -DestinationPath $stage
Copy-Item -Recurse -Force (Join-Path $stage '*') $destination
& (Join-Path $destination 'wb-client.exe') setup
Remove-Item -Recurse -Force $stage
