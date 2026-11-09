//! 代码块语言补全：在 ` ``` `（或 `~~~`）开头的行末输入时，列出服务器支持的语言（Pygments 的常用别名）
//! 与 Zulip 的特殊块（quote、spoiler、math），按输入的文字搜索。

use super::format::MarkdownEdit;
use super::{LineKind, MarkdownModel};
use crate::util::{contains_ignore_case, is_blank, starts_with_ignore_case, static_regex};

static_regex!(OPEN_FENCE = r"^(?<prefix>(?: {0,3}> ?)*)(?<fence>`{3,}|~{3,})(?<lang>[^\s`~]*)$");

/// 按常用程度排列（与网页版补全的优先顺序大致相同）。
pub const ALL: &[&str] = &[
    "python",
    "javascript",
    "typescript",
    "java",
    "c",
    "cpp",
    "csharp",
    "go",
    "rust",
    "kotlin",
    "swift",
    "ruby",
    "php",
    "html",
    "css",
    "json",
    "yaml",
    "xml",
    "sql",
    "bash",
    "shell",
    "console",
    "powershell",
    "text",
    "markdown",
    "diff",
    "dockerfile",
    "makefile",
    "ini",
    "toml",
    "quote",
    "spoiler",
    "math",
    "scss",
    "sass",
    "less",
    "jsx",
    "tsx",
    "vue",
    "graphql",
    "lua",
    "perl",
    "r",
    "scala",
    "haskell",
    "elixir",
    "erlang",
    "clojure",
    "dart",
    "objective-c",
    "matlab",
    "groovy",
    "nginx",
    "vim",
    "latex",
    "protobuf",
    "cmake",
    "fsharp",
    "ocaml",
    "julia",
    "zig",
    "nim",
    "http",
    "apache",
    "batch",
    "vbnet",
    "nasm",
    "solidity",
    "hcl",
    "coffeescript",
    "d",
    "fortran",
    "pascal",
    "prolog",
    "scheme",
    "common-lisp",
    "tex",
    "properties",
    "csv",
    "js",
    "ts",
    "py",
    "sh",
    "cs",
];

/// 不是编程语言的特殊块，以及与全名重复的简写：选择语言的下拉框里不列出。
const NOT_PICKABLE: &[&str] = &["quote", "spoiler", "math", "js", "ts", "py", "sh", "cs"];

/// 选择语言的下拉框列出的语言，按常用顺序。
pub fn pickable() -> impl Iterator<Item = &'static str> {
    ALL.iter().copied().filter(|l| !NOT_PICKABLE.contains(l))
}

/// 光标位于开头围栏行（[`LineKind::FenceOpen`]）的末尾时，取出正在输入的语言：
/// 返回语言的起点和已输入的部分（可以为空）。
pub fn query_at<'a>(
    text: &'a str,
    caret: usize,
    model: &MarkdownModel,
) -> Option<(usize, &'a str)> {
    if caret > text.len() || !text.is_char_boundary(caret) {
        return None;
    }
    let line_start = text[..caret].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[caret..].find('\n').map_or(text.len(), |i| caret + i);
    if !is_blank(&text[caret..line_end]) {
        return None;
    }
    let line = &model.lines[model.clamped_line_index(caret)?];
    if line.kind != LineKind::FenceOpen || line.start != line_start {
        return None;
    }
    let lang = OPEN_FENCE
        .captures(&text[line_start..caret])?
        .name("lang")?;
    Some((
        line_start + lang.start(),
        &text[line_start + lang.start()..caret],
    ))
}

/// 按输入的文字搜索（不区分大小写）：前缀匹配在前，其次包含，各自保持常用顺序。
pub fn search(query: &str, max: usize) -> Vec<&'static str> {
    let prefix = ALL.iter().filter(|l| starts_with_ignore_case(l, query));
    let contains = ALL
        .iter()
        .filter(|l| !starts_with_ignore_case(l, query) && contains_ignore_case(l, query));
    prefix.chain(contains).copied().take(max).collect()
}

/// 选中语言：把 `[start, caret)` 换成语言；围栏还没有结束时补上空行和结束围栏，光标放在空行上，
/// 已经结束时光标移到下一行（代码块的第一行）。
pub fn accept(
    text: &str,
    caret: usize,
    start: usize,
    language: &str,
    model: &MarkdownModel,
) -> MarkdownEdit {
    let index = model.clamped_line_index(caret).unwrap_or(0);
    let line_start = model.lines.get(index).map_or(0, |l| l.start);
    let line_end = model.lines.get(index).map_or(text.len(), |l| l.end);
    let head = format!("{}{language}", &text[line_start..start]);
    let m = OPEN_FENCE.captures(&head);
    let prefix = m
        .as_ref()
        .and_then(|c| c.name("prefix"))
        .map_or("", |g| g.as_str());
    let fence = m
        .as_ref()
        .and_then(|c| c.name("fence"))
        .map_or("```", |g| g.as_str());
    let len = caret - start;
    if model
        .lines
        .iter()
        .skip(index + 1)
        .any(|l| l.kind == LineKind::FenceClose)
    {
        // 替换后本行的行尾 + 1：下一行的开头
        let next = line_end + language.len() - len + 1;
        return MarkdownEdit::new(start, len, language.to_owned(), next, next);
    }
    let at = start + language.len() + 1 + prefix.len();
    MarkdownEdit::new(
        start,
        len,
        format!("{language}\n{prefix}\n{prefix}{fence}"),
        at,
        at,
    )
}

