//! Code block language completion: when typing at the end of a line that starts with ` ``` ` (or `~~~`), lists the languages supported by the server (common Pygments aliases)
//! and Zulip's special blocks (quote, spoiler, math), searched by the typed text.

use super::format::MarkdownEdit;
use super::{LineKind, MarkdownModel};
use crate::util::{contains_ignore_case, is_blank, starts_with_ignore_case, static_regex};

static_regex!(OPEN_FENCE = r"^(?<prefix>(?: {0,3}> ?)*)(?<fence>`{3,}|~{3,})(?<lang>[^\s`~]*)$");

/// Ordered by popularity (roughly the same priority order as completion in the web version).
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

/// Special blocks that are not programming languages, and short aliases that duplicate the full name: not listed in the language-selection dropdown.
const NOT_PICKABLE: &[&str] = &["quote", "spoiler", "math", "js", "ts", "py", "sh", "cs"];

/// Languages listed in the language-selection dropdown, in order of popularity.
pub fn pickable() -> impl Iterator<Item = &'static str> {
    ALL.iter().copied().filter(|l| !NOT_PICKABLE.contains(l))
}

/// When the cursor is at the end of an opening fence line ([`LineKind::FenceOpen`]), extracts the language being typed:
/// returns the start of the language and the part already typed (may be empty).
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

/// Searches by the typed text (case-insensitive): prefix matches first, then containing matches, each keeping the popularity order.
pub fn search(query: &str, max: usize) -> Vec<&'static str> {
    let prefix = ALL.iter().filter(|l| starts_with_ignore_case(l, query));
    let contains = ALL
        .iter()
        .filter(|l| !starts_with_ignore_case(l, query) && contains_ignore_case(l, query));
    prefix.chain(contains).copied().take(max).collect()
}

/// Selects a language: replaces `[start, caret)` with the language; if the fence is not closed yet, adds an empty line and a closing fence with the cursor on the empty line,
/// and if it is already closed, moves the cursor to the next line (the first line of the code block).
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
        // End of this line after the replacement + 1: the start of the next line
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

/// Pressing Enter at the end of an unclosed opening fence line: adds an empty line and a closing fence according to the typed language (may be empty),
/// with the cursor on the empty line. `None` when not at such a position.
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

/// Replaces the language of line `open_line` (the opening fence) with `language`, with the cursor at the start of the first line of the code block.
/// `None` if this line is not an opening fence.
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
