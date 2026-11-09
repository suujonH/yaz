/// Online status of a user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub enum PresenceState {
    #[default]
    Offline,
    Idle,
    Online,
}

/// Kind of mention of oneself in an unread message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum MentionKind {
    #[default]
    None,
    /// @me (including user groups I belong to).
    Personal,
    /// @everyone, @topic.
    Wildcard,
}

/// Red marker shown before the conversation preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum ConversationNotice {
    #[default]
    None,
    AtAll,
    AtMe,
    NewFile,
}
