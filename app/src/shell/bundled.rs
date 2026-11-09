//! Static resources compiled into the program (formula fonts, code language icons), served to the
//! WebView through a custom protocol.

use dioxus::desktop::wry::RequestAsyncResponder;
use dioxus::desktop::wry::http::{Request, Response};
use std::borrow::Cow;

/// Custom protocol name (in WebView2 the address is `http://zulip-asset.localhost/...`).
pub const SCHEME: &str = "zulip-asset";

/// Formula styles; font addresses have been changed to this protocol.
pub const KATEX_CSS: &str = include_str!("../../assets/katex/katex.css");

macro_rules! katex_fonts {
    ($($name:literal),* $(,)?) => {
        &[$((
            concat!("/katex/", $name, ".woff2"),
            include_bytes!(concat!("../../assets/katex/fonts/", $name, ".woff2")),
        )),*]
    };
}

macro_rules! language_icons {
    ($($name:literal),* $(,)?) => {
        &[$((
            concat!("/langs/", $name, ".svg"),
            include_bytes!(concat!("../../assets/langs/", $name, ".svg")),
        )),*]
    };
}

/// Code language -> icon (Material Icon Theme, MIT license). Languages not in the table use
/// [`DEFAULT_ICON`].
const LANGUAGE_ICONS: &[(&str, &str)] = &[
    ("python", "python"),
    ("py", "python"),
    ("javascript", "javascript"),
    ("js", "javascript"),
    ("typescript", "typescript"),
    ("ts", "typescript"),
    ("java", "java"),
    ("c", "c"),
    ("cpp", "cpp"),
    ("csharp", "csharp"),
    ("cs", "csharp"),
    ("go", "go"),
    ("rust", "rust"),
    ("kotlin", "kotlin"),
    ("swift", "swift"),
    ("ruby", "ruby"),
    ("php", "php"),
    ("html", "html"),
    ("css", "css"),
    ("json", "json"),
    ("yaml", "yaml"),
    ("xml", "xml"),
    ("sql", "database"),
    ("bash", "console"),
    ("shell", "console"),
    ("sh", "console"),
    ("console", "console"),
    ("batch", "console"),
    ("powershell", "powershell"),
    ("markdown", "markdown"),
    ("diff", "diff"),
    ("dockerfile", "docker"),
    ("makefile", "makefile"),
    ("ini", "settings"),
    ("properties", "settings"),
    ("apache", "settings"),
    ("toml", "toml"),
    ("scss", "sass"),
    ("sass", "sass"),
    ("less", "less"),
    ("jsx", "react"),
    ("tsx", "react_ts"),
    ("vue", "vue"),
    ("graphql", "graphql"),
    ("lua", "lua"),
    ("perl", "perl"),
    ("r", "r"),
    ("scala", "scala"),
    ("haskell", "haskell"),
    ("elixir", "elixir"),
    ("erlang", "erlang"),
    ("clojure", "clojure"),
    ("dart", "dart"),
    ("objective-c", "objective-c"),
    ("matlab", "matlab"),
    ("groovy", "groovy"),
    ("nginx", "nginx"),
    ("vim", "vim"),
    ("latex", "tex"),
    ("tex", "tex"),
    ("protobuf", "proto"),
    ("cmake", "cmake"),
    ("fsharp", "fsharp"),
    ("ocaml", "ocaml"),
    ("julia", "julia"),
    ("zig", "zig"),
    ("nim", "nim"),
    ("http", "http"),
    ("vbnet", "visualstudio"),
    ("nasm", "assembly"),
    ("solidity", "solidity"),
    ("hcl", "hcl"),
    ("coffeescript", "coffee"),
    ("d", "d"),
    ("fortran", "fortran"),
    ("pascal", "pascal"),
    ("prolog", "prolog"),
    ("scheme", "scheme"),
    ("common-lisp", "lisp"),
    ("csv", "table"),
];

/// Languages without a dedicated icon (and plain text).
const DEFAULT_ICON: &str = "document";

/// Address of a code language icon (case-insensitive).
pub fn language_icon_url(language: &str) -> String {
    let language = language.to_lowercase();
    let icon = LANGUAGE_ICONS
        .iter()
        .find(|(l, _)| *l == language)
        .map_or(DEFAULT_ICON, |(_, icon)| icon);
    format!("http://{SCHEME}.localhost/langs/{icon}.svg")
}

