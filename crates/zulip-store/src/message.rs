use rusqlite::params;

use crate::db::Db;
use crate::error::Result;

/// Local copy of a deleted message: its content in the message cache at deletion time (excluding
/// reactions and widgets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedMessage {
    pub message_id: i64,
    /// Storage string of the conversation it belongs to.
    pub conv_key: String,
    pub sender_id: i64,
    pub sender_name: String,
    pub avatar_url: Option<String>,
    /// HTML rendered by the server.
    pub content: String,
    /// Unix seconds.
    pub timestamp: i64,
    pub last_edit_timestamp: Option<i64>,
    pub is_me_message: bool,
    /// Topic name (original case); empty for direct messages.
    pub topic: String,
}

/// A deleted message. The same message is stored only once.
pub struct DeletedMessageRepo {
    db: Db,
    account_id: i64,
}

impl DeletedMessageRepo {
    pub fn new(db: &Db, account_id: i64) -> Self {
        Self {
            db: db.clone(),
            account_id,
        }
    }

    pub fn add(&self, m: &DeletedMessage) {
        self.db.write(
            "INSERT INTO deleted_message(account_id, message_id, conv_key, sender_id, sender_name, avatar_url, content,
               timestamp, last_edit_timestamp, is_me_message, topic)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(account_id, message_id) DO NOTHING",
            params![
                self.account_id,
                m.message_id,
                m.conv_key,
                m.sender_id,
                m.sender_name,
                m.avatar_url,
                m.content,
                m.timestamp,
                m.last_edit_timestamp,
                m.is_me_message,
                m.topic
            ],
        );
    }

    /// All deleted messages, in ascending id order.
    pub fn load_all(&self) -> Result<Vec<DeletedMessage>> {
        self.db.query(
            "SELECT message_id, conv_key, sender_id, sender_name, avatar_url, content, timestamp, last_edit_timestamp,
               is_me_message, topic
             FROM deleted_message WHERE account_id = ?1 ORDER BY message_id",
            [self.account_id],
            |r| {
                Ok(DeletedMessage {
                    message_id: r.get(0)?,
                    conv_key: r.get(1)?,
                    sender_id: r.get(2)?,
                    sender_name: r.get(3)?,
                    avatar_url: r.get(4)?,
                    content: r.get(5)?,
                    timestamp: r.get(6)?,
                    last_edit_timestamp: r.get(7)?,
                    is_me_message: r.get(8)?,
                    topic: r.get(9)?,
                })
            },
        )
    }
}

/// Versions of a message before edits (HTML rendered by the server). Comes from the message's
/// edit_history and update_message events; the same edit (same edit time) is stored only once, the
/// first one stored wins.
pub struct MessageVersionRepo {
    db: Db,
    account_id: i64,
}

impl MessageVersionRepo {
    pub fn new(db: &Db, account_id: i64) -> Self {
        Self {
            db: db.clone(),
            account_id,
        }
    }

    /// Versions before edits, from the earliest (original content) to the most recent.
    pub fn load(&self, message_id: i64) -> Result<Vec<String>> {
        self.db.query(
            "SELECT content FROM message_version WHERE account_id = ?1 AND message_id = ?2 ORDER BY edited_at",
            params![self.account_id, message_id],
            |r| r.get(0),
        )
    }

    /// Stores the content replaced by the edit at `edited_at` (Unix seconds).
    pub fn add(&self, message_id: i64, edited_at: i64, content: &str) {
        self.db.write(
            "INSERT INTO message_version(account_id, message_id, edited_at, content) VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(account_id, message_id, edited_at) DO NOTHING",
            params![self.account_id, message_id, edited_at, content],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::tests::add_account;

    #[test]
    fn deleted_messages_are_saved_once() {
        let db = Db::open_in_memory().unwrap();
        let repo = DeletedMessageRepo::new(&db, add_account(&db));
        let message = |id, content: &str| DeletedMessage {
            message_id: id,
            conv_key: "t:1:a".into(),
            sender_id: 1,
            sender_name: "甲".into(),
            avatar_url: None,
            content: content.into(),
            timestamp: 2,
            last_edit_timestamp: Some(3),
            is_me_message: false,
            topic: "A".into(),
        };
        repo.add(&message(2, "二"));
        repo.add(&message(1, "一"));
        repo.add(&message(1, "另一个"));
        assert_eq!(
            repo.load_all().unwrap(),
            [message(1, "一"), message(2, "二")]
        );
    }

    #[test]
    fn versions_keep_first_for_each_edit_time() {
        let db = Db::open_in_memory().unwrap();
        let repo = MessageVersionRepo::new(&db, add_account(&db));
        repo.add(1, 20, "二");
        repo.add(1, 10, "一");
        repo.add(1, 20, "另一个");
        repo.add(2, 10, "别的消息");
        assert_eq!(repo.load(1).unwrap(), ["一", "二"]);
    }
}
