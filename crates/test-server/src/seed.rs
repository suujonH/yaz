//! Chinese-language seed data: some users, channels, topics, messages, DMs, image uploads, starred
//! messages, drafts, polls, tables, code blocks, and quotes.

use serde_json::{Map, Value, json};

use crate::state::{Chan, State, Target, Upload, User};
use crate::{BOT_ID, ME_EMAIL, ME_ID};

/// A 1x1 pixel PNG: used for both uploaded images and avatars.
pub const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// id, name, description, whether private, color, topics.
type ChannelSeed = (
    i64,
    &'static str,
    &'static str,
    bool,
    &'static str,
    &'static [&'static str],
);

const OTHERS: [&str; 8] = [
    "阿杰", "小雨", "老王", "晓晓", "大熊", "米粒", "阿飞", "橙子",
];

pub const AJIE: i64 = 2;
pub const XIAOYU: i64 = 3;
pub const LAOWANG: i64 = 4;

pub const DEV: i64 = 1;
pub const GAME: i64 = 2;
pub const DAILY: i64 = 3;
pub const NOTICE: i64 = 4;

const CHATTER: [&str; 8] = [
    "大家好！",
    "收到",
    "这个我来跟进",
    "周五之前能搞定吗？",
    "**重要**：先看一下文档",
    "可以，没问题",
    "我晚点回复",
    "哈哈哈",
];

const STRESS_MESSAGES: usize = 5000;

/// Announcements only a real server would generate: images, custom emoji, collapsing, tables,
/// files, http links, and content that must be stripped or must send no request (`javascript:`
/// links, event attributes, image URLs like `/http://...` that point to another site once the
/// leading `/` is removed). During verification, listen on port 9996: there should be no
/// connection; 9997 is the control, where the same listener should receive a connection.
const NOTICE_FIXTURE: &str = concat!(
    r#"<p>公告 <strong>粗体</strong> <a href="http://127.0.0.1:9997/notice-link">http 链接</a> "#,
    r#"<a class="bad-link" href="javascript:window.__xss=1">恶意链接</a> "#,
    r#"表情 <img class="emoji" alt=":custom:" title="custom" src="/user_uploads/1/ef/大图.png"></p>"#,
    r#"<div class="message_inline_image"><a href="/user_uploads/1/ef/大图.png" title="大图.png">"#,
    r#"<img data-original-dimensions="640x480" src="/user_uploads/1/ef/大图.png" onerror="window.__xss=1"></a></div>"#,
    r#"<p>恶意图片 <img class="bad-image" src="/http://127.0.0.1:9996/evil.png"> "#,
    r#"<img class="emoji" title="evil" src="/http://127.0.0.1:9996/evil-emoji.png"> "#,
    r#"对照 <img class="emoji" title="control" src="http://127.0.0.1:9997/control.png"></p>"#,
    r#"<div class="spoiler-block"><div class="spoiler-header"><p>折叠标题</p></div>"#,
    r#"<div class="spoiler-content" aria-hidden="true"><p>折叠内容</p></div></div>"#,
    r#"<table><thead><tr><th>甲</th></tr></thead><tbody><tr><td>乙</td></tr></tbody></table>"#,
    r#"<p><a href="/user_uploads/1/cd/周报.txt">周报.txt</a></p>"#,
);

