#Requires -Version 7
<#
.SYNOPSIS
  向测试用 Zulip 服务器写入种子数据（用户、频道、话题、消息、私聊、上传、星标、草稿等）。

.DESCRIPTION
  脚本是幂等的：已存在的用户 / 频道会跳过；频道或私聊中已有消息时不再生成消息。
  密码通过参数传入，不写入仓库；各账号的 API key 输出到 -OutFile（请放在仓库之外）。

  所有者需要 can_create_users 权限才能通过 API 创建用户。

.EXAMPLE
  ./tools/seed-zulip.ps1 -OwnerPassword (Get-Content owner.pw) -UserPassword (Get-Content user.pw) -OutFile $env:TEMP\zulip-keys.json
#>
param(
    [string]$Realm = 'https://localhost.localdomain:8443',
    [string]$OwnerEmail = 'owner@zulip.test',
    [Parameter(Mandatory)][string]$OwnerPassword,
    [Parameter(Mandatory)][string]$UserPassword,
    [Parameter(Mandatory)][string]$OutFile
)

$ErrorActionPreference = 'Stop'
$Realm = $Realm.TrimEnd('/')

function Get-AuthHeader([pscustomobject]$Account) {
    $raw = [Text.Encoding]::UTF8.GetBytes("$($Account.Email):$($Account.Key)")
    @{ Authorization = 'Basic ' + [Convert]::ToBase64String($raw) }
}

# 调用 Zulip API；429 时按 Retry-After 等待后重试
function Invoke-Zulip {
    param([pscustomobject]$Account, [string]$Method, [string]$Path, [hashtable]$Body)
    for ($attempt = 0; ; $attempt++) {
        try {
            $req = @{
                Uri = "$Realm/api/v1/$Path"; Method = $Method; SkipCertificateCheck = $true
                Headers = (Get-AuthHeader $Account)
            }
            if ($Body) {
                # 手动编码为 UTF-8 表单：PowerShell 对 PATCH 的哈希表 Body 不会按表单发送
                $q = ($Body.GetEnumerator() | ForEach-Object { "$($_.Key)=$([Uri]::EscapeDataString([string]$_.Value))" }) -join '&'
                if ($Method -eq 'GET') { $req.Uri += "?$q" }
                else {
                    $req.Body = $q
                    $req.ContentType = 'application/x-www-form-urlencoded; charset=utf-8'
                }
            }
            return Invoke-RestMethod @req
        } catch {
            $resp = $_.Exception.Response
            if ($resp -and [int]$resp.StatusCode -eq 429 -and $attempt -lt 5) {
                $wait = 2
                if ($resp.Headers.RetryAfter -and $resp.Headers.RetryAfter.Delta) { $wait = [math]::Ceiling($resp.Headers.RetryAfter.Delta.TotalSeconds) }
                Start-Sleep -Seconds ([math]::Max($wait, 1))
                continue
            }
            throw
        }
    }
}

