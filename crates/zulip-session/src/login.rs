//! 登录流程中的纯逻辑。

use url::Url;

use crate::error::{Result, SessionError};

/// 登录框里的服务器地址 → 规范形式（没有结尾斜杠和查询串，缺协议时补 https）。无效时为 `None`。
pub fn normalize_realm(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };
    let url = Url::parse(&with_scheme).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?;
    let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
    let path = url.path().trim_end_matches('/');
    Some(format!("{}://{host}{port}{path}", url.scheme()))
}

/// zuliprc 的内容。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Zuliprc {
    pub email: String,
    pub api_key: String,
    pub site: String,
}

/// 解析 zuliprc：`[api]` 段的 `email`、`key`、`site`，`=` 或 `:` 分隔，`#`、`;` 开头为注释。
pub fn parse_zuliprc(text: &str) -> Result<Zuliprc> {
    let mut in_api = false;
    let (mut email, mut key, mut site) = (None, None, None);
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_api = section.trim().eq_ignore_ascii_case("api");
            continue;
        }
        if !in_api {
            continue;
        }
        let Some((name, value)) = line.split_once(['=', ':']) else {
            continue;
        };
        let value = value.trim().to_owned();
        match name.trim().to_lowercase().as_str() {
            "email" => email = Some(value),
            "key" => key = Some(value),
            "site" => site = Some(value),
            _ => {}
        }
    }
    let (Some(email), Some(api_key), Some(site)) = (
        email.filter(|v| !v.is_empty()),
        key.filter(|v| !v.is_empty()),
        site.filter(|v| !v.is_empty()),
    ) else {
        return Err(SessionError::invalid(
            "zuliprc 中缺少 [api] 段的 email、key 或 site",
        ));
    };
    let site =
        normalize_realm(&site).ok_or_else(|| SessionError::invalid("zuliprc 中的 site 无效"))?;
    Ok(Zuliprc {
        email,
        api_key,
        site,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realm_normalization() {
        for (input, expected) in [
            ("chat.example.com", Some("https://chat.example.com")),
            (
                "  https://chat.example.com/  ",
                Some("https://chat.example.com"),
            ),
            ("https://localhost:8443/", Some("https://localhost:8443")),
            ("http://10.0.0.2:9991", Some("http://10.0.0.2:9991")),
            (
                "https://chat.example.com/zulip/?x=1",
                Some("https://chat.example.com/zulip"),
            ),
            ("", None),
            ("   ", None),
            ("ftp://chat.example.com", None),
            ("https://", None),
        ] {
            assert_eq!(normalize_realm(input).as_deref(), expected, "{input}");
        }
    }

    #[test]
    fn zuliprc_parses_both_separators_and_comments() {
        let parsed = parse_zuliprc(
            "# 注释\n[other]\nemail=wrong\n[api]\n; 注释\nemail = a@x.test\nkey: abc=123\nsite=https://z.test/\n",
        )
        .unwrap();
        assert_eq!(
            parsed,
            Zuliprc {
                email: "a@x.test".into(),
                api_key: "abc=123".into(),
                site: "https://z.test".into(),
            }
        );
    }

    #[test]
    fn zuliprc_errors() {
        assert_eq!(
            parse_zuliprc("[api]\nemail=a\nkey=b\n")
                .unwrap_err()
                .to_string(),
            "zuliprc 中缺少 [api] 段的 email、key 或 site"
        );
        assert_eq!(
            parse_zuliprc("email=a\nkey=b\nsite=https://z.test")
                .unwrap_err()
                .to_string(),
            "zuliprc 中缺少 [api] 段的 email、key 或 site"
        );
        assert_eq!(
            parse_zuliprc("[api]\nemail=a\nkey=b\nsite=ftp://x")
                .unwrap_err()
                .to_string(),
            "zuliprc 中的 site 无效"
        );
    }
}
