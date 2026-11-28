<#
.SYNOPSIS
  Runs redist-cli.exe for real in a throwaway Windows container: check,
  install, check.

.DESCRIPTION
  The container uses Hyper-V isolation, so its Windows build may differ from
  the host's. Nothing is installed on the host: the exe and the manifest are
  mounted read-only, and the output and the work folders' logs are copied
  out to -Out.

  It needs an elevated PowerShell (containerd only admits administrators),
  and containerd with nerdctl set up for Windows containers.

  The container's network is a NAT network of nerdctl's own, -Network, whose
  config lives in its own folder, -CniConf. The script creates it if it is
  missing. nerdctl 2.4.2 crashes on the single-plugin .conf that containerd's
  install script leaves in its default folder, so that folder is not used.
  To remove the network: nerdctl --cni-netconfpath <CniConf> network rm <Network>

  The default image, Windows Server, has the full Windows API, DirectX
  included. Server Core is about half the size and has the 32-bit layer
  (WoW64), PowerShell and MSI, but not all of DirectX. Nano Server has none of
  them.

.EXAMPLE
  cargo build --release --locked -p redist --bin redist-cli
  pwsh -File e2e\container-test.ps1

  Then read target\container-test\out\. The lighter image:

  pwsh -File e2e\container-test.ps1 -Image mcr.microsoft.com/windows/servercore:ltsc2025
#>
param(
    [string]$Image = 'mcr.microsoft.com/windows/server:ltsc2025',
    [string]$Exe = "$PSScriptRoot\..\target\release\redist-cli.exe",
    [string]$Manifest = "$PSScriptRoot\manifest.json",
    [string]$Out = "$PSScriptRoot\..\target\container-test",
    [string]$Network = 'redist-test',
    [string]$CniConf = "$env:ProgramData\redist-unified-test\cni\conf"
)

$ErrorActionPreference = 'Stop'

$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) {
    throw 'Run this in an elevated PowerShell: containerd only admits administrators.'
}

$exe = (Resolve-Path $Exe).Path
$manifest = (Resolve-Path $Manifest).Path
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path

# What the container sees: the exe, the manifest and the steps, read-only;
# and a folder for what comes out.
$stage = Join-Path $Out 'in'
$results = Join-Path $Out 'out'
foreach ($dir in $stage, $results) {
    if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
    New-Item -ItemType Directory $dir | Out-Null
}
Copy-Item $exe, $manifest $stage

$steps = @'
$exe = 'C:\redist\redist-cli.exe'
$manifest = 'C:\redist\manifest.json'
"Windows in the container: " + (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion').CurrentBuild
& $exe --manifest $manifest --check *>&1 | Tee-Object -FilePath C:\out\before.txt
& $exe --manifest $manifest *>&1 | Tee-Object -FilePath C:\out\install.txt
$code = $LASTEXITCODE
"exit code: $code" | Tee-Object -FilePath C:\out\install.txt -Append
& $exe --manifest $manifest --check *>&1 | Tee-Object -FilePath C:\out\after.txt
Get-ChildItem C:\Windows\Temp -Directory -Filter 'redist-unified-*' |
    ForEach-Object { Copy-Item $_.FullName "C:\out\$($_.Name)" -Recurse }
if ($code -notin 0, 3010) { exit $code }
if ($code -eq 0 -and -not (Select-String -Path C:\out\after.txt -SimpleMatch 'Nothing is missing.' -Quiet)) {
    'Something is still missing after installing.'
    exit 1
}
exit 0
'@
Set-Content -Path (Join-Path $stage 'steps.ps1') -Value $steps -Encoding utf8

# The CNI plugin writes wincni.log, with the host's network adapters, into the
# current folder; keep it with the results, out of the repository.
Push-Location $Out

Write-Host "Pulling $Image (several GB the first time)..."
nerdctl pull $Image
if ($LASTEXITCODE -ne 0) { throw "nerdctl pull failed ($LASTEXITCODE)." }

New-Item -ItemType Directory -Force $CniConf | Out-Null
nerdctl --cni-netconfpath $CniConf network inspect $Network *> $null
if ($LASTEXITCODE -ne 0) {
    Write-Host "Creating the container network $Network..."
    nerdctl --cni-netconfpath $CniConf network create $Network
    if ($LASTEXITCODE -ne 0) { throw "nerdctl network create failed ($LASTEXITCODE)." }
}

Write-Host 'Running check, install, check in a throwaway container...'
nerdctl --cni-netconfpath $CniConf run --rm --isolation hyperv --network $Network `
    -v "${stage}:C:\redist:ro" `
    -v "${results}:C:\out" `
    $Image `
    powershell -NoProfile -ExecutionPolicy Bypass -File C:\redist\steps.ps1
$code = $LASTEXITCODE
Pop-Location

Write-Host "Container exit code: $code. Output and logs: $results"
exit $code