function Get-ApiKey([string]$Email, [string]$Password) {
    $r = Invoke-RestMethod -Uri "$Realm/api/v1/fetch_api_key" -Method Post -SkipCertificateCheck `
        -Body @{ username = $Email; password = $Password }
    [pscustomobject]@{ Email = $r.email; Key = $r.api_key; Id = [long]$r.user_id }
}

function ConvertTo-Json1($Value) { ConvertTo-Json -InputObject $Value -Compress -Depth 5 }

# ---------- 1. 账号 ----------
Write-Host '== 账号'
$owner = Get-ApiKey $OwnerEmail $OwnerPassword
$owner | Add-Member Name '群主大大'

$names = '阿杰', '小雨', '老王', '晓晓', '大熊', '米粒', '阿飞', '橙子', '小鹿', '石头', '可乐', '豆豆'
$existing = (Invoke-Zulip $owner GET 'users').members
$users = @()
for ($i = 1; $i -le $names.Count; $i++) {
    $email = 'u{0:d2}@zulip.test' -f $i
    $name = $names[$i - 1]
    if (-not ($existing | Where-Object { $_.email -eq $email -or $_.delivery_email -eq $email })) {
        Invoke-Zulip $owner POST 'users' @{ email = $email; password = $UserPassword; full_name = $name } | Out-Null
        Write-Host "  创建 $email $name"
    }
    $u = Get-ApiKey $email $UserPassword
    $u | Add-Member Name $name
    $users += $u
}
$everyone = @($owner) + $users

function Get-Mention([pscustomobject]$U) { "@**$($U.Name)|$($U.Id)**" }

# ---------- 2. 频道 ----------
Write-Host '== 频道'
$channels = @(
    @{ Name = '开发'; Description = '开发讨论。**群公告**：周五下午 3 点例会。'; Private = $false; Members = $everyone
       Topics = '接口设计', '构建失败', '代码评审', '发布计划' }
    @{ Name = '游戏'; Description = '下班一起打游戏'; Private = $false; Members = $everyone
       Topics = '周末开黑', '新游推荐', '攻略分享', '吐槽' }
    @{ Name = '日常'; Description = '吃喝玩乐，随便聊'; Private = $false; Members = $everyone
       Topics = '午饭吃什么', '周末去哪', '宠物', '天气' }
    @{ Name = '公告'; Description = '仅核心成员可见'; Private = $true; Members = @($owner) + $users[0..3]
       Topics = '人事通知', '假期安排', '预算' }
)
foreach ($c in $channels) {
    $principals = ConvertTo-Json1 @($c.Members | ForEach-Object { $_.Id })
    Invoke-Zulip $owner POST 'users/me/subscriptions' @{
        subscriptions = ConvertTo-Json1 @(@{ name = $c.Name; description = $c.Description })
        principals = $principals
        invite_only = $c.Private.ToString().ToLower()
    } | Out-Null
    $c.Id = (Invoke-Zulip $owner GET 'get_stream_id' @{ stream = $c.Name }).stream_id
    Write-Host "  #$($c.Name) id=$($c.Id) 成员 $($c.Members.Count)"
}

function Send-Channel([pscustomobject]$From, [hashtable]$Channel, [string]$Topic, [string]$Content) {
    (Invoke-Zulip $From POST 'messages' @{ type = 'channel'; to = $Channel.Id; topic = $Topic; content = $Content }).id
}

function Send-Direct([pscustomobject]$From, [long[]]$To, [string]$Content) {
    (Invoke-Zulip $From POST 'messages' @{ type = 'direct'; to = (ConvertTo-Json1 $To); content = $Content }).id
}

function Test-NarrowEmpty([pscustomobject]$Account, $Narrow) {
    $r = Invoke-Zulip $Account GET 'messages' @{ anchor = 'newest'; num_before = 1; num_after = 0; narrow = (ConvertTo-Json1 $Narrow) }
    $r.messages.Count -eq 0
}

# ---------- 3. 上传 ----------
# 上传文件。不用 -Form：它会把所有文件都标为 application/octet-stream，
# Zulip 因此不会把图片当作图片（没有预览，has:image 也查不到）
$uploadClient = [Net.Http.HttpClient]::new([Net.Http.HttpClientHandler]@{ ServerCertificateCustomValidationCallback = [Net.Http.HttpClientHandler]::DangerousAcceptAnyServerCertificateValidator })
function New-Upload([pscustomobject]$From, [string]$FileName, [byte[]]$Bytes) {
    $mime = switch ([IO.Path]::GetExtension($FileName).ToLowerInvariant()) {
        '.png' { 'image/png' } '.jpg' { 'image/jpeg' } '.txt' { 'text/plain' } '.zip' { 'application/zip' }
        default { 'application/octet-stream' }
    }
    $file = [Net.Http.ByteArrayContent]::new($Bytes)
    $file.Headers.ContentType = [Net.Http.Headers.MediaTypeHeaderValue]::new($mime)
    $form = [Net.Http.MultipartFormDataContent]::new()
    $form.Add($file, 'file', $FileName)
    $msg = [Net.Http.HttpRequestMessage]::new([Net.Http.HttpMethod]::Post, "$Realm/api/v1/user_uploads")
    $msg.Headers.Authorization = [Net.Http.Headers.AuthenticationHeaderValue]::Parse((Get-AuthHeader $From).Authorization)
    $msg.Content = $form
    $resp = $uploadClient.SendAsync($msg).GetAwaiter().GetResult()
    $json = $resp.Content.ReadAsStringAsync().GetAwaiter().GetResult() | ConvertFrom-Json
    if (-not $resp.IsSuccessStatusCode) { throw "上传 $FileName 失败：$($json.msg)" }
    "[$FileName]($($json.url))"
}
function New-Png([int]$Seed) {
    Add-Type -AssemblyName System.Drawing
    $bmp = [Drawing.Bitmap]::new(320, 200)
    $g = [Drawing.Graphics]::FromImage($bmp)
    $rnd = [Random]::new($Seed)
    $g.Clear([Drawing.Color]::FromArgb($rnd.Next(80, 255), $rnd.Next(80, 255), $rnd.Next(80, 255)))
    $g.DrawString("YaZ #$Seed", [Drawing.Font]::new('Arial', 24), [Drawing.Brushes]::Black, 20, 80)
    $ms = [IO.MemoryStream]::new()
    $bmp.Save($ms, [Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    $ms.ToArray()
}

# ---------- 4. 频道消息 ----------
Write-Host '== 频道消息'
$rnd = [Random]::new(42)
$lines = @(
    '大家好！', '收到', '这个我来跟进', '有人知道怎么回事吗？', '+1', '哈哈哈 :smile:', '我晚点看一下',
    '已经解决了', '明天再说吧', '好的没问题', '辛苦了 :thumbs_up:', '这周能搞定吗', '我觉得可以', '先这样吧',
    '有点意思', '同意楼上', '等我五分钟', '已提交', '谁有空帮忙看看', '我也遇到过'
)
function Get-NarrowCount([pscustomobject]$Account, $Narrow, [int]$Max) {
    $r = Invoke-Zulip $Account GET 'messages' @{ anchor = 'newest'; num_before = $Max; num_after = 0; narrow = (ConvertTo-Json1 $Narrow) }
    $r.messages.Count
}

# 每个话题补足 25 条；中途失败后重新运行会从缺少的条数继续
foreach ($c in $channels) {
    $others = @($c.Members | Where-Object { $_.Id -ne $owner.Id })
    $added = 0
    foreach ($t in $c.Topics) {
        # 话题可能已被第 6 步标记为已解决（改名为 "✔ 话题"），两个名字一起计数
        $existing = 0
        foreach ($name in $t, "✔ $t") {
            $existing += Get-NarrowCount $owner @(@{ operator = 'channel'; operand = $c.Id }, @{ operator = 'topic'; operand = $name }) 25
        }
        $missing = 25 - $existing
        for ($n = 0; $n -lt $missing; $n++) {
            $from = $c.Members[$rnd.Next($c.Members.Count)]
            $text = $lines[$rnd.Next($lines.Count)]
            switch ($rnd.Next(14)) {
                0 { $text = "$(Get-Mention $others[$rnd.Next($others.Count)]) $text" }
                1 { $text = "``````python`nprint('hello $t')`n``````" }
                2 { $text = "**$t**：$text`n- 第一点`n- 第二点" }
                3 { $text = "``````quote`n$($lines[$rnd.Next($lines.Count)])`n```````n$text" }
                default { }
            }
            Send-Channel $from $c $t $text | Out-Null
            $added++
        }
    }
    # 文件与图片（放在第一个话题）
    $from = $others[0]
    $first = @(@{ operator = 'channel'; operand = $c.Id }, @{ operator = 'topic'; operand = $c.Topics[0] })
    if (Test-NarrowEmpty $owner ($first + @(@{ operator = 'has'; operand = 'attachment' }))) {
        Send-Channel $from $c $c.Topics[0] ("文档在这里 " + (New-Upload $from "说明-$($c.Name).txt" ([Text.Encoding]::UTF8.GetBytes("测试文件 $($c.Name)`n" * 50)))) | Out-Null
        $added++
    }
    if (Test-NarrowEmpty $owner ($first + @(@{ operator = 'has'; operand = 'image' }))) {
        Send-Channel $from $c $c.Topics[0] ("截图 " + (New-Upload $from "截图-$($c.Name).png" (New-Png $c.Id))) | Out-Null
        $added++
    }
    Write-Host "  #$($c.Name) 新增 $added 条"
}

