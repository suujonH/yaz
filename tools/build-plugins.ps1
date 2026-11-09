<#
.SYNOPSIS
    Build the built-in plugins (release) and copy them to the program's plugins directory.

.DESCRIPTION
    plugins/model-center, translator and reply-assistant. Each plugin goes to <OutDir>\<id from plugin.json>\,
    containing only plugin.json and the DLL. plugins/echo is a sample and is not shipped with the program.

.EXAMPLE
    ./tools/build-plugins.ps1 -OutDir publish/YaZ/plugins
    ./tools/build-plugins.ps1 -OutDir target/debug/plugins
#>
param(
    [Parameter(Mandatory)]
    [string]$OutDir
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$plugins = @('model-center', 'translator', 'reply-assistant')

Push-Location $root
try {
    cargo build --release @($plugins | ForEach-Object { '-p', "plugin-$_" })
    if ($LASTEXITCODE -ne 0) { throw '编译插件失败' }
}
finally {
    Pop-Location
}

foreach ($plugin in $plugins) {
    $dir = Join-Path $root "plugins/$plugin"
    $manifest = Get-Content (Join-Path $dir 'plugin.json') -Raw -Encoding utf8 | ConvertFrom-Json
    $target = Join-Path $OutDir $manifest.id
    New-Item -ItemType Directory -Force $target | Out-Null
    Copy-Item (Join-Path $dir 'plugin.json') $target -Force
    Copy-Item (Join-Path $root "target/release/$($manifest.library)") $target -Force
    Write-Host "已复制到 $target"
}
