//! plugin.json 的 launch 白名单。三种写法：
//! - 协议 `todo:`，匹配该协议的任意 URI。http、https、file 等不能这样写。
//! - 网址 `https://google.com/maps`：协议、主机、端口相同且路径在其下；主机可以写成 `*.google.com`（含 google.com 本身）。
//! - 程序：完整路径只匹配该路径；只写文件名只匹配同样只写文件名的目标。

use std::path::{Path, absolute};

use url::Url;

const FORBIDDEN_SCHEMES: [&str; 6] = ["http", "https", "file", "javascript", "vbscript", "data"];

#[derive(Clone, Debug, PartialEq, Eq)]
enum Rule {
    Scheme(String),
    Url {
        scheme: String,
        host: String,
        wildcard: bool,
        port: Option<u16>,
        path: String,
    },
    Program(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchRule {
    /// plugin.json 中的原文（开启确认时显示）。
    pub pattern: String,
    rule: Rule,
}

impl LaunchRule {
    /// 解析一项；格式错误时返回原因。
    pub fn parse(pattern: &str) -> Result<Self, String> {
        let pattern = pattern.trim();
        let rule = |rule| {
            Ok(Self {
                pattern: pattern.to_owned(),
                rule,
            })
        };
        if let Some(scheme) = scheme_only(pattern) {
            if FORBIDDEN_SCHEMES
                .iter()
                .any(|s| s.eq_ignore_ascii_case(scheme))
            {
                return Err(format!("不能把整个 {scheme} 协议加入白名单：{pattern}"));
            }
            return rule(Rule::Scheme(scheme.to_owned()));
        }

        let lower = pattern.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            let wildcard = pattern.contains("://*.");
            let parsed = Url::parse(&pattern.replacen("://*.", "://", 1)).ok();
            let Some(url) = parsed.filter(|u| {
                u.host_str().is_some_and(|h| !h.contains('*'))
                    && u.username().is_empty()
                    && u.password().is_none()
            }) else {
                return Err(format!("网址格式错误：{pattern}"));
            };
            return rule(Rule::Url {
                scheme: url.scheme().to_owned(),
                host: url.host_str().unwrap_or_default().to_owned(),
                wildcard,
                port: url.port_or_known_default(),
                path: url.path().to_owned(),
            });
        }

        if is_fully_qualified(pattern) {
            return rule(Rule::Program(full_path(pattern)));
        }
        if !pattern.is_empty() && !pattern.contains(INVALID_FILE_NAME) {
            return rule(Rule::Program(pattern.to_owned()));
        }
        Err(format!("无法识别的白名单项：{pattern}"))
    }

    pub fn matches(&self, target: &str) -> bool {
        let target = target.trim();
        if target.is_empty() {
            return false;
        }
        let uri = uri_of(target);
        match &self.rule {
            Rule::Scheme(scheme) => uri.is_some_and(|u| u.scheme().eq_ignore_ascii_case(scheme)),
            Rule::Url {
                scheme,
                host,
                wildcard,
                port,
                path,
            } => {
                let Some(uri) = uri else {
                    return false;
                };
                if !uri.scheme().eq_ignore_ascii_case(scheme)
                    || uri.port_or_known_default() != *port
                    || !uri.username().is_empty()
                    || uri.password().is_some()
                {
                    return false;
                }
                let target_host = uri.host_str().unwrap_or_default().to_ascii_lowercase();
                let host = host.to_ascii_lowercase();
                let host_matches = target_host == host
                    || (*wildcard && target_host.ends_with(&format!(".{host}")));
                host_matches && path_matches(path, uri.path())
            }
            Rule::Program(program) => {
                if uri.is_some() || target.to_ascii_lowercase().starts_with("file:") {
                    return false;
                }
                if is_fully_qualified(program) {
                    is_fully_qualified(target) && full_path(target).eq_ignore_ascii_case(program)
                } else {
                    target.eq_ignore_ascii_case(program)
                }
            }
        }
    }
}

const INVALID_FILE_NAME: [char; 9] = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

// `xx:`，协议至少两个字符（`C:` 是盘符）。
fn scheme_only(pattern: &str) -> Option<&str> {
    let scheme = pattern.strip_suffix(':')?;
    let mut chars = scheme.chars();
    let valid = scheme.len() >= 2
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
    valid.then_some(scheme)
}

/// 目标是 URI（协议至少两个字符，`C:\…` 是程序路径）。file: 不算 URI，也不能作为程序打开。
pub fn uri_of(target: &str) -> Option<Url> {
    Url::parse(target)
        .ok()
        .filter(|u| u.scheme().len() > 1 && u.scheme() != "file")
}

fn is_fully_qualified(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/'))
}

fn full_path(path: &str) -> String {
    absolute(Path::new(path))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_owned())
}

