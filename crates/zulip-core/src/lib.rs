//! Domain model and pure logic of the Zulip client; no IO at all.
//!
//! Text offsets are always UTF-8 byte offsets; UTF-16 offsets coming from the WebView are converted with [`offsets`] first.

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
