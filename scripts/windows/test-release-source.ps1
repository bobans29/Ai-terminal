# Verify tag selection without building or publishing a release.
$ErrorActionPreference = 'Stop'
$selector = Join-Path $PSScriptRoot 'select-release-source.ps1'
$temporary = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $temporary | Out-Null
function Git-Checked {
    & git @args
    if ($LASTEXITCODE -ne 0) { throw "Git fixture command failed: $args" }
}
Push-Location $temporary
try {
    Git-Checked init --bare remote.git | Out-Null
    Git-Checked clone ./remote.git work | Out-Null
    Set-Location work
    Git-Checked config user.email 'test@example.invalid'
    Git-Checked config user.name 'Release test'
    Set-Content version.txt 'tagged commit'
    Git-Checked add version.txt
    Git-Checked commit -m 'Tagged version' | Out-Null
    $tagged = Git-Checked rev-parse HEAD
    Git-Checked tag -a v1.0.0 -m 'Annotated release tag'
    Set-Content version.txt 'newer workflow commit'
    Git-Checked commit -am 'Newer version' | Out-Null
    $selected = Git-Checked rev-parse HEAD
    Git-Checked push origin HEAD --tags | Out-Null
    $resolved = & $selector -Tag v1.0.0
    if ($resolved -ne $tagged -or (Get-Content version.txt) -ne 'tagged commit') {
        throw 'Existing tag did not select its own source commit.'
    }
    Git-Checked checkout --detach $selected | Out-Null
    $resolved = & $selector -Tag v2.0.0
    if ($resolved -ne $selected -or (Get-Content version.txt) -ne 'newer workflow commit') {
        throw 'New tag did not retain the selected workflow commit.'
    }
    Write-Host 'Existing annotated tag and new-tag source selection passed.'
} finally {
    Pop-Location
    Remove-Item -LiteralPath $temporary -Recurse -Force -ErrorAction SilentlyContinue
}
