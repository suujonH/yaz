//! 只保存在本机的标记（备注、置顶）的目标。

use std::fmt;
use std::str::FromStr;

use crate::conversation::{ConversationFolder, ConversationKey, ListEntry, ParseKeyError};

/// 备注的对象。一对一私聊的备注就是对方的备注，见 [`canonical`](Self::canonical)。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RemarkTarget {
    User(i64),
    Channel(i64),
    Conversation(ConversationKey),
}

impl RemarkTarget {
    /// 同一含义只有一种写法：一对一私聊 → 对方。
    pub fn canonical(self) -> Self {
        match self {
            Self::Conversation(ConversationKey::Dm(dm)) if dm.other_user_ids().len() == 1 => {
                Self::User(dm.other_user_ids()[0])
            }
            other => other,
        }
    }

    /// 存储形式：`(种类, 对象 id)`。
    pub fn storage(&self) -> (&'static str, String) {
        match self {
            Self::User(id) => ("user", id.to_string()),
            Self::Channel(id) => ("channel", id.to_string()),
            Self::Conversation(key) => ("conversation", key.to_string()),
        }
    }

    pub fn from_storage(kind: &str, id: &str) -> Option<Self> {
        match kind {
            "user" => id.parse().ok().map(Self::User),
            "channel" => id.parse().ok().map(Self::Channel),
            "conversation" => id.parse().ok().map(Self::Conversation),
            _ => None,
        }
    }
}

/// 置顶所在的页面。两个页面的置顶互相独立。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PinScope {
    Messages,
    Contacts,
}

impl PinScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Messages => "messages",
            Self::Contacts => "contacts",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "messages" => Some(Self::Messages),
            "contacts" => Some(Self::Contacts),
            _ => None,
        }
    }
}

/// 可以置顶的东西。消息页是会话和会话夹，联系人页是好友和话题。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PinItem {
    User(i64),
    Conversation(ConversationKey),
    Folder(ConversationFolder),
}

impl From<&ListEntry> for PinItem {
    fn from(entry: &ListEntry) -> Self {
        match entry {
            ListEntry::Conversation(key) => Self::Conversation(key.clone()),
            ListEntry::Folder(folder) => Self::Folder(*folder),
        }
    }
}

/// 存储形式：好友 `u:{id}`，会话同会话键，会话夹 `helper` / `resolved`。
impl fmt::Display for PinItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User(id) => write!(f, "u:{id}"),
            Self::Conversation(key) => key.fmt(f),
            Self::Folder(folder) => folder.fmt(f),
        }
    }
}

impl FromStr for PinItem {
    type Err = ParseKeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "helper" => return Ok(Self::Folder(ConversationFolder::Helper)),
            "resolved" => return Ok(Self::Folder(ConversationFolder::Resolved)),
            _ => {}
        }
        if let Some(id) = s.strip_prefix("u:") {
            return id
                .parse()
                .map(Self::User)
                .map_err(|_| ParseKeyError(s.to_owned()));
        }
        s.parse().map(Self::Conversation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_to_one_dm_remark_is_the_users() {
        let dm = RemarkTarget::Conversation(ConversationKey::dm([9], 1)).canonical();
        assert_eq!(dm, RemarkTarget::User(9));
        let group = RemarkTarget::Conversation(ConversationKey::dm([9, 10], 1));
        assert_eq!(group.clone().canonical(), group);
        let own = RemarkTarget::Conversation(ConversationKey::dm([1], 1));
        assert_eq!(own.clone().canonical(), own);
    }

    #[test]
    fn remark_target_storage_round_trip() {
        for target in [
            RemarkTarget::User(9),
            RemarkTarget::Channel(4),
            RemarkTarget::Conversation(ConversationKey::topic(4, "a:b")),
        ] {
            let (kind, id) = target.storage();
            assert_eq!(RemarkTarget::from_storage(kind, &id), Some(target));
        }
        assert_eq!(RemarkTarget::from_storage("x", "1"), None);
        assert_eq!(RemarkTarget::from_storage("user", "x"), None);
    }

    #[test]
    fn pin_item_text_round_trip() {
        for item in [
            PinItem::User(9),
            PinItem::Conversation(ConversationKey::topic(4, "接口")),
            PinItem::Conversation(ConversationKey::dm([2, 3], 1)),
            PinItem::Folder(ConversationFolder::Helper),
            PinItem::Folder(ConversationFolder::Resolved),
        ] {
            assert_eq!(item.to_string().parse::<PinItem>().unwrap(), item);
        }
        assert!("u:x".parse::<PinItem>().is_err());
    }
}
