//! 够用的 Markdown → Zulip 风格 HTML：段落、标题、粗体、斜体、行内代码、代码块、引用、列表、表格、
//! 提及、频道链接和上传文件。

use std::sync::LazyLock;

use regex::{Captures, Regex};

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("正则表达式有效")
}

static BOLD: LazyLock<Regex> = LazyLock::new(|| regex(r"\*\*([^*]+)\*\*"));
static ITALIC: LazyLock<Regex> = LazyLock::new(|| regex(r"\*([^*]+)\*"));
static LINK: LazyLock<Regex> = LazyLock::new(|| regex(r"\[([^\]]*)\]\(([^)\s]+)\)"));
static MENTION: LazyLock<Regex> = LazyLock::new(|| regex(r"@(_?)\*\*([^*|]+)(?:\|(\d+))?\*\*"));
static CHANNEL: LazyLock<Regex> = LazyLock::new(|| regex(r"#\*\*([^*]+?)(?:&gt;([^*]+))?\*\*"));
static ORDERED: LazyLock<Regex> = LazyLock::new(|| regex(r"^\d+\.\s+(.*)$"));

/// 把提及里的名字解析为用户 id：`@**名字**` 这种不带 id 的写法用。
pub type NameLookup<'a> = &'a dyn Fn(&str) -> Option<i64>;

pub fn render(source: &str, lookup: NameLookup) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let mut out = String::new();
    render_block(&lines, lookup, &mut out);
    out
}

fn render_block(lines: &[&str], lookup: NameLookup, out: &mut String) {
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if trimmed.is_empty() {
            i += 1;
        } else if let Some(fence_len) = fence_of(trimmed) {
            let lang = trimmed[fence_len..].trim();
            let close = "`".repeat(fence_len);
            let end = (i + 1..lines.len())
                .find(|&j| lines[j].trim_end() == close)
                .unwrap_or(lines.len());
            render_fence(lang, &lines[i + 1..end], lookup, out);
            i = end + 1;
        } else if let Some(level) = heading_of(trimmed) {
            let text = inline(trimmed[level..].trim(), lookup);
            out.push_str(&format!("<h{level}>{text}</h{level}>\n"));
            i += 1;
        } else if trimmed.starts_with('>') {
            let end = run_end(lines, i, |l| l.trim_start().starts_with('>'));
            let body: Vec<&str> = lines[i..end]
                .iter()
                .map(|l| l.trim_start()[1..].trim_start())
                .collect();
            out.push_str("<blockquote>\n");
            render_block(&body, lookup, out);
            out.push_str("</blockquote>\n");
            i = end;
        } else if trimmed.starts_with('|') && lines.get(i + 1).is_some_and(|l| is_table_rule(l)) {
            let end = run_end(lines, i, |l| l.trim_start().starts_with('|'));
            render_table(&lines[i..end], lookup, out);
            i = end;
        } else if bullet_of(trimmed).is_some() {
            let end = run_end(lines, i, |l| bullet_of(l.trim_start()).is_some());
            render_list("ul", &lines[i..end], lookup, out);
            i = end;
        } else if ORDERED.is_match(trimmed) {
            let end = run_end(lines, i, |l| ORDERED.is_match(l.trim_start()));
            render_list("ol", &lines[i..end], lookup, out);
            i = end;
        } else {
            let end = run_end(lines, i, |l| {
                let t = l.trim_start();
                !t.is_empty() && fence_of(t).is_none() && !t.starts_with('>')
            });
            render_paragraph(&lines[i..end], lookup, out);
            i = end;
        }
    }
}

fn render_fence(lang: &str, body: &[&str], lookup: NameLookup, out: &mut String) {
    if lang.eq_ignore_ascii_case("quote") {
        out.push_str("<blockquote>\n");
        render_block(body, lookup, out);
        out.push_str("</blockquote>\n");
        return;
    }
    let attr = if lang.is_empty() {
        String::new()
    } else {
        format!(" data-code-language=\"{}\"", escape(lang))
    };
    out.push_str(&format!(
        "<div class=\"codehilite\"{attr}><pre><span></span><code>{}\n</code></pre></div>\n",
        escape(&body.join("\n"))
    ));
}

fn render_paragraph(lines: &[&str], lookup: NameLookup, out: &mut String) {
    let text = lines
        .iter()
        .map(|l| inline(l.trim(), lookup))
        .collect::<Vec<_>>()
        .join("<br>\n");
    out.push_str(&format!("<p>{text}</p>\n"));
    for (href, name) in lines.iter().flat_map(|l| image_links(l)) {
        out.push_str(&format!(
            "<div class=\"message_inline_image\"><a href=\"{href}\" title=\"{name}\"><img src=\"{href}\"></a></div>\n"
        ));
    }
}

fn image_links(line: &str) -> Vec<(String, String)> {
    LINK.captures_iter(line)
        .filter(|c| c[2].starts_with("/user_uploads/") && is_image(&c[2]))
        .map(|c| (escape(&c[2]), escape(&c[1])))
        .collect()
}

pub fn is_image(path: &str) -> bool {
    mime_guess::from_path(path)
        .first()
        .is_some_and(|m| m.type_() == "image")
}

fn render_list(tag: &str, lines: &[&str], lookup: NameLookup, out: &mut String) {
    out.push_str(&format!("<{tag}>\n"));
    for line in lines {
        let t = line.trim_start();
        let text = match bullet_of(t) {
            Some(text) => text.to_owned(),
            None => ORDERED.replace(t, "$1").into_owned(),
        };
        out.push_str(&format!("<li>{}</li>\n", inline(&text, lookup)));
    }
    out.push_str(&format!("</{tag}>\n"));
}

