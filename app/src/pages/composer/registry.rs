//! 正在显示的输入框（账号，会话），以及插件“填入输入框”时的决策。

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use zulip_core::ConversationKey;

/// 同一会话可能同时在主窗口和独立窗口中打开，所以按个数记录。
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

/// 有输入框正在显示这个会话。
pub fn is_composer_open(account: i64, key: &ConversationKey) -> bool {
    open().contains(account, key)
}

/// 这个输入框是否接收 FillComposer：指定了会话时看账号和会话；
/// 没有指定时看账号，且这个会话是该账号用户最后切换到的会话。
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

/// 没有输入框显示会话时填入的文字写进草稿：替换，或追加到现有草稿后。
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
