#requires -Version 7
<#
.SYNOPSIS
  从 TDesign Icons（tdesign-icons-svg）生成 app/src/ui/icon_data.rs。

.DESCRIPTION
  需要新图标时，在 $icons 中追加一行后重新运行本脚本，不要手改 icon_data.rs。
  TDesign 没有的图标写在 $manual 中，原样输出。
  某个名字下载返回 404 时，换成语义最接近的 TDesign 图标，并在 $icons 的注释中说明。
  输出经过 rustfmt（在 PATH 中），和 cargo fmt 的结果一致。
#>
param(
    [string]$Out = (Join-Path $PSScriptRoot '..\app\src\ui\icon_data.rs')
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $true
$version = '0.4.7'

# IconName 成员 → TDesign 图标名（按输出顺序排列）；值以 'manual:' 开头的取自 $manual
$icons = [ordered]@{
    Logo            = 'manual:Logo'
    Chat            = 'chat'
    ChatFilled      = 'chat-filled'
    User            = 'user'
    UserFilled      = 'user-filled'
    Mobile          = 'mobile'
    ViewList        = 'view-list'
    Search          = 'search'
    Add             = 'add'
    UserSetting     = 'user-setting'
    ChevronRight    = 'chevron-right'
    Moon            = 'moon'
    Layout          = 'layout'
    Minus           = 'minus'
    Rectangle       = 'rectangle'
    Close           = 'close'
    NotificationOff = 'manual:NotificationOff'
    Edit            = 'edit'
    Check           = 'check'
    ChevronDown     = 'chevron-down'
    ArrowRightUp    = 'arrow-right-up'
    Forbid          = 'manual:Forbid'
    ChevronUpDouble = 'chevron-up-double'
    ChevronLeft     = 'chevron-left'
    Lock            = 'lock-on'
    Usergroup       = 'usergroup'
    Smile           = 'smile'
    Folder          = 'folder'
    Image           = 'image'
    File            = 'file'
    Time            = 'time'
    History         = 'history'
    More            = 'ellipsis'
    Star            = 'star'
    StarFilled      = 'star-filled'
    Setting         = 'setting'
    Palette         = 'palette'
    Share           = 'share'
    Link            = 'link'
    Download        = 'download'
    Logout          = 'logout'
    Pin             = 'pin'
    Filter          = 'filter'
    Location        = 'location'
    Video           = 'video'
    Quote           = 'quote'
    ErrorCircle     = 'error-circle'
    Loading         = 'loading'
    ZoomIn          = 'zoom-in'
    ZoomOut         = 'zoom-out'
    Fullscreen      = 'fullscreen'
    Copy            = 'file-copy'
    Cut             = 'cut'
    Paste           = 'paste'
    CheckCircle     = 'check-circle'
    ChatBubble      = 'chat-bubble'
    App             = 'app'
    FileZip         = 'file-zip'
    FilePdf         = 'file-pdf'
    FileWord        = 'file-word'
    FileExcel       = 'file-excel'
    FilePowerpoint  = 'file-powerpoint'
    FileUnknown     = 'file-unknown'
    Upload          = 'upload'
    Refresh         = 'refresh'
    Backtop         = 'backtop'
    Notification    = 'notification'
    Delete          = 'delete'
    Format          = 'edit-1'
    Bold            = 'textformat-bold'
    Italic          = 'textformat-italic'
    Strikethrough   = 'textformat-strikethrough'
    Code            = 'code'
    Spoiler         = 'browse-off'
    Formula         = 'formula'
    ListNumbered    = 'list-numbered'
    ListBulleted    = 'manual:ListBulleted'
    SourceCode      = 'file-code'
    Mail            = 'mail'
    Mention         = 'mentioned'
    RotateLeft      = 'anticlockwise'
    OpenExternal    = 'jump'
    Sound           = 'sound'
    Poll            = 'chart-bar'
    ViewModule      = 'view-module'
    MoreVertical    = 'more'
    AlignLeft       = 'manual:AlignLeft'
    AlignCenter     = 'manual:AlignCenter'
    AlignRight      = 'manual:AlignRight'
    Restore         = 'manual:Restore'
}

# TDesign 没有的图标：描边路径、填充路径、填充规则是否 evenodd
$manual = @{
    # 与程序图标（app/assets/yaz.ico）相同的气泡和字母 a
    Logo            = @('', 'M12 2A10 10 0 0 0 2 12A10 10 0 0 0 12 22H22V12A10 10 0 0 0 12 2Z M12.5 5.8A6.2 6.2 0 0 1 18.7 12V19.4H15.6V17.37A6.2 6.2 0 1 1 12.5 5.8Z M15.8 12A3.3 3.3 0 1 1 9.2 12A3.3 3.3 0 1 1 15.8 12Z M5.5 12.4A0.9 0.9 0 1 1 3.7 12.4A0.9 0.9 0 1 1 5.5 12.4Z', $true)
    NotificationOff = @('M5 8V13L3 16V19H21V16L19 13V8C19 4.13401 15.866 1 12 1C8.13401 1 5 4.13401 5 8Z M8.5 19H15.5C15.5 20.933 13.933 22.5 12 22.5C10.067 22.5 8.5 20.933 8.5 19Z M2 2L22 22', '', $false)
    ListBulleted    = @('M2 5H5M2 12H5M2 19H5M8 5H21M8 12H21M8 19H21', '', $false)
    AlignLeft       = @('M3 5H21M3 10H14M3 15H21M3 20H14', '', $false)
    AlignCenter     = @('M3 5H21M7 10H17M3 15H21M7 20H17', '', $false)
    AlignRight      = @('M3 5H21M10 10H21M3 15H21M10 20H21', '', $false)
    Forbid          = @('M21 12A9 9 0 1 1 3 12A9 9 0 1 1 21 12Z M5.63604 5.63604L18.364 18.364', '', $false)
    Restore         = @('M8 8V4H20V16H16M4 8H16V20H4V8Z', '', $false)
}

function Convert-Svg([string]$svg) {
    $strokes = @(); $fills = @(); $evenOdd = $false
    foreach ($m in [regex]::Matches($svg, '<path\b[^>]*>')) {
        $p = $m.Value
        $d = [regex]::Match($p, '\sd="([^"]+)"').Groups[1].Value
        if ($p -match 'stroke="currentColor"') { $strokes += $d }
        elseif ($p -match 'fill="currentColor"') {
            $fills += $d
            if ($p -match 'fill-rule="evenodd"') { $evenOdd = $true }
        }
    }
    if (-not ($strokes.Count -or $fills.Count)) { throw '图标中没有可用的 path' }
    return @(($strokes -join ' '), ($fills -join ' '), $evenOdd)
}

$sb = [System.Text.StringBuilder]::new()
[void]$sb.AppendLine("//! 图标数据：TDesign Icons（tdesign-icons-svg@$version），网格 24x24。TDesign 没有的手工绘制。")
[void]$sb.AppendLine('')
[void]$sb.AppendLine('/// 描边路径、填充路径、填充规则是否 evenodd。')
[void]$sb.AppendLine("pub type IconPaths = (&'static str, &'static str, bool);")
[void]$sb.AppendLine('')
[void]$sb.AppendLine('#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]')
[void]$sb.AppendLine('pub enum IconName {')
foreach ($key in $icons.Keys) { [void]$sb.AppendLine("    $key,") }
[void]$sb.AppendLine('}')
[void]$sb.AppendLine('')
[void]$sb.AppendLine('impl IconName {')
[void]$sb.AppendLine("    pub const ALL: &'static [IconName] = &[")
foreach ($key in $icons.Keys) { [void]$sb.AppendLine("        IconName::$key,") }
[void]$sb.AppendLine('    ];')
[void]$sb.AppendLine('')
[void]$sb.AppendLine('    pub const fn paths(self) -> IconPaths {')
[void]$sb.AppendLine('        match self {')

foreach ($key in $icons.Keys) {
    $name = $icons[$key]
    if ($name.StartsWith('manual:')) {
        $paths = $manual[$name.Substring(7)]
    }
    else {
        $url = "https://unpkg.com/tdesign-icons-svg@$version/src/$name.svg"
        try {
            $svg = (Invoke-WebRequest -UseBasicParsing $url -TimeoutSec 30).Content
        }
        catch {
            throw "下载 $name 失败（$url）：$($_.Exception.Message)"
        }
        $paths = Convert-Svg $svg
    }
    $evenOdd = if ($paths[2]) { 'true' } else { 'false' }
    [void]$sb.AppendLine("            IconName::$key => (`"$($paths[0])`", `"$($paths[1])`", $evenOdd),")
}

[void]$sb.AppendLine('        }')
[void]$sb.AppendLine('    }')
[void]$sb.AppendLine('}')

$path = Join-Path (Resolve-Path -LiteralPath (Split-Path $Out)).Path (Split-Path $Out -Leaf)
[System.IO.File]::WriteAllText($path, $sb.ToString(), [System.Text.UTF8Encoding]::new($false))
rustfmt --edition 2024 $path
Write-Host "已生成 $Out（$($icons.Count) 个图标）"