fn render_table(lines: &[&str], lookup: NameLookup, out: &mut String) {
    let cells = |line: &str| -> Vec<String> {
        line.trim()
            .trim_matches('|')
            .split('|')
            .map(|c| inline(c.trim(), lookup))
            .collect()
    };
    out.push_str("<table>\n<thead>\n<tr>\n");
    for cell in cells(lines[0]) {
        out.push_str(&format!("<th>{cell}</th>\n"));
    }
    out.push_str("</tr>\n</thead>\n<tbody>\n");
    for line in &lines[2..] {
        out.push_str("<tr>\n");
        for cell in cells(line) {
            out.push_str(&format!("<td>{cell}</td>\n"));
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</tbody>\n</table>\n");
}

fn run_end(lines: &[&str], start: usize, keep: impl Fn(&str) -> bool) -> usize {
    (start + 1..lines.len())
        .find(|&j| !keep(lines[j]))
        .unwrap_or(lines.len())
}

fn fence_of(line: &str) -> Option<usize> {
    let count = line.bytes().take_while(|&b| b == b'`').count();
    (count >= 3).then_some(count)
}

fn heading_of(line: &str) -> Option<usize> {
    let count = line.bytes().take_while(|&b| b == b'#').count();
    ((1..=6).contains(&count) && line[count..].starts_with(' ')).then_some(count)
}

fn bullet_of(line: &str) -> Option<&str> {
    line.strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("+ "))
}

fn is_table_rule(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.contains('-') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

fn inline(text: &str, lookup: NameLookup) -> String {
    text.split('`')
        .enumerate()
        .map(|(i, part)| {
            if i % 2 == 1 {
                format!("<code>{}</code>", escape(part))
            } else {
                styled(part, lookup)
            }
        })
        .collect()
}

fn styled(text: &str, lookup: NameLookup) -> String {
    let escaped = escape(text);
    let mentions = MENTION.replace_all(&escaped, |c: &Captures| mention(c, lookup));
    let channels = CHANNEL.replace_all(&mentions, |c: &Captures| match c.get(2) {
        Some(topic) => format!(
            "<a class=\"stream-topic\">#{} &gt; {}</a>",
            &c[1],
            topic.as_str()
        ),
        None => format!("<a class=\"stream\">#{}</a>", &c[1]),
    });
    let links = LINK.replace_all(&channels, "<a href=\"$2\">$1</a>");
    let bold = BOLD.replace_all(&links, "<strong>$1</strong>");
    ITALIC.replace_all(&bold, "<em>$1</em>").into_owned()
}

fn mention(c: &Captures, lookup: NameLookup) -> String {
    let silent = !c[1].is_empty();
    let name = &c[2];
    let shown = if silent {
        name.to_owned()
    } else {
        format!("@{name}")
    };
    let silent_class = if silent { " silent" } else { "" };
    match name {
        "all" | "everyone" | "channel" | "stream" => format!(
            "<span class=\"user-mention channel-wildcard-mention{silent_class}\" data-user-id=\"*\">{shown}</span>"
        ),
        "topic" => format!("<span class=\"topic-mention{silent_class}\">{shown}</span>"),
        _ => {
            let id = c
                .get(3)
                .and_then(|m| m.as_str().parse().ok())
                .or_else(|| lookup(name));
            match id {
                Some(id) => format!(
                    "<span class=\"user-mention{silent_class}\" data-user-id=\"{id}\">{shown}</span>"
                ),
                None => c[0].to_owned(),
            }
        }
    }
}

pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> Option<i64> {
        None
    }

    #[test]
    fn inline_formats() {
        assert_eq!(
            render("**粗** *斜* `a<b` <x>", &none),
            "<p><strong>粗</strong> <em>斜</em> <code>a&lt;b</code> &lt;x&gt;</p>\n"
        );
    }

    #[test]
    fn mentions_have_zulip_classes() {
        let html = render("@**张三|5** @_**李四|6** @**all** @**王五**", &|_| {
            Some(9)
        });
        assert!(html.contains("<span class=\"user-mention\" data-user-id=\"5\">@张三</span>"));
        assert!(
            html.contains("<span class=\"user-mention silent\" data-user-id=\"6\">李四</span>")
        );
        assert!(html.contains("channel-wildcard-mention\" data-user-id=\"*\">@all"));
        assert!(html.contains("data-user-id=\"9\">@王五"));
    }

    #[test]
    fn blocks() {
        let html = render(
            "```py\nx<1\n```\n```quote\n你好\n```\n> 引\n- a\n- b\n1. c",
            &none,
        );
        assert!(html.contains("data-code-language=\"py\"><pre><span></span><code>x&lt;1\n"));
        assert!(html.contains("<blockquote>\n<p>你好</p>\n</blockquote>"));
        assert!(html.contains("<blockquote>\n<p>引</p>\n</blockquote>"));
        assert!(html.contains("<ul>\n<li>a</li>\n<li>b</li>\n</ul>"));
        assert!(html.contains("<ol>\n<li>c</li>\n</ol>"));
    }

    #[test]
    fn table_and_image() {
        let html = render(
            "| 名称 | 值 |\n|---|---|\n| a | 1 |\n\n[图.png](/user_uploads/1/a/图.png)",
            &none,
        );
        assert!(html.contains("<th>名称</th>"));
        assert!(html.contains("<td>1</td>"));
        assert!(html.contains("<a href=\"/user_uploads/1/a/图.png\">图.png</a>"));
        assert!(html.contains("class=\"message_inline_image\""));
    }
}