pub fn build(now: i64) -> State {
    let mut s = State {
        next_id: 1000,
        settings: default_settings(),
        ..State::default()
    };
    s.users.push(user(ME_ID, ME_EMAIL, "测试用户", false, 100));
    for (i, name) in OTHERS.iter().enumerate() {
        let id = i as i64 + 2;
        s.users
            .push(user(id, &format!("user{id}@zulip.test"), name, false, 400));
    }
    s.users.push(user(
        BOT_ID,
        "notification-bot@zulip.test",
        "Notification Bot",
        true,
        400,
    ));

    let topics: [ChannelSeed; 4] = [
        (
            DEV,
            "开发",
            "开发讨论。**群公告**：周五下午 3 点例会。",
            false,
            "#76ce90",
            &["接口设计", "构建失败", "代码评审", "发布计划"],
        ),
        (
            GAME,
            "游戏",
            "打游戏的来这里",
            false,
            "#fae589",
            &["周末开黑", "新游推荐", "吐槽"],
        ),
        (
            DAILY,
            "日常",
            "吃喝玩乐",
            false,
            "#a6c7e5",
            &["午饭吃什么", "宠物"],
        ),
        (
            NOTICE,
            "公告",
            "公司公告",
            true,
            "#e4523d",
            &["人事通知", "假期安排"],
        ),
    ];
    for (id, name, description, invite_only, color, _) in topics {
        s.channels.push(Chan {
            id,
            name: name.to_owned(),
            description: description.to_owned(),
            rendered_description: None,
            invite_only,
            color: color.to_owned(),
            subscribed: true,
            is_muted: false,
            properties: Map::new(),
            date_created: now - 86400 * 30,
        });
    }

    let mut ts = now - 86400 * 3;
    let mut tick = |step: i64| {
        ts += step;
        ts
    };
    let mut line = 0;
    for (stream, _, _, _, _, names) in topics {
        for topic in names {
            for _ in 0..5 {
                let sender = 2 + (line as i64 % OTHERS.len() as i64);
                let at = tick(600);
                let id = s.insert(
                    sender,
                    Target::Stream(stream, (*topic).to_owned()),
                    CHATTER[line % CHATTER.len()],
                    at,
                );
                mark_read(&mut s, id);
                line += 1;
            }
        }
    }

    let dev = |topic: &str| Target::Stream(DEV, topic.to_owned());
    let code = "看一下这段：\n```rust\nfn main() {\n    println!(\"你好\");\n}\n```";
    let id = s.insert(AJIE, dev("接口设计"), code, tick(600));
    mark_read(&mut s, id);
    let table = "| 接口 | 状态 |\n|---|---|\n| 登录 | 完成 |\n| 上传 | 进行中 |";
    let id = s.insert(XIAOYU, dev("接口设计"), table, tick(600));
    mark_read(&mut s, id);
    let quote = "@_**阿杰|2** [发言道](#narrow/channel/1)：\n```quote\n看一下这段\n```\n已经看了";
    let id = s.insert(LAOWANG, dev("接口设计"), quote, tick(600));
    mark_read(&mut s, id);
    let list = "发布步骤：\n1. 打标签\n2. 构建\n3. 发公告";
    let release_id = s.insert(ME_ID, dev("发布计划"), list, tick(600));
    star(&mut s, release_id);
    let poll = "/poll 周五吃什么\n火锅\n烧烤\n沙县";
    let id = s.insert(
        XIAOYU,
        Target::Stream(DAILY, "午饭吃什么".into()),
        poll,
        tick(600),
    );
    mark_read(&mut s, id);

    s.uploads.insert(
        "1/ab/截图.png".to_owned(),
        Upload {
            bytes: TINY_PNG.to_vec(),
            content_type: "image/png".to_owned(),
            name: "截图.png".to_owned(),
            attachment_id: 1,
            created_at: now - 3600,
            dangling_refs: Vec::new(),
        },
    );
    s.uploads.insert(
        "1/cd/周报.txt".to_owned(),
        Upload {
            bytes: "本周完成了登录模块。".as_bytes().to_vec(),
            content_type: "text/plain".to_owned(),
            name: "周报.txt".to_owned(),
            attachment_id: 2,
            created_at: now - 3600,
            dangling_refs: Vec::new(),
        },
    );
    s.uploads.insert(
        "1/gh/旧资料.txt".to_owned(),
        Upload {
            bytes: "引用它的消息已删除。".as_bytes().to_vec(),
            content_type: "text/plain".to_owned(),
            name: "旧资料.txt".to_owned(),
            attachment_id: 4,
            created_at: now - 7200,
            dangling_refs: vec![999_999],
        },
    );
    let id = s.insert(
        ME_ID,
        dev("构建失败"),
        "错误截图：[截图.png](/user_uploads/1/ab/截图.png)",
        tick(600),
    );
    star(&mut s, id);
    let id = s.insert(
        AJIE,
        dev("代码评审"),
        "[周报.txt](/user_uploads/1/cd/周报.txt)",
        tick(600),
    );
    mark_read(&mut s, id);

    for text in ["在吗？", "下午开会", "好的"] {
        let id = s.insert(AJIE, Target::Dm(vec![AJIE, ME_ID]), text, tick(300));
        mark_read(&mut s, id);
    }
    s.insert(ME_ID, Target::Dm(vec![AJIE, ME_ID]), "马上到", tick(300));
    let id = s.insert(
        XIAOYU,
        Target::Dm(vec![XIAOYU, LAOWANG, ME_ID]),
        "群聊第一句",
        tick(300),
    );
    mark_read(&mut s, id);
    s.insert(
        ME_ID,
        Target::Dm(vec![ME_ID]),
        "给自己的备忘：买牛奶",
        tick(300),
    );

    // Unread: some DMs, mentions, and channel messages each
    s.insert(
        LAOWANG,
        Target::Dm(vec![LAOWANG, ME_ID]),
        "周末有空吗？",
        tick(60),
    );
    s.insert(
        XIAOYU,
        Target::Stream(GAME, "周末开黑".into()),
        "@**测试用户|1** 今晚来吗？",
        tick(60),
    );
    s.insert(
        AJIE,
        Target::Stream(DEV, "发布计划".into()),
        "@**all** 今天下午发版",
        tick(60),
    );
    s.insert(
        AJIE,
        Target::Stream(DEV, "发布计划".into()),
        "记得更新文档",
        tick(60),
    );

    // DMs from Notification Bot: system notifications and delivered reminders
    s.insert(
        BOT_ID,
        Target::Dm(vec![BOT_ID, ME_ID]),
        "@_**阿杰|2** 加入了组织。欢迎新同事！",
        tick(60),
    );
    let reminder = format!(
        "You requested a reminder for the following message.\n\n@_**测试用户|1** [said](#narrow/channel/1-开发/topic/发布计划/near/{release_id}) in #**开发>发布计划**:\n```quote\n发布步骤：\n1. 打标签\n2. 构建\n3. 发公告\n```"
    );
    s.insert(BOT_ID, Target::Dm(vec![BOT_ID, ME_ID]), &reminder, tick(60));

    s.drafts.insert(
        1,
        json!({
            "id": 1, "type": "stream", "to": [GAME], "topic": "新游推荐",
            "content": "我想推荐一个……", "timestamp": now - 1800,
        }),
    );
    s.user_topics.push(json!({
        "stream_id": GAME, "topic_name": "吐槽",
        "visibility_policy": 1, "last_updated": now - 3600,
    }));
    s.presences.insert(
        AJIE.to_string(),
        json!({"active_timestamp": now, "idle_timestamp": now}),
    );
    s.presences.insert(
        XIAOYU.to_string(),
        json!({"active_timestamp": now - 300, "idle_timestamp": now}),
    );
    s.user_status
        .insert(AJIE.to_string(), json!({"status_text": "摸鱼中"}));
    s.snippets.insert(
        1,
        json!({"id": 1, "title": "问候", "content": "大家好！", "date_created": now - 7200}),
    );
    if std::env::var_os("TEST_SERVER_CHAT_FIXTURES").is_some() {
        chat_fixtures(&mut s, now);
    }
    s.next_id = s.next_id.max(*s.messages.keys().last().unwrap_or(&0));
    s
}

