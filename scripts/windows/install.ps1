# Install the portable x64 release for the current Windows user. No admin needed.
[CmdletBinding()]
param([string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\aiterm'))
$ErrorActionPreference = 'Stop'
$architecture = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
if ($env:OS -ne 'Windows_NT' -or $architecture -ne 'AMD64') {
    throw 'aiterm requires x64 Windows.'
}
if ([Environment]::OSVersion.Version.Build -lt 17763) {
    throw 'aiterm requires Windows 10 version 1809 or newer.'
}
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
$temporary = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $temporary | Out-Null
try {
    $release = Invoke-RestMethod -Uri 'https://api.github.com/repos/bobans29/Ai-terminal/releases/latest'
    $zipName = 'aiterm-windows-x86_64.zip'
    $zipAsset = @($release.assets | Where-Object { $_.name -eq $zipName })
    $hashAsset = @($release.assets | Where-Object { $_.name -eq "$zipName.sha256" })
    if ($zipAsset.Count -ne 1 -or $hashAsset.Count -ne 1) {
        throw 'No Windows release is available yet. Check the repository Releases page.'
    }
    $archive = Join-Path $temporary $zipName
    $checksum = Join-Path $temporary 'checksum.txt'
    Invoke-WebRequest -UseBasicParsing -Uri $zipAsset[0].browser_download_url -OutFile $archive
    Invoke-WebRequest -UseBasicParsing -Uri $hashAsset[0].browser_download_url -OutFile $checksum
    $expected = (Get-Content -Raw $checksum).Trim().Split(' ')[0]
    if ($expected -notmatch '^[a-fA-F0-9]{64}$' -or (Get-FileHash $archive -Algorithm SHA256).Hash -ne $expected) {
        throw 'Release checksum verification failed; nothing was installed.'
    }
    $unpacked = Join-Path $temporary 'unpacked'
    Expand-Archive -LiteralPath $archive -DestinationPath $unpacked
    $executable = Join-Path $unpacked 'aiterm.exe'
    if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) { throw 'Release is missing aiterm.exe.' }
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Copy-Item -LiteralPath $executable -Destination (Join-Path $InstallDir 'aiterm.exe') -Force
    $userPath = [string][Environment]::GetEnvironmentVariable('Path', 'User')
    if (@($userPath -split ';' | Where-Object { $_.TrimEnd('\') -ieq $InstallDir.TrimEnd('\') }).Count -eq 0) {
        [Environment]::SetEnvironmentVariable('Path', (($userPath.TrimEnd(';') + ';' + $InstallDir).TrimStart(';')), 'User')
    }
    if (@($env:Path -split ';' | Where-Object { $_.TrimEnd('\') -ieq $InstallDir.TrimEnd('\') }).Count -eq 0) {
        $env:Path += ";$InstallDir"
    }
    Write-Host "Installed aiterm to $InstallDir. Run: aiterm"
} finally {
    Remove-Item -LiteralPath $temporary -Recurse -Force -ErrorAction SilentlyContinue
}
