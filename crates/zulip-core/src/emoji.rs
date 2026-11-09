//! Emoji catalog: organization custom emoji first, then Unicode emoji. Lookup works by name or
//! alias; when a custom emoji and a Unicode emoji share a name, the custom emoji wins (same as
//! Zulip).

use std::collections::HashMap;

use serde_json::Value;

use crate::html;
use crate::util::{contains_ignore_case, starts_with_ignore_case};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EmojiKind {
    Unicode,
    Realm,
}

impl EmojiKind {
    /// The reaction_type of a reaction.
    pub fn reaction_type(self) -> &'static str {
        match self {
            Self::Unicode => "unicode_emoji",
            Self::Realm => "realm_emoji",
        }
    }
}

/// An emoji. For a Unicode emoji `code` is the code point (such as `1f44d`) and `text` is the
/// character; for a custom emoji `code` is the emoji id and `image_url` is the image URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmojiEntry {
    pub name: String,
    pub code: String,
    pub kind: EmojiKind,
    pub aliases: Vec<String>,
    pub text: Option<String>,
    pub image_url: Option<String>,
}

impl EmojiEntry {
    fn names(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.name.as_str()).chain(self.aliases.iter().map(String::as_str))
    }
}

#[derive(Clone, Debug, Default)]
pub struct EmojiCatalog {
    all: Vec<EmojiEntry>,
    by_name: HashMap<String, usize>,
}

impl EmojiCatalog {
    pub fn new(realm: Vec<EmojiEntry>, unicode: Vec<EmojiEntry>) -> Self {
        let realm_count = realm.len();
        let all: Vec<EmojiEntry> = realm.into_iter().chain(unicode).collect();
        let mut by_name = HashMap::new();
        for (index, entry) in all.iter().enumerate().skip(realm_count) {
            for name in entry.names() {
                by_name.entry(name.to_owned()).or_insert(index);
            }
        }
        for (index, entry) in all.iter().enumerate().take(realm_count) {
            by_name.insert(entry.name.clone(), index);
        }
        Self { all, by_name }
    }

    pub fn all(&self) -> &[EmojiEntry] {
        &self.all
    }

    /// Look up by name or alias.
    pub fn get(&self, name: &str) -> Option<&EmojiEntry> {
        self.by_name.get(name).map(|i| &self.all[*i])
    }

    /// Search: name or alias contains the keyword (case-insensitive, a leading colon is allowed).
    /// Those whose name or alias starts with the keyword come first, the rest keep catalog order.
    /// An empty keyword returns everything.
    pub fn search(&self, query: &str) -> Vec<&EmojiEntry> {
        let query = query.trim().trim_start_matches(':');
        let (prefix, others): (Vec<&EmojiEntry>, Vec<&EmojiEntry>) = self
            .all
            .iter()
            .partition(|e| e.names().any(|n| starts_with_ignore_case(n, query)));
        let contains = others
            .into_iter()
            .filter(|e| e.names().any(|n| contains_ignore_case(n, query)));
        prefix.into_iter().chain(contains).collect()
    }
}

/// Parse the content of `server_emoji_data_url`: `{"code_to_names": {"1f44d": ["+1", "thumbs_up",
/// ...]}}`, the first name is the canonical name, the rest are aliases. Code points that cannot be
/// converted to a character are skipped.
pub fn parse_unicode(json: &str) -> Result<Vec<EmojiEntry>, serde_json::Error> {
    let root: Value = serde_json::from_str(json)?;
    let Some(map) = root.get("code_to_names").and_then(Value::as_object) else {
        return Ok(Vec::new());
    };
    let mut result: Vec<EmojiEntry> = map
        .iter()
        .filter_map(|(code, names)| {
            let mut names = names
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned);
            let name = names.next()?;
            Some(EmojiEntry {
                name,
                code: code.clone(),
                kind: EmojiKind::Unicode,
                aliases: names.collect(),
                text: Some(unicode_from_code(code)?),
                image_url: None,
            })
        })
        .collect();
    // Data is sorted by code point, so keycaps and flags (shown as letters on Windows) would come
    // first; adjust by block so smiley faces come first
    result.sort_by_key(|e| block_rank(&e.code));
    Ok(result)
}

/// Convert a code point string like `1f469-200d-1f4bb` to a character; `None` if it cannot be
/// parsed.
pub fn unicode_from_code(code: &str) -> Option<String> {
    html::emoji_from_class(&format!("emoji-{code}"))
}