/// Address path -> file content.
const FONTS: &[(&str, &[u8])] = katex_fonts![
    "KaTeX_AMS-Regular",
    "KaTeX_Caligraphic-Bold",
    "KaTeX_Caligraphic-Regular",
    "KaTeX_Fraktur-Bold",
    "KaTeX_Fraktur-Regular",
    "KaTeX_Main-Bold",
    "KaTeX_Main-BoldItalic",
    "KaTeX_Main-Italic",
    "KaTeX_Main-Regular",
    "KaTeX_Math-BoldItalic",
    "KaTeX_Math-Italic",
    "KaTeX_SansSerif-Bold",
    "KaTeX_SansSerif-Italic",
    "KaTeX_SansSerif-Regular",
    "KaTeX_Script-Regular",
    "KaTeX_Size1-Regular",
    "KaTeX_Size2-Regular",
    "KaTeX_Size3-Regular",
    "KaTeX_Size4-Regular",
    "KaTeX_Typewriter-Regular",
];

const ICONS: &[(&str, &[u8])] = language_icons![
    "assembly",
    "c",
    "clojure",
    "cmake",
    "coffee",
    "console",
    "cpp",
    "csharp",
    "css",
    "d",
    "dart",
    "database",
    "diff",
    "docker",
    "document",
    "elixir",
    "erlang",
    "fortran",
    "fsharp",
    "go",
    "graphql",
    "groovy",
    "haskell",
    "hcl",
    "html",
    "http",
    "java",
    "javascript",
    "json",
    "julia",
    "kotlin",
    "less",
    "lisp",
    "lua",
    "makefile",
    "markdown",
    "matlab",
    "nginx",
    "nim",
    "objective-c",
    "ocaml",
    "pascal",
    "perl",
    "php",
    "powershell",
    "prolog",
    "proto",
    "python",
    "r",
    "react",
    "react_ts",
    "ruby",
    "rust",
    "sass",
    "scala",
    "scheme",
    "settings",
    "solidity",
    "swift",
    "table",
    "tex",
    "toml",
    "typescript",
    "vim",
    "visualstudio",
    "vue",
    "xml",
    "yaml",
    "zig",
];

/// Address path -> file content and type.
fn find(path: &str) -> Option<(&'static [u8], &'static str)> {
    let lookup = |files: &'static [(&str, &[u8])]| {
        files
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, bytes)| *bytes)
    };
    lookup(FONTS)
        .map(|bytes| (bytes, "font/woff2"))
        .or_else(|| lookup(ICONS).map(|bytes| (bytes, "image/svg+xml")))
}

/// Protocol handler.
pub fn handle(request: Request<Vec<u8>>, responder: RequestAsyncResponder) {
    let response = match find(request.uri().path()) {
        Some((bytes, content_type)) => Response::builder()
            .header("Content-Type", content_type)
            .header("Access-Control-Allow-Origin", "*")
            .header("Cache-Control", "max-age=31536000, immutable")
            .body(Cow::Borrowed(bytes)),
        None => Response::builder()
            .status(404)
            .body(Cow::Borrowed(&b""[..])),
    };
    if let Ok(response) = response {
        responder.respond(response);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_font_in_the_stylesheet_is_bundled() {
        let prefix = format!("url(http://{SCHEME}.localhost");
        let urls: Vec<&str> = KATEX_CSS
            .split(prefix.as_str())
            .skip(1)
            .map(|rest| &rest[..rest.find(')').unwrap()])
            .collect();
        assert_eq!(urls.len(), FONTS.len());
        for url in urls {
            assert!(find(url).is_some(), "{url}");
        }
    }

    #[test]
    fn every_language_icon_is_bundled() {
        let prefix = format!("http://{SCHEME}.localhost");
        for (language, _) in LANGUAGE_ICONS {
            let url = language_icon_url(language);
            assert!(find(&url[prefix.len()..]).is_some(), "{language}");
        }
        assert!(language_icon_url("Python").ends_with("/python.svg"));
        assert!(language_icon_url("cobol").ends_with("/document.svg"));
        assert!(find("/langs/document.svg").is_some());
    }
}
