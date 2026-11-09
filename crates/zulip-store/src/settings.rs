//! Settings stored only on this machine (settings Zulip does not have).
//!
//! [`LocalSettings`] is app-level (shared by all accounts); [`AccountSettings`] is account-level. A
//! value that cannot be parsed falls back to the default. For reads and writes see [`SettingRepo`]:
//! neither blocks, and both may be called while holding any lock.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::db::Db;
use crate::paths::default_save_root;
use crate::setting::SettingRepo;

/// Setting keys.
pub mod keys {
    pub const OPEN_CHAT_IN_NEW_WINDOW: &str = "open_chat_in_new_window";
    pub const CLASSIC_MODE: &str = "classic_mode";
    pub const EDGE_AUTO_HIDE: &str = "edge_auto_hide";
    pub const SHOW_TRAY_ICON: &str = "show_tray_icon";
    pub const NOTIFICATION_MUTED: &str = "notification_muted";
    pub const CLOSE_ACTION: &str = "close_action";
    pub const SHOW_UNAVAILABLE: &str = "dev_show_unavailable";
    pub const LOG_REQUESTS: &str = "dev_log_requests";
    pub const REQUEST_LOG_LIMIT: &str = "dev_request_log_limit";
    pub const SHOW_SETTING_SOURCE: &str = "dev_show_setting_source";
    /// Key prefix for window position and size, followed by the window name.
    pub const WINDOW_BOUNDS_PREFIX: &str = "window_bounds_";
    /// Window name of the main window in classic mode.
    pub const MAIN_BOUNDS_CLASSIC: &str = "main_classic";
    /// Window name of the main window in default mode.
    pub const MAIN_BOUNDS_NORMAL: &str = "main_normal";
    pub const FONT_SIZE: &str = "font_size";
    pub const PALETTE: &str = "palette";
    pub const SAVE_ROOT: &str = "save_root";
    pub const TOPIC_TITLE_FORMAT: &str = "topic_title_format";
    pub const COLLAPSE_RESOLVED_TOPICS: &str = "collapse_resolved_topics";
    pub const CLIENT_NAME: &str = "client_name";
    pub const WILDCARD_MENTIONS_NOTIFY: &str = "wildcard_mentions_notify";
    pub const SYSTEM_NOTICES_CLEARED_BEFORE: &str = "system_notices_cleared_before";
    pub const COMPOSER_SOURCE_MODE: &str = "composer_source_mode";
    pub const CLEARED_REMINDER_IDS: &str = "cleared_reminder_ids";
    pub const RECENT_EMOJI: &str = "recent_emoji";
    pub const RECENT_SEARCH: &str = "recent_search";
}

pub const MIN_FONT_SIZE: i32 = 12;
pub const MAX_FONT_SIZE: i32 = 18;
pub const DEFAULT_FONT_SIZE: i32 = 14;

/// Default template for topic titles: show only the topic name.
pub const DEFAULT_TOPIC_TITLE_FORMAT: &str = zulip_core::text::topic_title::DEFAULT;

/// Default number of request log entries to keep.
pub const DEFAULT_REQUEST_LOG_LIMIT: usize = 1000;

/// When the number of request log entries to keep equals this value, the count is unlimited.
pub const UNLIMITED_REQUEST_LOG_LIMIT: usize = 0;

/// Behavior when the main panel is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAction {
    MinimizeToTray,
    Exit,
}

impl CloseAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::MinimizeToTray => "MinimizeToTray",
            Self::Exit => "Exit",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "MinimizeToTray" => Some(Self::MinimizeToTray),
            "Exit" => Some(Self::Exit),
            _ => None,
        }
    }
}

/// Typed reads and writes.
#[derive(Clone)]
struct Typed(SettingRepo);

impl Typed {
    fn read(&self, key: &str) -> Option<String> {
        self.0.get(key)
    }

    fn bool(&self, key: &str, default: bool) -> bool {
        match self.read(key).as_deref() {
            Some("1") => true,
            Some("0") => false,
            _ => default,
        }
    }

