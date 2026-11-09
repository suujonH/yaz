//! Zulip 客户端的领域模型与纯逻辑，不做任何 IO。
//!
//! 文字偏移一律是 UTF-8 字节偏移；来自 WebView 的 UTF-16 偏移先用 [`offsets`] 转换。

pub mod avatar;
pub mod color;
pub mod conversation;
pub mod emoji;
pub mod file;
pub mod html;
pub mod links;
pub mod markdown;
pub mod model;
pub mod notice;
pub mod offsets;
pub mod permission;
pub mod push;
pub mod text;
pub mod widget;
pub mod window;

mod util;

pub use util::{contains_ignore_case, starts_with_ignore_case};

pub use conversation::{
    ConversationFolder, ConversationKey, DmKey, ListEntry, ParseKeyError, TopicKey,
};