// “/maps”匹配“/maps”和“/maps/…”，不匹配“/mapsx”。
fn path_matches(rule: &str, path: &str) -> bool {
    if rule.is_empty() || rule == "/" {
        return true;
    }
    let Some(rest) = path.strip_prefix(rule) else {
        return false;
    };
    rule.ends_with('/') || rest.is_empty() || rest.starts_with('/')
}

/// 按钮上显示的完整目标（程序带参数）。
pub fn describe(target: &str, args: &str) -> String {
    if args.is_empty() {
        target.to_owned()
    } else {
        format!("{target} {args}")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("目标不合法")]
    InvalidTarget,
    #[error("无法打开 {0}（{1}）")]
    Failed(String, isize),
}

/// 打开 URI（交给系统，参数忽略）或运行程序。
pub fn launch(target: &str, args: &str) -> Result<(), LaunchError> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, PCWSTR, w};

    let target = target.trim();
    if target.is_empty() || target.to_ascii_lowercase().starts_with("file:") {
        return Err(LaunchError::InvalidTarget);
    }
    let file = HSTRING::from(target);
    let params = (!args.is_empty() && uri_of(target).is_none()).then(|| HSTRING::from(args));
    let params_ptr = params
        .as_ref()
        .map_or(PCWSTR::null(), |p| PCWSTR(p.as_ptr()));
    // SAFETY: 字符串在调用期间有效。
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &file,
            params_ptr,
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // 返回值大于 32 表示成功。
    if result.0 as isize > 32 {
        Ok(())
    } else {
        Err(LaunchError::Failed(target.to_owned(), result.0 as isize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(pattern: &str) -> LaunchRule {
        LaunchRule::parse(pattern).expect(pattern)
    }

    #[test]
    fn scheme_rule() {
        let todo = rule("todo:");
        assert!(todo.matches("todo:add?x=1"));
        assert!(!todo.matches("other:add"));
        assert!(!todo.matches("notepad.exe"));
        assert!(LaunchRule::parse("https:").is_err());
        assert!(LaunchRule::parse("file:").is_err());
        assert!(LaunchRule::parse("JavaScript:").is_err());
    }

    #[test]
    fn url_rule() {
        let maps = rule("https://google.com/maps");
        assert!(maps.matches("https://google.com/maps"));
        assert!(maps.matches("https://GOOGLE.com/maps/x"));
        assert!(!maps.matches("https://google.com/mapsx"));
        assert!(!maps.matches("http://google.com/maps"));
        assert!(!maps.matches("https://evil.google.com/maps"));
        assert!(!maps.matches("https://a@google.com/maps"));
        assert!(!maps.matches("https://google.com:8443/maps"));

        let wild = rule("https://*.google.com");
        assert!(wild.matches("https://google.com/x"));
        assert!(wild.matches("https://a.google.com/x"));
        assert!(!wild.matches("https://notgoogle.com/"));
        assert!(LaunchRule::parse("https://user@google.com").is_err());
    }

    #[test]
    fn program_rule() {
        let full = rule(r"C:\Tools\todo.exe");
        assert!(full.matches(r"c:\tools\TODO.exe"));
        assert!(!full.matches("todo.exe"));
        assert!(!full.matches(r"file:///C:\Tools\todo.exe"));

        let name = rule("notepad.exe");
        assert!(name.matches("NOTEPAD.EXE"));
        assert!(!name.matches(r"C:\Windows\notepad.exe"));
        assert!(LaunchRule::parse("a/b.exe").is_err());
        assert!(LaunchRule::parse("").is_err());
    }
}