# ---------- 5. 私聊 ----------
Write-Host '== 私聊'
if (Test-NarrowEmpty $owner @(@{ operator = 'dm'; operand = @($users[0].Id) })) {
    foreach ($u in $users[0..4]) {
        Send-Direct $u @($owner.Id) "在吗？我是$($u.Name)" | Out-Null
        Send-Direct $owner @($u.Id) '在的，怎么了' | Out-Null
        Send-Direct $u @($owner.Id) $lines[$rnd.Next($lines.Count)] | Out-Null
    }
    Send-Direct $users[0] @($owner.Id) ("给你发个压缩包 " + (New-Upload $users[0] '资料.zip' ([byte[]](1..2048 | ForEach-Object { $rnd.Next(256) })))) | Out-Null
    Send-Direct $users[1] @($owner.Id, $users[2].Id, $users[3].Id) '周末一起吃饭？' | Out-Null
    Send-Direct $users[2] @($owner.Id, $users[1].Id, $users[3].Id) '好啊 :smile:' | Out-Null
    Send-Direct $users[4] @($owner.Id, $users[5].Id) '这个讨论组只有三个人' | Out-Null
    Send-Direct $owner @($owner.Id) '给自己的备忘：买牛奶' | Out-Null
    Write-Host '  已生成'
} else { Write-Host '  已有私聊，跳过' }

