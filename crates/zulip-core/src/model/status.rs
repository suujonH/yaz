/// 用户的在线状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub enum PresenceState {
    #[default]
    Offline,
    Idle,
    Online,
}

/// 未读消息对自己的提及类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum MentionKind {
    #[default]
    None,
    /// @我（含所在的用户组）。
    Personal,
    /// @全体成员、@topic。
    Wildcard,
}

/// 会话预览前显示的红色提示。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum ConversationNotice {
    #[default]
    None,
    AtAll,
    AtMe,
    NewFile,
}
