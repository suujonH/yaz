# Confirm the Release build was compiled without developer mode: search the exe for text unique to the developer page (strings in a PE file are UTF-8).
# Usage: run cargo build -p yaz --release first, then ./tools/check-release-no-devmode.ps1
param([string]$Exe = "$PSScriptRoot/../target/release/yaz.exe")

$ErrorActionPreference = 'Stop'

# Text unique to the developer page, the request details window, the source labels and the simulated-action hint
$absent = @(
    '开发者模式', '请求记录', '请求详情', '显示设置来源', '显示不可用的功能', '显示请求',
    '保存的条数', '只模拟操作', 'YaZ 设置 · 全局', 'YaZ 设置 · 账号',
    '搜索网址、请求头', 'settings-request-head'
)
# Control: text that must be present. If it is not found, the search method is broken, and no conclusion of "absent" can be drawn
$present = @('超级调色盘', '数据目录配置已损坏', '使用默认目录')

$latin1 = [Text.Encoding]::GetEncoding(28591)
$content = $latin1.GetString([IO.File]::ReadAllBytes((Resolve-Path $Exe)))
function Test-Contains([string]$text) {
    $content.Contains($latin1.GetString([Text.Encoding]::UTF8.GetBytes($text)), [StringComparison]::Ordinal)
}

$failed = $false
foreach ($text in $present) {
    if (Test-Contains $text) { "应存在，已找到：$text" }
    else { "错误：应存在但没有找到：$text"; $failed = $true }
}
foreach ($text in $absent) {
    if (Test-Contains $text) { "错误：不应存在但找到了：$text"; $failed = $true }
    else { "不存在：$text" }
}
if ($failed) { exit 1 }
"通过：$Exe"
