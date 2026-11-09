//! Restricted Markdown for attached content: paragraphs, line breaks, lists, headings, code blocks,
//! bold, italic, inline code, http(s) links. Everything else (including HTML) is escaped, so the
//! output can be displayed directly as HTML.

use zulip_core::html::encode;

/// Converts plain text provided by a plugin to HTML: blank lines split paragraphs, a single newline
/// becomes `<br>`.
pub fn plain_text_html(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n");
    normalized
        .split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| format!("<p>{}</p>", encode(p).replace('\n', "<br>")))
        .collect()
}

/// Converts restricted Markdown to safe HTML.
pub fn markdown_html(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n");
    let mut out = String::new();
    let mut paragraph: Vec<&str> = Vec::new();
    let mut list: Vec<&str> = Vec::new();
    let mut code: Option<Vec<&str>> = None;
    for line in normalized.lines() {
        if let Some(lines) = &mut code {
            if line.trim_start().starts_with("```") {
                out.push_str("<pre><code>");
                out.push_str(&encode(&lines.join("\n")));
                out.push_str("</code></pre>");
                code = None;
            } else {
                lines.push(line);
            }
            continue;
        }
        let trimmed = line.trim();
        let item = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "));
        if item.is_none() {
            flush_list(&mut list, &mut out);
        }
        if trimmed.starts_with("```") {
            flush_paragraph(&mut paragraph, &mut out);
            code = Some(Vec::new());
        } else if let Some(item) = item {
            flush_paragraph(&mut paragraph, &mut out);
            list.push(item);
        } else if trimmed.is_empty() {
            flush_paragraph(&mut paragraph, &mut out);
        } else if let Some(title) = heading(trimmed) {
            flush_paragraph(&mut paragraph, &mut out);
            out.push_str("<p><strong>");
            inline(title, &mut out);
            out.push_str("</strong></p>");
        } else {
            paragraph.push(trimmed);
        }
    }
    flush_paragraph(&mut paragraph, &mut out);
    flush_list(&mut list, &mut out);
    if let Some(lines) = code {
        out.push_str("<pre><code>");
        out.push_str(&encode(&lines.join("\n")));
        out.push_str("</code></pre>");
    }
    out
}

fn heading(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches('#');
    let level = line.len() - rest.len();
    (1..=6).contains(&level).then(|| rest.strip_prefix(' '))?
}

fn flush_paragraph(lines: &mut Vec<&str>, out: &mut String) {
    if lines.is_empty() {
        return;
    }
    out.push_str("<p>");
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            out.push_str("<br>");
        }
        inline(line, out);
    }
    out.push_str("</p>");
    lines.clear();
}

fn flush_list(items: &mut Vec<&str>, out: &mut String) {
    if items.is_empty() {
        return;
    }
    out.push_str("<ul>");
    for item in items.iter() {
        out.push_str("<li>");
        inline(item, out);
        out.push_str("</li>");
    }
    out.push_str("</ul>");
    items.clear();
}

fn is_safe_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

// When the part between markers is non-empty, returns the content and the text after it.
fn delimited<'a>(text: &'a str, marker: &str) -> Option<(&'a str, &'a str)> {
    let rest = text.strip_prefix(marker)?;
    let end = rest.find(marker).filter(|end| *end > 0)?;
    Some((&rest[..end], &rest[end + marker.len()..]))
}

fn link(text: &str) -> Option<(&str, &str, &str)> {
    let rest = text.strip_prefix('[')?;
    let (label, rest) = rest.split_once("](")?;
    let (url, rest) = rest.split_once(')')?;
    (!label.is_empty() && !label.contains('[') && is_safe_url(url)).then_some((label, url, rest))
}

fn inline(text: &str, out: &mut String) {
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if let Some((code, after)) = delimited(rest, "`") {
            out.push_str("<code>");
            out.push_str(&encode(code));
            out.push_str("</code>");
            rest = after;
        } else if let Some((inner, after)) = delimited(rest, "**") {
            out.push_str("<strong>");
            inline(inner, out);
            out.push_str("</strong>");
            rest = after;
        } else if let Some((inner, after)) = delimited(rest, "*") {
            out.push_str("<em>");
            inline(inner, out);
            out.push_str("</em>");
            rest = after;
        } else if let Some((label, url, after)) = link(rest) {
            out.push_str("<a href=\"");
            out.push_str(&encode(url));
            out.push_str("\" rel=\"noopener noreferrer\">");
            inline(label, out);
            out.push_str("</a>");
            rest = after;
        } else {
            out.push_str(&encode(&c.to_string()));
            rest = &rest[c.len_utf8()..];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_paragraphs() {
        assert_eq!(
            plain_text_html("a<b\nc\n\n\n d"),
            "<p>a&lt;b<br>c</p><p>d</p>"
        );
    }

    #[test]
    fn renders_supported_syntax() {
        assert_eq!(
            markdown_html("**粗** *斜* `a<b`\n下一行\n\n- 一\n- [链接](https://x.test/?a=1&b=2)"),
            "<p><strong>粗</strong> <em>斜</em> <code>a&lt;b</code><br>下一行</p>\
             <ul><li>一</li><li><a href=\"https://x.test/?a=1&amp;b=2\" rel=\"noopener noreferrer\">链接</a></li></ul>"
        );
        assert_eq!(
            markdown_html("# 标题\n```\n<b>\n```"),
            "<p><strong>标题</strong></p><pre><code>&lt;b&gt;</code></pre>"
        );
        assert_eq!(
            markdown_html("```\n未闭合"),
            "<pre><code>未闭合</code></pre>"
        );
        assert_eq!(markdown_html("2 * 3 * 4"), "<p>2 <em> 3 </em> 4</p>");
        assert_eq!(markdown_html("*"), "<p>*</p>");
    }

    #[test]
    fn output_is_safe() {
        for attack in [
            "<script>alert(1)</script>",
            "<img src=x onerror=alert(1)>",
            "**<svg onload=alert(1)>**",
            "`</code><script>`",
            "```\n</pre><script>\n```",
        ] {
            let html = markdown_html(attack);
            assert!(!html.contains("<script"), "{html}");
            assert!(!html.contains("<img"), "{html}");
            assert!(!html.contains("<svg"), "{html}");
        }
        // Only http(s) links are allowed; the attribute cannot be closed.
        for url in [
            "javascript:alert(1)",
            "JAVASCRIPT:alert(1)",
            "data:text/html,x",
            "file:///c:/a",
        ] {
            assert!(!markdown_html(&format!("[点我]({url})")).contains("<a "));
        }
        let quoted = markdown_html("[x](https://a.test/\"onclick=\"alert(1))");
        assert!(!quoted.contains("\"onclick"), "{quoted}");
    }
}
