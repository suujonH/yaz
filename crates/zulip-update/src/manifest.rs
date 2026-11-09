use serde::{Deserialize, Serialize};

/// Release 中清单文件的名称；由 tools/pack-release.ps1 生成。
pub const MANIFEST_FILE_NAME: &str = "YaZ-update.json";

/// 更新包大小的上限，防止清单错误时写满磁盘。
pub const MAX_PACKAGE_SIZE: u64 = 2 * 1024 * 1024 * 1024;

/// Release 附带的清单：最新版本和更新包的名称、大小、SHA-256。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateManifest {
    pub version: String,
    pub package: String,
    pub sha256: String,
    pub size: u64,
}

impl UpdateManifest {
    /// 解析并检查清单；内容不合法时返回 `None`。
    pub fn parse(json: &str) -> Option<Self> {
        let manifest: Self = serde_json::from_str(json).ok()?;
        manifest.is_valid().then_some(manifest)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    // 包名只能是一个文件名：下载地址由它拼接，不能借此跳到别的路径
    fn is_valid(&self) -> bool {
        crate::parse_version(&self.version).is_some()
            && !self.package.is_empty()
            && self
                .package
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            && !self.package.starts_with('.')
            && self.package.to_ascii_lowercase().ends_with(".zip")
            && self.sha256.len() == 64
            && self.sha256.chars().all(|c| c.is_ascii_hexdigit())
            && (1..=MAX_PACKAGE_SIZE).contains(&self.size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn parses_valid_json_and_round_trips() {
        let manifest = UpdateManifest {
            version: "1.2.3".into(),
            package: "YaZ-v1.2.3-win-x64.zip".into(),
            sha256: HASH.into(),
            size: 10,
        };
        assert_eq!(UpdateManifest::parse(&manifest.to_json()), Some(manifest));
    }

    #[test]
    fn rejects_invalid_content() {
        let manifest = |version: &str, package: &str, sha256: &str, size: i64| {
            format!(
                r#"{{"version":"{version}","package":"{package}","sha256":"{sha256}","size":{size}}}"#
            )
        };
        let cases = [
            "not json".to_string(),
            "{}".to_string(),
            manifest("0.01", "a.zip", HASH, 1),
            manifest("1.0.0", "../evil.zip", HASH, 1),
            manifest("1.0.0", "a.exe", HASH, 1),
            manifest("1.0.0", "a.zip", "abc", 1),
            manifest("1.0.0", "a.zip", HASH, 0),
            manifest("1.0.0", "a.zip", HASH, -1),
            manifest("1.0.0", "a.zip", HASH, MAX_PACKAGE_SIZE as i64 + 1),
        ];
        for json in cases {
            assert_eq!(UpdateManifest::parse(&json), None, "{json}");
        }
        assert!(UpdateManifest::parse(&manifest("1.0.0", "a.zip", HASH, 1)).is_some());
    }
}
