# Offline installer checks using the executable built by CI; no releases published.
$ErrorActionPreference = 'Stop'
$fixture = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString())
$originalPath = $env:Path
$originalUserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
New-Item -ItemType Directory -Path $fixture | Out-Null
$installDir = Join-Path $fixture 'install with spaces'
$zip = Join-Path $fixture 'fixture.zip'
$hash = Join-Path $fixture 'hash.txt'
try {
    Compress-Archive -LiteralPath (Join-Path $PSScriptRoot '..\..\target\release\aiterm.exe') -DestinationPath $zip
    (Get-FileHash $zip -Algorithm SHA256).Hash | Set-Content -Encoding ascii $hash
    function Invoke-RestMethod {
        param($Uri)
        if ($Uri -ne 'https://api.github.com/repos/bobans29/Ai-terminal/releases/latest') { throw 'Unexpected metadata URL' }
        return @{ assets = @(
            @{ name = 'aiterm-windows-x86_64.zip'; browser_download_url = 'https://fixture.invalid/archive' },
            @{ name = 'aiterm-windows-x86_64.zip.sha256'; browser_download_url = 'https://fixture.invalid/hash' }
        ) }
    }
    function Invoke-WebRequest {
        param([switch]$UseBasicParsing, $Uri, $OutFile)
        switch ($Uri) {
            'https://fixture.invalid/archive' { Copy-Item -LiteralPath $zip -Destination $OutFile }
            'https://fixture.invalid/hash' { Copy-Item -LiteralPath $hash -Destination $OutFile }
            default { throw 'Unexpected asset URL' }
        }
    }
    $installer = Join-Path $PSScriptRoot 'install.ps1'
    [Environment]::SetEnvironmentVariable('Path', $null, 'User')
    & $installer -InstallDir $installDir
    & $installer -InstallDir $installDir
    if (-not (Test-Path -LiteralPath (Join-Path $installDir 'aiterm.exe'))) { throw 'Executable was not installed' }
    & (Join-Path $installDir 'aiterm.exe') --help
    if ($LASTEXITCODE -ne 0) { throw 'Installed executable failed' }
    $savedPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (@($savedPath -split ';' | Where-Object { $_ -eq $installDir }).Count -ne 1) { throw 'PATH entry was duplicated' }
    if (@($env:Path -split ';' | Where-Object { $_ -eq $installDir }).Count -ne 1) { throw 'Current PATH was not updated' }
    # Corrupt hash must fail before installing anything.
    ('0' * 64) | Set-Content -Encoding ascii $hash
    $rejected = $false
    try { & $installer -InstallDir (Join-Path $fixture 'must not exist') } catch {
        if ($_.Exception.Message -notlike '*checksum verification failed*') { throw }
        $rejected = $true
    }
    if (-not $rejected -or (Test-Path -LiteralPath (Join-Path $fixture 'must not exist'))) { throw 'Corrupt archive was accepted' }
    Write-Host 'Installer, repeat install, PATH, executable, and checksum rejection passed.'
} finally {
    $env:Path = $originalPath
    [Environment]::SetEnvironmentVariable('Path', $originalUserPath, 'User')
    Remove-Item -LiteralPath $fixture -Recurse -Force -ErrorAction SilentlyContinue
}