# ---------- 6. 所有者状态 ----------
# 每一步单独判断是否已完成，脚本中途失败后可以直接重新运行
Write-Host '== 未读、星标、表情、草稿、已解决、静音'
$dev = $channels[0]; $game = $channels[1]
function Test-OwnerNarrowEmpty($Narrow) { Test-NarrowEmpty $owner $Narrow }

# 先把历史消息全部标为已读，再制造一批新的未读。
# 以"构建失败"话题中 @我 的消息是否存在作为完成标记；中途失败时重新运行会再执行一次
$mentionNarrow = @(@{ operator = 'channel'; operand = $dev.Id }, @{ operator = 'topic'; operand = '构建失败' }, @{ operator = 'is'; operand = 'mentioned' })
if (Test-OwnerNarrowEmpty $mentionNarrow) {
    $anchor = 'oldest'; $include = 'true'
    do {
        $r = Invoke-Zulip $owner POST 'messages/flags/narrow' @{
            anchor = $anchor; include_anchor = $include; num_before = 0; num_after = 5000
            narrow = '[]'; op = 'add'; flag = 'read'
        }
        $anchor = $r.last_processed_id; $include = 'false'
    } while (-not $r.found_newest)

    for ($n = 0; $n -lt 30; $n++) { Send-Channel $users[$n % 6] $dev '构建失败' "第 $n 条：CI 又挂了" | Out-Null }
    Send-Channel $users[1] $dev '构建失败' "$(Get-Mention $owner) 帮忙看一下这个报错" | Out-Null
    for ($n = 0; $n -lt 10; $n++) { Send-Channel $users[$n % 6] $dev '构建失败' "补充信息 $n" | Out-Null }
    Send-Channel $users[2] $game '周末开黑' '@**all** 周六晚上 8 点开黑，都来！' | Out-Null
    Send-Channel $users[3] $channels[2] '宠物' ("我家猫 " + (New-Upload $users[3] '猫.txt' ([Text.Encoding]::UTF8.GetBytes('喵' * 100)))) | Out-Null
    Send-Direct $users[0] @($owner.Id) '有空回我一下' | Out-Null
    Write-Host '  未读已生成'
}

$recent = (Invoke-Zulip $owner GET 'messages' @{ anchor = 'newest'; num_before = 200; num_after = 0; narrow = '[]' }).messages

# 星标 10 条
if (Test-OwnerNarrowEmpty @(@{ operator = 'is'; operand = 'starred' })) {
    $star = @($recent | Select-Object -First 120 | Get-Random -Count 10 -SetSeed 7 | ForEach-Object { $_.id })
    Invoke-Zulip $owner POST 'messages/flags' @{ messages = (ConvertTo-Json1 $star); op = 'add'; flag = 'starred' } | Out-Null
    Write-Host '  星标已生成'
}

# 表情回应（重复添加会返回错误，忽略即可）
foreach ($m in ($recent | Select-Object -First 20 | Select-Object -Last 8)) {
    foreach ($u in $users[0..2]) {
        try { Invoke-Zulip $u POST "messages/$($m.id)/reactions" @{ emoji_name = 'thumbs_up' } | Out-Null } catch { }
    }
}

# 草稿
if ((Invoke-Zulip $owner GET 'drafts').count -eq 0) {
    Invoke-Zulip $owner POST 'drafts' @{ drafts = (ConvertTo-Json1 @(@{
        type = 'stream'; to = @($game.Id); topic = '新游推荐'; content = '我想推荐一个……'; timestamp = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    })) } | Out-Null
    Write-Host '  草稿已生成'
}

# 已解决的话题
$reviewNarrow = @(@{ operator = 'channel'; operand = $dev.Id }, @{ operator = 'topic'; operand = '代码评审' })
if (-not (Test-OwnerNarrowEmpty $reviewNarrow)) {
    $last = (Invoke-Zulip $owner GET 'messages' @{ anchor = 'newest'; num_before = 1; num_after = 0; narrow = (ConvertTo-Json1 $reviewNarrow) }).messages[0]
    Invoke-Zulip $owner PATCH "messages/$($last.id)" @{
        topic = '✔ 代码评审'; propagate_mode = 'change_all'
        send_notification_to_old_thread = 'false'; send_notification_to_new_thread = 'false'
    } | Out-Null
    Write-Host '  话题已标记为已解决'
}

# 静音话题（重复设置无副作用）
Invoke-Zulip $owner POST 'user_topics' @{ stream_id = $game.Id; topic = '吐槽'; visibility_policy = 1 } | Out-Null

$uploadClient.Dispose()

# ---------- 7. 输出 ----------
$everyone | Select-Object Email, Name, Id, Key | ConvertTo-Json | Set-Content -Encoding utf8 $OutFile
$everyone | Format-Table Email, Name, Id -AutoSize
Write-Host "API key 已写入 $OutFile"