/// 在还没有结束的开头围栏行末尾按回车：按已输入的语言（可以为空）补上空行和结束围栏，
/// 光标放在空行上。不在这样的位置时为 `None`。
pub fn complete_fence(text: &str, caret: usize, model: &MarkdownModel) -> Option<MarkdownEdit> {
    let (start, language) = query_at(text, caret, model)?;
    let index = model.clamped_line_index(caret)?;
    if model
        .lines
        .iter()
        .skip(index + 1)
        .any(|l| l.kind == LineKind::FenceClose)
    {
        return None;
    }
    Some(accept(text, caret, start, language, model))
}

/// 把第 `open_line` 行（开头围栏）的语言换成 `language`，光标放在代码块的第一行开头。
/// 这一行不是开头围栏时为 `None`。
pub fn set_language(
    text: &str,
    model: &MarkdownModel,
    open_line: usize,
    language: &str,
) -> Option<MarkdownEdit> {
    let line = model.lines.get(open_line)?;
    if line.kind != LineKind::FenceOpen {
        return None;
    }
    let fence = FENCE_MARK.find(&text[line.start..line.end])?;
    let start = line.start + fence.end();
    let len = line.end - start;
    let next = (line.end + language.len() - len + 1).min(text.len() + language.len() - len);
    Some(MarkdownEdit::new(
        start,
        len,
        language.to_owned(),
        next,
        next,
    ))
}

static_regex!(FENCE_MARK = r"^(?: {0,3}> ?)*(?:`{3,}|~{3,})");

#[cfg(test)]
mod tests {
    use super::super::{NoResolver, parse};
    use super::*;

    fn model(text: &str) -> MarkdownModel {
        parse(text, &NoResolver)
    }

    #[test]
    fn query_at_fence_line_end() {
        for (text, start, query) in [("```", 3, ""), ("```py", 3, "py"), ("x\n```ru", 5, "ru")] {
            assert_eq!(
                query_at(text, text.len(), &model(text)),
                Some((start, query)),
                "{text}"
            );
        }
    }

    #[test]
    fn query_not_on_fence_line() {
        let text = "abc";
        assert_eq!(query_at(text, 3, &model(text)), None);
        let text = "```py\ncode";
        assert_eq!(query_at(text, text.len(), &model(text)), None);
        assert_eq!(query_at("", 0, &MarkdownModel::default()), None);
    }

    #[test]
    fn search_prefix_before_contains() {
        assert_eq!(search("PY", 2), ["python", "py"]);
        assert_eq!(
            search("script", 9),
            ["javascript", "typescript", "coffeescript"]
        );
        assert_eq!(search("", 2), ["python", "javascript"]);
        assert!(search("zzzz", 5).is_empty());
    }

    #[test]
    fn accept_adds_closing_fence() {
        let text = "> ```p";
        let edit = accept(text, text.len(), 5, "python", &model(text));
        assert_eq!(edit.apply(text), "> ```python\n> \n> ```");
        assert_eq!(edit.selection_start, "> ```python\n> ".len());
    }

    #[test]
    fn complete_fence_keeps_typed_language() {
        for (text, done, caret) in [
            ("```", "```\n\n```", 4),
            ("x\n```foo", "x\n```foo\n\n```", 9),
            ("~~~", "~~~\n\n~~~", 4),
        ] {
            let edit = complete_fence(text, text.len(), &model(text)).expect(text);
            assert_eq!(edit.apply(text), done);
            assert_eq!(edit.selection_start, caret, "{text}");
        }
    }

    #[test]
    fn complete_fence_only_on_unclosed_fence_end() {
        let text = "```\ncode\n```";
        assert_eq!(complete_fence(text, 3, &model(text)), None);
        let text = "abc";
        assert_eq!(complete_fence(text, 3, &model(text)), None);
    }

    #[test]
    fn set_language_replaces_info() {
        let text = "```py title\ncode\n```";
        let edit = set_language(text, &model(text), 0, "rust").unwrap();
        assert_eq!(edit.apply(text), "```rust\ncode\n```");
        assert_eq!(edit.selection_start, "```rust\n".len());
        let text = "```\ncode\n```";
        assert_eq!(
            set_language(text, &model(text), 0, "c")
                .unwrap()
                .apply(text),
            "```c\ncode\n```"
        );
        assert_eq!(set_language(text, &model(text), 1, "c"), None);
    }

    #[test]
    fn pickable_skips_special_blocks_and_aliases() {
        let all: Vec<&str> = pickable().collect();
        assert_eq!(all[0], "python");
        assert!(!all.contains(&"math") && !all.contains(&"py"));
    }

    #[test]
    fn accept_moves_to_next_line_when_closed() {
        let text = "```p\ncode\n```";
        let edit = accept(text, 4, 3, "python", &model(text));
        assert_eq!(edit.apply(text), "```python\ncode\n```");
        assert_eq!(edit.selection_start, "```python\n".len());
    }
}
