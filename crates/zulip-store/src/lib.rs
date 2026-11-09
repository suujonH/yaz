//! Local storage: the SQLite database and repositories, settings, data directory, logs, and API key
//! encryption.

mod account;
mod contact;
mod conversation;
mod db;
mod download;
mod draft;
mod error;
mod location;
mod logging;
mod marks;
mod message;
mod paths;
mod plugin;
mod secret;
mod setting;
mod settings;

pub use account::{AccountRepo, AccountRow};
pub use contact::ContactRepo;
pub use conversation::{
    ConversationCache, ConversationCacheRepo, ConversationPref, ConversationPrefRepo,
};
pub use db::{Db, SCHEMA_VERSION};
pub use download::{DownloadRecord, DownloadRepo};
pub use draft::LocalDraftRepo;
pub use error::{Result, StoreError};
pub use location::{DataLocation, MoveError, Resolved, rebase_paths};
pub use logging::{FileLogger, init_logging};
pub use marks::{PinRepo, RemarkRepo};
pub use message::{DeletedMessage, DeletedMessageRepo, MessageVersionRepo};
pub use paths::{AppPaths, default_save_root};
pub use plugin::{PluginRecord, PluginRepo};
pub use secret::{protect_key, unprotect_key};
pub use setting::SettingRepo;
pub use settings::*;
