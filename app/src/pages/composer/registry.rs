//! Composers currently being shown (account, conversation), and the decision when a plugin fills
//! into the composer.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use zulip_core::ConversationKey;

/// The same conversation may be open in both the main window and a standalone window, so it is
/// recorded by count.
#[derive(Default)]
struct OpenComposers(HashMap<(i64, ConversationKey), usize>);

impl OpenComposers {
    fn add(&mut self, account: i64, key: ConversationKey) {
        *self.0.entry((account, key)).or_default() += 1;
    }

    fn remove(&mut self, account: i64, key: ConversationKey) {
        let entry = (account, key);
        if let Some(count) = self.0.get_mut(&entry) {
            *count -= 1;
            if *count == 0 {
                self.0.remove(&entry);
            }
        }
    }

    fn contains(&self, account: i64, key: &ConversationKey) -> bool {
        self.0.contains_key(&(account, key.clone()))
    }
}

static OPEN: LazyLock<Mutex<OpenComposers>> = LazyLock::new(Mutex::default);

fn open() -> std::sync::MutexGuard<'static, OpenComposers> {
    OPEN.lock().unwrap_or_else(|e| e.into_inner())
}

pub(super) fn register(account: i64, key: ConversationKey) {
    open().add(account, key);
}

pub(super) fn unregister(account: i64, key: ConversationKey) {
    open().remove(account, key);
}

/// A composer is currently showing this conversation.
pub fn is_composer_open(account: i64, key: &ConversationKey) -> bool {
    open().contains(account, key)
}

/// Whether this composer receives FillComposer: when a conversation is specified, check the account
/// and conversation; when not specified, check the account, and this conversation is the one the
/// account's user switched to last.
pub(super) fn fill_targets_composer(
    event_account: i64,
    event_conv: Option<&ConversationKey>,
    account: i64,
    key: &ConversationKey,
    current: Option<&ConversationKey>,
) -> bool {
    event_account == account
        && match event_conv {
            Some(conv) => conv == key,
            None => current == Some(key),
        }
}

/// Text filled in while no composer shows the conversation is written to the draft: replacing it,
/// or appended after the existing draft.
pub fn filled_draft(existing: Option<&str>, text: &str, replace: bool) -> String {
    match existing.filter(|draft| !replace && !draft.is_empty()) {
        Some(draft) if draft.ends_with('\n') => format!("{draft}{text}"),
        Some(draft) => format!("{draft}\n{text}"),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(stream: i64) -> ConversationKey {
        ConversationKey::topic(stream, "话题")
    }

    #[test]
    fn counts_every_open_composer() {
        let mut open = OpenComposers::default();
        assert!(!open.contains(1, &key(1)));
        open.add(1, key(1));
        open.add(1, key(1));
        assert!(open.contains(1, &key(1)));
        assert!(!open.contains(2, &key(1)));
        assert!(!open.contains(1, &key(2)));
        open.remove(1, key(1));
        assert!(open.contains(1, &key(1)));
        open.remove(1, key(1));
        assert!(!open.contains(1, &key(1)));
        open.remove(1, key(1));
        assert!(!open.contains(1, &key(1)));
    }

    #[test]
    fn fill_goes_to_the_named_or_current_conversation() {
        let (mine, other) = (key(1), key(2));
        assert!(fill_targets_composer(
            1,
            Some(&mine),
            1,
            &mine,
            Some(&other)
        ));
        assert!(!fill_targets_composer(
            1,
            Some(&other),
            1,
            &mine,
            Some(&mine)
        ));
        assert!(!fill_targets_composer(2, Some(&mine), 1, &mine, None));
        assert!(fill_targets_composer(1, None, 1, &mine, Some(&mine)));
        assert!(!fill_targets_composer(1, None, 1, &mine, Some(&other)));
        assert!(!fill_targets_composer(1, None, 1, &mine, None));
        assert!(!fill_targets_composer(2, None, 1, &mine, Some(&mine)));
    }

    #[test]
    fn draft_is_replaced_or_appended() {
        assert_eq!(filled_draft(None, "新", false), "新");
        assert_eq!(filled_draft(Some(""), "新", false), "新");
        assert_eq!(filled_draft(Some("旧"), "新", false), "旧\n新");
        assert_eq!(filled_draft(Some("旧\n"), "新", false), "旧\n新");
        assert_eq!(filled_draft(Some("旧"), "新", true), "新");
    }
}
