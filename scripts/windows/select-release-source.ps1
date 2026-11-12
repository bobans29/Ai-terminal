# Resolve existing tags before building; new tags use the selected workflow commit.
[CmdletBinding()]
param([Parameter(Mandatory)][ValidatePattern('^v[0-9]+\.[0-9]+\.[0-9]+$')][string]$Tag)
$ErrorActionPreference = 'Stop'
$existing = git ls-remote --tags origin "refs/tags/$Tag"
if ($LASTEXITCODE -ne 0) { throw 'Could not check the remote release tag.' }
if ($existing) {
    git fetch --depth=1 origin "refs/tags/$Tag" | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'Could not fetch the release tag.' }
    git checkout --detach FETCH_HEAD | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'Could not check out the release tag.' }
}
$commit = git rev-parse --verify 'HEAD^{commit}'
if ($LASTEXITCODE -ne 0) { throw 'Could not resolve the release commit.' }
$commit