/// Data for chat page verification: many messages, unread positioning, and HTML only a real server
/// generates (the fake server's Markdown does not support this syntax).
fn chat_fixtures(s: &mut State, now: i64) {
    let dev = |topic: &str| Target::Stream(DEV, topic.to_owned());
    let mut at = now - 400_000;
    for i in 0..STRESS_MESSAGES {
        at += 60;
        let sender = if i % 7 == 0 {
            ME_ID
        } else {
            2 + (i as i64 % OTHERS.len() as i64)
        };
        let text = match i % 11 {
            3 => format!("第 {i} 条：`行内代码` 和 **粗体**"),
            7 => format!("第 {i} 条：\n```rust\nfn main() {{ println!(\"{i}\"); }}\n```"),
            _ => format!("第 {i} 条：{}", CHATTER[i % CHATTER.len()]),
        };
        let id = s.insert(sender, dev("压力测试"), &text, at);
        mark_read(s, id);
    }
    for i in 0..60 {
        at += 60;
        let mention = if i == 45 { "@**测试用户|1** " } else { "" };
        let id = s.insert(AJIE, dev("未读定位"), &format!("{mention}未读定位 {i}"), at);
        if i < 30 {
            mark_read(s, id);
        }
    }
    s.uploads.insert(
        "1/ef/大图.png".to_owned(),
        Upload {
            bytes: TINY_PNG.to_vec(),
            content_type: "image/png".to_owned(),
            name: "大图.png".to_owned(),
            attachment_id: 3,
            created_at: now - 3600,
            dangling_refs: Vec::new(),
        },
    );
    // Resources page: two references to the same file, one loads and the other makes the server
    // error when fetching the message
    const FAILING_REFERENCE: i64 = 999_998;
    s.uploads.insert(
        "1/ij/部分失败.txt".to_owned(),
        Upload {
            bytes: "引用只有一部分能加载。".as_bytes().to_vec(),
            content_type: "text/plain".to_owned(),
            name: "部分失败.txt".to_owned(),
            attachment_id: 5,
            created_at: now - 3600,
            dangling_refs: vec![FAILING_REFERENCE],
        },
    );
    s.failing_messages.insert(FAILING_REFERENCE);
    let id = s.insert(
        AJIE,
        Target::Stream(GAME, "周末开黑".to_owned()),
        "[部分失败.txt](/user_uploads/1/ij/部分失败.txt)",
        now - 3600,
    );
    mark_read(s, id);
    if let Some(channel) = s.channels.iter_mut().find(|c| c.id == GAME) {
        channel.rendered_description = Some(NOTICE_FIXTURE.to_owned());
    }
    let wide: String = (0..30)
        .map(|i| format!("<th>很长的表头第{i}列</th>"))
        .collect();
    let cells: String = (0..30).map(|i| format!("<td>内容{i}</td>")).collect();
    let tall: String = (0..40)
        .map(|i| format!("<tr><td>行{i}</td><td>值</td></tr>"))
        .collect();
    let image = |n: &str| {
        format!(
            r#"<div class="message_inline_image"><a href="/user_uploads/1/ef/大图.png" title="大图.png"><img data-original-dimensions="{n}" src="/user_uploads/1/ef/大图.png"></a></div>"#
        )
    };
    let samples = [
        (AJIE, "<p>行内 <code>code()</code>，<strong>粗</strong> <em>斜</em> <del>删</del>，<a href=\"https://example.com/a\">外部链接</a></p>".to_owned()),
        (ME_ID, "<p>我的气泡：<span class=\"user-mention\" data-user-id=\"2\">@阿杰</span> <code>code</code> <a href=\"https://example.com/\">链接</a></p>".to_owned()),
        (XIAOYU, format!("<p>提及我 <span class=\"user-mention\" data-user-id=\"{ME_ID}\">@测试用户</span> 全体 <span class=\"user-mention channel-wildcard-mention\" data-user-id=\"*\">@all</span> 表情 <span aria-label=\"smile\" class=\"emoji emoji-1f604\" role=\"img\" title=\"smile\">:smile:</span></p>")),
        (AJIE, format!("<div class=\"codehilite\" data-code-language=\"Rust\"><pre><span></span><code><span class=\"k\">fn</span> long() {{ let s = \"{}\"; }}\n</code></pre></div>", "很长的一行".repeat(40))),
        (LAOWANG, format!("<p>宽表格</p><table><thead><tr>{wide}</tr></thead><tbody><tr>{cells}</tr></tbody></table>")),
        (XIAOYU, format!("<table><thead><tr><th>高表格</th><th style=\"text-align: right;\">值</th></tr></thead><tbody>{tall}<tr><td>换行a&lt;br /&gt;换行b</td><td>x</td></tr></tbody></table>")),
        (AJIE, "<p><span class=\"user-mention silent\" data-user-id=\"3\">小雨</span> <a href=\"#narrow/channel/1/topic/x/near/1001\">发言道</a>：</p><blockquote><p>这段是引用</p></blockquote><blockquote><p><span class=\"user-mention silent\" data-user-id=\"3\">小雨</span> <a href=\"/#narrow/channel/1-dev/topic/x/near/1001\">发言道</a>：</p><p>引用块带跳转</p></blockquote>".to_owned()),
        (AJIE, "<div class=\"spoiler-block\"><div class=\"spoiler-header\"><p>剧透标题</p></div><div class=\"spoiler-content\" aria-hidden=\"true\"><p>隐藏的内容</p></div></div><div class=\"spoiler-block\"><div class=\"spoiler-header\"></div><div class=\"spoiler-content\" aria-hidden=\"true\"><p>没有标题</p></div></div>".to_owned()),
        (XIAOYU, "<p>公式 <span class=\"katex\"><span class=\"katex-mathml\"><math><semantics><annotation encoding=\"application/x-tex\">E=mc^2</annotation></semantics></math></span><span class=\"katex-html\">E=mc2</span></span></p><p><span class=\"katex-display\"><span class=\"katex\"><span class=\"katex-mathml\"><math><semantics><annotation encoding=\"application/x-tex\">\\sum_i x_i</annotation></semantics></math></span></span></span></p>".to_owned()),
        (AJIE, format!("<p>两张图的相册</p>{}{}", image("1200x800"), image("100x400"))),
        (LAOWANG, image("640x480").to_string()),
        (XIAOYU, "<p>span 也是预览 <span class=\"message_inline_image\"><a href=\"/user_uploads/1/ef/大图.png\"><img src=\"/user_uploads/1/ef/大图.png\"></a></span></p>".to_owned()),
        (AJIE, "<p><a href=\"/user_uploads/1/cd/周报.txt\">周报.txt</a></p>".to_owned()),
        (AJIE, "<p>文字 <a href=\"/user_uploads/1/cd/周报.txt\">嵌在文字中的文件</a> 之后</p><p><a href=\"#narrow/channel/1-dev/topic/压力测试/near/1100\">内部链接：定位消息</a> <a href=\"#narrow/dm/2-阿杰\">私聊链接</a></p>".to_owned()),
        (XIAOYU, "<p><span class=\"highlight_text_inserted\">新增</span><span class=\"highlight_text_deleted\">删除</span> <time datetime=\"2026-01-01T00:00:00Z\">2026-01-01</time></p>".to_owned()),
        (AJIE, "<p>/me 挥了挥手</p>".to_owned()),
    ];
    for (sender, html) in samples {
        at += 60;
        let id = s.insert(sender, dev("渲染样例"), "占位", at);
        if let Some(m) = s.messages.get_mut(&id) {
            m.html = html;
        }
        mark_read(s, id);
    }
    at += 60;
    let id = s.insert(ME_ID, dev("渲染样例"), "裸图片", at);
    if let Some(m) = s.messages.get_mut(&id) {
        let bare = "<span class=\"highlight_text_inserted\"><img src=\"/user_uploads/1/ef/大图.png\" data-original-dimensions=\"480x240\" onerror=\"window.__xss=1\" onload=\"window.__xss=1\"></span>";
        m.html = format!(
            "<p>裸图片 {bare} 非法地址 <img src=\"javascript:window.__xss=1\" onerror=\"window.__xss=1\"></p>"
        );
        m.history.push(serde_json::json!({
            "topic": m.topic, "content": "旧内容", "rendered_content": "<p>旧内容</p>",
            "user_id": ME_ID, "timestamp": at - 30,
        }));
        m.last_edit = Some(at);
    }
    mark_read(s, id);
    at += 60;
    let id = s.insert(ME_ID, dev("渲染样例"), "/poll 投票样例\n选项一\n选项二", at);
    mark_read(s, id);
    at += 60;
    let id = s.insert(AJIE, dev("渲染样例"), "占位", at);
    if let Some(m) = s.messages.get_mut(&id) {
        m.html = String::new();
        m.submessages = vec![json!({
            "id": id * 10,
            "message_id": id,
            "sender_id": AJIE,
            "msg_type": "widget",
            "content": json!({
                "widget_type": "todo",
                "extra_data": {"task_list_title": "待办样例", "tasks": [{"task": "写代码", "desc": "今天"}, {"task": "写测试", "desc": ""}]},
            }).to_string(),
        })];
    }
    mark_read(s, id);
}

fn user(id: i64, email: &str, name: &str, is_bot: bool, role: i32) -> User {
    User {
        id,
        email: email.to_owned(),
        name: name.to_owned(),
        is_bot,
        role,
        is_active: true,
        avatar_version: 1,
    }
}

fn mark_read(s: &mut State, id: i64) {
    if let Some(m) = s.messages.get_mut(&id) {
        m.flags.insert("read".to_owned());
    }
}

fn star(s: &mut State, id: i64) {
    if let Some(m) = s.messages.get_mut(&id) {
        m.flags.insert("starred".to_owned());
    }
}

fn default_settings() -> Map<String, Value> {
    let value = json!({
        "enter_sends": true,
        "twenty_four_hour_time": true,
        "enable_desktop_notifications": true,
        "enable_sounds": true,
        "notification_sound": "zulip",
        "pm_content_in_desktop_notifications": true,
        "enable_stream_desktop_notifications": false,
        "enable_stream_audible_notifications": false,
        "enable_drafts_synchronization": true,
        "send_read_receipts": true,
        "send_private_typing_notifications": true,
        "send_stream_typing_notifications": true,
        "presence_enabled": true,
    });
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}