// The data has no categories; sort by the Unicode block of the first code point
fn block_rank(code: &str) -> u8 {
    let first = code.split('-').next().unwrap_or(code);
    let Ok(codepoint) = u32::from_str_radix(first, 16) else {
        return 7;
    };
    match codepoint {
        0x1F600..=0x1F64F => 0,                     // Smiley faces
        0x1F900..=0x1F9FF => 1,                     // Supplemental emoticons and people
        0x1F440..=0x1F4FF => 2,                     // Gestures, people, objects
        0x1F300..=0x1F43F => 3,                     // Nature, weather, animals, food
        0x1F680..=0x1F6FF | 0x1FA70..=0x1FAFF => 4, // Transport, objects
        0x1F1E6..=0x1F1FF | 0..=0x1FFF => 7,        // Flags, keycaps, (c) and (R) signs
        0x2600..=0x27BF => 5,                       // Miscellaneous symbols
        _ => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unicode() -> Vec<EmojiEntry> {
        parse_unicode(include_str!("../testdata/emoji_data_sample.json")).unwrap()
    }

    fn parrot(name: &str, code: &str) -> EmojiEntry {
        EmojiEntry {
            name: name.to_owned(),
            code: code.to_owned(),
            kind: EmojiKind::Realm,
            aliases: Vec::new(),
            text: None,
            image_url: Some("/user_avatars/2/emoji/images/7.gif".to_owned()),
        }
    }

    fn find<'a>(entries: &'a [EmojiEntry], name: &str) -> &'a EmojiEntry {
        entries.iter().find(|e| e.name == name).unwrap()
    }

    #[test]
    fn first_name_is_canonical() {
        let entries = unicode();
        let thumbs = entries.iter().find(|e| e.code == "1f44d").unwrap();
        assert_eq!(thumbs.name, "+1");
        assert_eq!(thumbs.aliases, ["thumbs_up", "like"]);
        assert_eq!(thumbs.text.as_deref(), Some("👍"));
        assert_eq!(thumbs.kind.reaction_type(), "unicode_emoji");
    }

    #[test]
    fn multi_codepoint_and_keycap() {
        let entries = unicode();
        assert_eq!(find(&entries, "technologist").text.as_deref(), Some("🧑‍💻"));
        assert_eq!(find(&entries, "hash").text.as_deref(), Some("#\u{20e3}"));
    }

    #[test]
    fn faces_first_keycaps_last() {
        let entries = unicode();
        assert_eq!(entries[0].code, "1f600");
        assert_eq!(entries[entries.len() - 1].name, "hash");
    }

    #[test]
    fn skips_invalid_code() {
        assert!(!unicode().iter().any(|e| e.name == "broken"));
    }

    #[test]
    fn invalid_json_is_an_error() {
        assert!(parse_unicode("{").is_err());
        assert!(parse_unicode("[]").unwrap().is_empty());
    }

    #[test]
    fn get_by_alias_returns_canonical_entry() {
        let catalog = EmojiCatalog::new(Vec::new(), unicode());
        assert_eq!(
            catalog.get("pray").map(|e| e.name.as_str()),
            Some("folded_hands")
        );
        assert_eq!(catalog.get("nope"), None);
    }

    #[test]
    fn realm_emoji_comes_first_and_overrides_same_name() {
        let catalog =
            EmojiCatalog::new(vec![parrot("parrot", "7"), parrot("smile", "8")], unicode());
        assert_eq!(catalog.all()[0].name, "parrot");
        assert_eq!(catalog.get("smile").map(|e| e.kind), Some(EmojiKind::Realm));
    }

    #[test]
    fn search_prefix_before_contains() {
        let catalog = EmojiCatalog::new(Vec::new(), unicode());
        let names = |query: &str| -> Vec<String> {
            catalog
                .search(query)
                .iter()
                .map(|e| e.name.clone())
                .collect()
        };
        assert_eq!(names(":ha"), ["grinning", "hash", "folded_hands"]);
        assert_eq!(names("love"), ["heart"]);
        assert_eq!(names("ears"), ["joy"]);
        assert_eq!(catalog.search("  ").len(), catalog.all().len());
    }

    #[test]
    fn empty_catalog() {
        assert!(EmojiCatalog::default().search("").is_empty());
    }
}