    fn int(&self, key: &str) -> Option<i64> {
        self.read(key)?.trim().parse().ok()
    }

    fn write(&self, key: &str, value: &str) {
        self.0.set(key, value);
    }

    fn write_bool(&self, key: &str, value: bool) {
        self.write(key, if value { "1" } else { "0" });
    }
}

/// A getter and setter pair for a boolean setting; the doc comment at the call site belongs to the
/// getter.
macro_rules! bool_setting {
    ($(#[$doc:meta])* $get:ident, $set:ident, $key:ident, $default:expr) => {
        $(#[$doc])*
        pub fn $get(&self) -> bool {
            self.0.bool(keys::$key, $default)
        }

        pub fn $set(&self, value: bool) {
            self.0.write_bool(keys::$key, value);
        }
    };
}

/// App-level settings, shared by all accounts. Also available when no account is online.
#[derive(Clone)]
pub struct LocalSettings(Typed);

impl LocalSettings {
    pub fn new(db: &Db) -> Self {
        Self(Typed(SettingRepo::app(db)))
    }

    bool_setting! {
        /// Open chats in a separate window (always in a separate window in classic mode).
        open_chat_in_new_window, set_open_chat_in_new_window, OPEN_CHAT_IN_NEW_WINDOW, false
    }

    bool_setting! {
        /// Classic mode: the main window has only the navigation bar and lists; chats all open in
        /// separate windows.
        classic_mode, set_classic_mode, CLASSIC_MODE, false
    }

    bool_setting! {
        /// Automatically hide the main window when it is docked to a screen edge.
        edge_auto_hide, set_edge_auto_hide, EDGE_AUTO_HIDE, true
    }

    bool_setting! {
        /// Show the tray icon.
        show_tray_icon, set_show_tray_icon, SHOW_TRAY_ICON, true
    }

    /// The saved behavior when the main panel is closed. For the actual behavior see
    /// [`effective_close_action`](Self::effective_close_action).
    pub fn close_action(&self) -> CloseAction {
        self.0
            .read(keys::CLOSE_ACTION)
            .as_deref()
            .and_then(CloseAction::parse)
            .unwrap_or(CloseAction::MinimizeToTray)
    }

    pub fn set_close_action(&self, value: CloseAction) {
        self.0.write(keys::CLOSE_ACTION, value.as_str());
    }

    /// The effective close behavior: without a tray icon the app cannot be restored from the tray,
    /// so it can only exit.
    pub fn effective_close_action(&self) -> CloseAction {
        if self.show_tray_icon() {
            self.close_action()
        } else {
            CloseAction::Exit
        }
    }

    bool_setting! {
        /// Developer mode: settings and actions that lack permission or are unsupported by the
        /// server are also shown; actions are only simulated and not sent to the server.
        show_unavailable, set_show_unavailable, SHOW_UNAVAILABLE, false
    }

    bool_setting! {
        /// Developer mode: below each setting, show its Zulip configuration name, or the scope of
        /// the local setting (global / account).
        show_setting_source, set_show_setting_source, SHOW_SETTING_SOURCE, false
    }

    bool_setting! {
        /// Developer mode: log requests and responses to and from the Zulip server, viewable on the
        /// "Developer mode" settings page.
        log_requests, set_log_requests, LOG_REQUESTS, false
    }

    /// Number of request log entries to keep; the earliest are deleted when exceeded;
    /// [`UNLIMITED_REQUEST_LOG_LIMIT`] means unlimited.
    pub fn request_log_limit(&self) -> usize {
        self.0
            .int(keys::REQUEST_LOG_LIMIT)
            .map_or(DEFAULT_REQUEST_LOG_LIMIT, |v| v.max(0) as usize)
    }

    pub fn set_request_log_limit(&self, value: usize) {
        self.0.write(keys::REQUEST_LOG_LIMIT, &value.to_string());
    }

    /// Window position and size; `None` if never saved. The format is defined by the caller.
    pub fn window_bounds(&self, window: &str) -> Option<String> {
        self.0
            .read(&format!("{}{window}", keys::WINDOW_BOUNDS_PREFIX))
    }

    pub fn set_window_bounds(&self, window: &str, bounds: &str) {
        self.0
            .write(&format!("{}{window}", keys::WINDOW_BOUNDS_PREFIX), bounds);
    }

    /// Color palette (JSON). `None` if never saved.
    pub fn palette(&self) -> Option<String> {
        self.0.read(keys::PALETTE)
    }

    pub fn set_palette(&self, palette: &str) {
        self.0.write(keys::PALETTE, palette);
    }

    /// Body font size, 12 to 18.
    pub fn font_size(&self) -> i32 {
        self.0.int(keys::FONT_SIZE).map_or(DEFAULT_FONT_SIZE, |v| {
            v.clamp(MIN_FONT_SIZE.into(), MAX_FONT_SIZE.into()) as i32
        })
    }

    pub fn set_font_size(&self, value: i32) {
        self.0.write(
            keys::FONT_SIZE,
            &value.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE).to_string(),
        );
    }

    bool_setting! {
        /// Mute (the speaker button at the bottom left of the main window): new messages only flash
        /// the taskbar, with no banner and no alert sound.
        notification_muted, set_notification_muted, NOTIFICATION_MUTED, false
    }

    /// The directory for "Save received or downloaded files to": files are saved here directly
    /// under their original names, with no subdirectories. Defaults to the Downloads folder.
    pub fn save_root(&self) -> PathBuf {
        self.0
            .read(keys::SAVE_ROOT)
            .filter(|v| !v.is_empty())
            .map_or_else(default_save_root, PathBuf::from)
    }

    pub fn set_save_root(&self, value: &str) {
        self.0.write(keys::SAVE_ROOT, value);
    }

    /// Client name reported to the server: placed in the User-Agent, from which the server records
    /// where a message was sent from and the user's activity. When empty, it is the product name.
    pub fn client_name(&self) -> String {
        self.0.read(keys::CLIENT_NAME).unwrap_or_default()
    }

    pub fn set_client_name(&self, value: &str) {
        self.0.write(keys::CLIENT_NAME, value.trim());
    }

    /// Title template for topic conversations (conversation list, chat title, separate window
    /// title).
    pub fn topic_title_format(&self) -> String {
        self.0
            .read(keys::TOPIC_TITLE_FORMAT)
            .unwrap_or_else(|| DEFAULT_TOPIC_TITLE_FORMAT.to_owned())
    }

    pub fn set_topic_title_format(&self, value: &str) {
        self.0.write(keys::TOPIC_TITLE_FORMAT, value);
    }

    bool_setting! {
        /// Collapse resolved topics: resolved topics go into "Resolved topics", do not appear in
        /// the message list or topic assistant, and do not alert.
        collapse_resolved_topics, set_collapse_resolved_topics, COLLAPSE_RESOLVED_TOPICS, false
    }
}

/// Account-level settings, related to the account's data.
#[derive(Clone)]
pub struct AccountSettings(Typed);

impl AccountSettings {
    pub fn new(db: &Db, account_id: i64) -> Self {
        Self(Typed(SettingRepo::account(db, account_id)))
    }

    bool_setting! {
        /// Alert on @all members (affects desktop alerts only; for mobile push and email see
        /// Zulip's setting of the same name).
        wildcard_mentions_notify, set_wildcard_mentions_notify, WILDCARD_MENTIONS_NOTIFY, true
    }

    bool_setting! {
        /// The composer shows Markdown source (otherwise it shows the rendered result).
        composer_source_mode, set_composer_source_mode, COMPOSER_SOURCE_MODE, false
    }

    /// Id of the newest notification when system notifications were cleared; notifications with an
    /// id not greater than it are no longer shown.
    pub fn system_notices_cleared_before(&self) -> i64 {
        self.0.int(keys::SYSTEM_NOTICES_CLEARED_BEFORE).unwrap_or(0)
    }

    pub fn set_system_notices_cleared_before(&self, value: i64) {
        self.0
            .write(keys::SYSTEM_NOTICES_CLEARED_BEFORE, &value.to_string());
    }

    /// Ids of reminder DMs cleared (no longer shown) from "Reminded".
    pub fn cleared_reminder_ids(&self) -> BTreeSet<i64> {
        self.0
            .read(keys::CLEARED_REMINDER_IDS)
            .unwrap_or_default()
            .split(',')
            .filter_map(|s| s.trim().parse::<i64>().ok())
            .filter(|&id| id > 0)
            .collect()
    }

    pub fn set_cleared_reminder_ids(&self, ids: &BTreeSet<i64>) {
        let text = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        self.0.write(keys::CLEARED_REMINDER_IDS, &text);
    }

    /// Recently used emoji names, newest first.
    pub fn recent_emoji(&self) -> Vec<String> {
        self.0
            .read(keys::RECENT_EMOJI)
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    pub fn set_recent_emoji(&self, names: &[String]) {
        self.0.write(
            keys::RECENT_EMOJI,
            &serde_json::Value::from(names).to_string(),
        );
    }

    /// Items recently opened from search results, as JSON (the session layer defines the format).
    pub fn recent_search(&self) -> Option<String> {
        self.0.read(keys::RECENT_SEARCH)
    }

    pub fn set_recent_search(&self, json: &str) {
        self.0.write(keys::RECENT_SEARCH, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> (Db, LocalSettings) {
        let db = Db::open_in_memory().unwrap();
        let settings = LocalSettings::new(&db);
        (db, settings)
    }

    #[test]
    fn defaults() {
        let (db, s) = settings();
        assert!(!s.open_chat_in_new_window() && !s.classic_mode());
        assert!(s.edge_auto_hide() && s.show_tray_icon());
        assert_eq!(s.close_action(), CloseAction::MinimizeToTray);
        assert_eq!(s.effective_close_action(), CloseAction::MinimizeToTray);
        assert!(!s.show_unavailable() && !s.show_setting_source() && !s.log_requests());
        assert_eq!(s.request_log_limit(), 1000);
        assert_eq!(s.window_bounds("main_normal"), None);
        assert_eq!(s.font_size(), 14);
        assert!(!s.notification_muted() && !s.collapse_resolved_topics());
        assert_eq!(s.save_root(), default_save_root());
        assert_eq!(s.client_name(), "");
        assert_eq!(s.topic_title_format(), "{Topic}");

        let a = AccountSettings::new(&db, 1);
        assert!(a.wildcard_mentions_notify());
        assert!(!a.composer_source_mode());
        assert_eq!(a.system_notices_cleared_before(), 0);
        assert!(a.cleared_reminder_ids().is_empty());
    }

    #[test]
    fn values_round_trip() {
        let (_db, s) = settings();
        s.set_classic_mode(true);
        s.set_edge_auto_hide(false);
        s.set_close_action(CloseAction::Exit);
        s.set_request_log_limit(UNLIMITED_REQUEST_LOG_LIMIT);
        s.set_window_bounds("main_classic", "1,2,300,400");
        s.set_save_root(r"D:\Files");
        s.set_client_name("  website \n");
        s.set_topic_title_format("{Channel} / {Topic}");
        s.set_log_requests(true);

        assert!(s.classic_mode() && !s.edge_auto_hide());
        assert_eq!(s.close_action(), CloseAction::Exit);
        assert_eq!(s.request_log_limit(), UNLIMITED_REQUEST_LOG_LIMIT);
        assert_eq!(
            s.window_bounds("main_classic").as_deref(),
            Some("1,2,300,400")
        );
        assert_eq!(s.window_bounds("main_normal"), None);
        assert_eq!(s.save_root(), PathBuf::from(r"D:\Files"));
        assert_eq!(s.client_name(), "website");
        assert_eq!(s.topic_title_format(), "{Channel} / {Topic}");
        assert!(s.log_requests());
    }

    #[test]
    fn closing_without_tray_icon_exits() {
        let (_db, s) = settings();
        s.set_show_tray_icon(false);
        assert_eq!(s.close_action(), CloseAction::MinimizeToTray);
        assert_eq!(s.effective_close_action(), CloseAction::Exit);
    }

    #[test]
    fn font_size_is_clamped_and_bad_values_fall_back() {
        let (db, s) = settings();
        s.set_font_size(40);
        assert_eq!(s.font_size(), 18);
        s.set_font_size(1);
        assert_eq!(s.font_size(), 12);

        let raw = SettingRepo::app(&db);
        raw.set(keys::FONT_SIZE, "99");
        assert_eq!(s.font_size(), 18);
        raw.set(keys::FONT_SIZE, "大");
        assert_eq!(s.font_size(), 14);
        raw.set(keys::REQUEST_LOG_LIMIT, "-5");
        assert_eq!(s.request_log_limit(), UNLIMITED_REQUEST_LOG_LIMIT);
        raw.set(keys::REQUEST_LOG_LIMIT, "abc");
        assert_eq!(s.request_log_limit(), DEFAULT_REQUEST_LOG_LIMIT);
        raw.set(keys::CLOSE_ACTION, "Unknown");
        assert_eq!(s.close_action(), CloseAction::MinimizeToTray);
        raw.set(keys::CLASSIC_MODE, "yes");
        assert!(!s.classic_mode());
        raw.set(keys::SAVE_ROOT, "");
        assert_eq!(s.save_root(), default_save_root());
    }

    #[test]
    fn account_settings_are_per_account_and_separate_from_app() {
        let (db, app) = settings();
        let (a, b) = (AccountSettings::new(&db, 1), AccountSettings::new(&db, 2));
        a.set_wildcard_mentions_notify(false);
        a.set_composer_source_mode(true);
        a.set_system_notices_cleared_before(77);
        assert!(!a.wildcard_mentions_notify() && a.composer_source_mode());
        assert_eq!(a.system_notices_cleared_before(), 77);
        assert!(b.wildcard_mentions_notify() && !b.composer_source_mode());
        assert!(SettingRepo::app(&db).get_all().is_empty());
        assert!(!app.classic_mode());
    }

    #[test]
    fn stored_key_strings_stay_the_same() {
        assert_eq!(keys::RECENT_EMOJI, "recent_emoji");
        assert_eq!(keys::MAIN_BOUNDS_CLASSIC, "main_classic");
        assert_eq!(keys::MAIN_BOUNDS_NORMAL, "main_normal");
        assert_eq!(DEFAULT_TOPIC_TITLE_FORMAT, "{Topic}");
    }

    #[test]
    fn recent_emoji_keeps_stored_format_and_tolerates_bad_values() {
        let (db, _) = settings();
        let a = AccountSettings::new(&db, 1);
        assert!(a.recent_emoji().is_empty());
        a.set_recent_emoji(&["heart".to_owned(), "+1".to_owned()]);
        let raw = SettingRepo::account(&db, 1);
        assert_eq!(
            raw.get(keys::RECENT_EMOJI).as_deref(),
            Some(r#"["heart","+1"]"#)
        );
        assert_eq!(a.recent_emoji(), ["heart", "+1"]);

        raw.set(keys::RECENT_EMOJI, "不是 JSON");
        assert!(a.recent_emoji().is_empty());
        raw.set(keys::RECENT_EMOJI, "[1,2]");
        assert!(a.recent_emoji().is_empty());
    }

    #[test]
    fn cleared_reminder_ids_are_sorted_and_sanitized() {
        let (db, _) = settings();
        let a = AccountSettings::new(&db, 1);
        a.set_cleared_reminder_ids(&BTreeSet::from([30, 5, 12]));
        assert_eq!(
            SettingRepo::account(&db, 1)
                .get(keys::CLEARED_REMINDER_IDS)
                .as_deref(),
            Some("5,12,30")
        );
        assert_eq!(a.cleared_reminder_ids(), BTreeSet::from([5, 12, 30]));

        SettingRepo::account(&db, 1).set(keys::CLEARED_REMINDER_IDS, "7, x,,0,-3,9");
        assert_eq!(a.cleared_reminder_ids(), BTreeSet::from([7, 9]));
    }
}
