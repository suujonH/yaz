use semver::BuildMetadata;
pub use semver::Version;

/// Parses `major.minor.patch[-prerelease][+build]`, optionally with a v prefix. Build info is
/// discarded and does not take part in comparison.
pub fn parse_version(text: &str) -> Option<Version> {
    let text = text.trim();
    let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
    let mut version = Version::parse(text).ok()?;
    version.build = BuildMetadata::EMPTY;
    Some(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        parse_version(text).unwrap()
    }

    #[test]
    fn orders_versions_by_semver() {
        let pairs = [
            ("1.2.3", "1.2.4"),
            ("1.2.9", "1.3.0"),
            ("1.9.9", "2.0.0"),
            ("0.9.0", "0.10.0"),
            ("1.0.0-beta", "1.0.0"),
            ("1.0.0-alpha", "1.0.0-alpha.1"),
            ("1.0.0-alpha.1", "1.0.0-alpha.beta"),
            ("1.0.0-beta.2", "1.0.0-beta.11"),
            ("1.0.0-rc.1", "1.0.1-alpha"),
        ];
        for (lower, higher) in pairs {
            assert!(v(higher) > v(lower), "{higher} 应高于 {lower}");
            assert!(v(lower) <= v(higher));
        }
    }

    #[test]
    fn prefix_and_build_metadata_are_ignored() {
        for (a, b) in [
            ("1.2.3", "v1.2.3"),
            ("1.2.3", "1.2.3+abcdef0"),
            ("1.2.3-beta", "V1.2.3-beta+build.5"),
        ] {
            assert_eq!(v(a), v(b));
        }
    }

    #[test]
    fn invalid_versions_are_rejected() {
        for text in [
            "",
            "  ",
            "0.01",
            "1.2",
            "1.2.3.4",
            "1.02.3",
            "a.b.c",
            "1.2.3-",
            "1.2.3-beta..1",
            "1.2.3-01",
            "1.2.3+",
            "vv1.2.3",
        ] {
            assert!(parse_version(text).is_none(), "{text:?} 应被拒绝");
        }
    }

    #[test]
    fn displays_without_build_metadata() {
        assert_eq!(v("v1.2.3-beta.1+abc").to_string(), "1.2.3-beta.1");
    }
}
