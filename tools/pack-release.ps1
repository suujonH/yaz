#Requires -Version 7
<#
.SYNOPSIS
    Package the publish directory into the Release files used by auto-update.

.DESCRIPTION
    Generate the file manifest yaz.files.sha256 in the publish directory (an update only replaces files listed in the manifest) and check that -Version matches the workspace version,
    then generate YaZ-v<version>-win-x64.zip and YaZ-update.json (version, package name, size, SHA-256) in -OutDir.
    The publish directory must already contain yaz.exe and yaz-updater.exe.

.EXAMPLE
    ./tools/pack-release.ps1 -Version 0.2.0 -PublishDir publish/YaZ -OutDir .
#>
param(
    [Parameter(Mandatory)]
    [string]$Version,

    [Parameter(Mandatory)]
    [string]$PublishDir,

    [Parameter(Mandatory)]
    [string]$OutDir
)

$ErrorActionPreference = 'Stop'

# Semantic version format identical to the version parsing in zulip-update: after a tag with a bad format is published, installed programs cannot recognize it
$numeric = '(0|[1-9]\d*)'
$identifier = '(0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)'
if ($Version -notmatch "^$numeric\.$numeric\.$numeric(-$identifier(\.$identifier)*)?$") {
    throw "版本号 '$Version' 不是合法的语义版本（例如 0.2.0 或 0.2.0-beta.1）"
}

$publish = (Resolve-Path -LiteralPath $PublishDir).Path
foreach ($required in 'yaz.exe', 'yaz-updater.exe') {
    if (-not (Test-Path -LiteralPath (Join-Path $publish $required))) {
        throw "发布目录中缺少 $required"
    }
}

# The program is compiled with the workspace version; if the two differ, the program after an update still considers itself to be the old version
$root = Split-Path -Parent $PSScriptRoot
$cargo = Get-Content -LiteralPath (Join-Path $root 'Cargo.toml') -Raw -Encoding utf8
if ($cargo -notmatch '(?ms)\[workspace\.package\][^\[]*?^version\s*=\s*"([^"]+)"') {
    throw 'Cargo.toml 的 [workspace.package] 中没有 version'
}
if ($Matches[1] -ne $Version) {
    throw "工作区版本 '$($Matches[1])' 与 '$Version' 不一致"
}
$productVersion = (Get-Item -LiteralPath (Join-Path $publish 'yaz.exe')).VersionInfo.ProductVersion
if ($productVersion -and ($productVersion -split '\+')[0] -ne $Version) {
    throw "yaz.exe 的版本 '$productVersion' 与 '$Version' 不一致"
}

$listName = 'yaz.files.sha256'
$listPath = Join-Path $publish $listName
Remove-Item -LiteralPath $listPath -ErrorAction SilentlyContinue
$lines = Get-ChildItem -LiteralPath $publish -Recurse -File | Sort-Object FullName | ForEach-Object {
    $relative = [IO.Path]::GetRelativePath($publish, $_.FullName)
    $hash = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  $relative"
}
[IO.File]::WriteAllLines($listPath, [string[]]$lines, [Text.UTF8Encoding]::new($false))

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$out = (Resolve-Path -LiteralPath $OutDir).Path
$package = "YaZ-v$Version-win-x64.zip"
$zip = Join-Path $out $package
Remove-Item -LiteralPath $zip -ErrorAction SilentlyContinue
Compress-Archive -Path (Join-Path $publish '*') -DestinationPath $zip

$manifest = [ordered]@{
    version = $Version
    package = $package
    sha256  = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    size    = (Get-Item -LiteralPath $zip).Length
}
$manifestPath = Join-Path $out 'YaZ-update.json'
[IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json), [Text.UTF8Encoding]::new($false))

Write-Host "已生成 $zip（$($manifest.size) 字节）和 $manifestPath"
