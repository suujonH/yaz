# 确认 Release 版没有编译开发者模式：在 exe 中搜索开发者页特有的文字（PE 中的字符串是 UTF-8）。
# 用法：先 cargo build -p yaz --release，再运行 ./tools/check-release-no-devmode.ps1
param([string]$Exe = "$PSScriptRoot/../target/release/yaz.exe")

$ErrorActionPreference = 'Stop'

# 开发者页、请求详情窗口、来源标签、模拟操作提示特有的文字
$absent = @(
    '开发者模式', '请求记录', '请求详情', '显示设置来源', '显示不可用的功能', '显示请求',
    '保存的条数', '只模拟操作', 'YaZ 设置 · 全局', 'YaZ 设置 · 账号',
    '搜索网址、请求头', 'settings-request-head'
)
# 对照：一定存在的文字。搜不到说明搜索方法有问题，不能得出“不存在”的结论
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
