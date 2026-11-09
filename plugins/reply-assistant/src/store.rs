//! Data the plugin stores itself (reply-assistant.db in the data directory): topic scenes, friend
//! identities, analysis results. Accessed by both the plugin thread and the worker thread; if the
//! database cannot be opened, reads are empty and saves are ignored (written to the plugin log).

use std::sync::OnceLock;

use rusqlite::{Connection, OptionalExtension, params};
use zulip_plugin_sdk::abi::LOG_WARNING;
use zulip_plugin_sdk::annotation_store::AnnotationStore;
use zulip_plugin_sdk::log;

const DATABASE_FILE: &str = "reply-assistant.db";
const ANALYSIS_TABLE: &str = "analysis";
// The analysis table is created by AnnotationStore.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS topic_scene(
    account INTEGER NOT NULL, stream INTEGER NOT NULL, topic TEXT NOT NULL COLLATE NOCASE,
    name TEXT NOT NULL, scene TEXT NOT NULL, PRIMARY KEY(account, stream, topic));
CREATE TABLE IF NOT EXISTS user_identity(
    account INTEGER NOT NULL, user_id INTEGER NOT NULL, name TEXT NOT NULL, identity TEXT NOT NULL,
    PRIMARY KEY(account, user_id));";

/// Saved topic scenes.
#[derive(Debug, PartialEq, Eq)]
pub struct TopicScene {
    pub account_id: i64,
    pub stream_id: i64,
    pub topic: String,
    pub name: String,
    pub scene: String,
}

/// Saved friend identities: `identity` is "scene id/relationship id".
#[derive(Debug, PartialEq, Eq)]
pub struct UserIdentity {
    pub account_id: i64,
    pub user_id: i64,
    pub name: String,
    pub identity: String,
}

fn select_topic_scene(
    db: &Connection,
    account: i64,
    stream: i64,
    topic: &str,
) -> rusqlite::Result<Option<String>> {
    db.query_row(
        "SELECT scene FROM topic_scene WHERE account = ?1 AND stream = ?2 AND topic = ?3",
        params![account, stream, topic],
        |row| row.get(0),
    )
    .optional()
}

// Deleted when scene is empty; the original name is kept when name is empty.
fn store_topic_scene(
    db: &Connection,
    account: i64,
    stream: i64,
    topic: &str,
    name: &str,
    scene: &str,
) -> rusqlite::Result<()> {
    if scene.is_empty() {
        db.execute(
            "DELETE FROM topic_scene WHERE account = ?1 AND stream = ?2 AND topic = ?3",
            params![account, stream, topic],
        )?;
    } else {
        db.execute(
            "INSERT INTO topic_scene(account, stream, topic, name, scene) VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(account, stream, topic) DO UPDATE SET scene = excluded.scene,
                 name = CASE WHEN excluded.name = '' THEN topic_scene.name ELSE excluded.name END",
            params![account, stream, topic, name, scene],
        )?;
    }
    Ok(())
}

fn select_topic_scenes(db: &Connection) -> rusqlite::Result<Vec<TopicScene>> {
    db.prepare("SELECT account, stream, topic, name, scene FROM topic_scene ORDER BY name")?
        .query_map([], |row| {
            Ok(TopicScene {
                account_id: row.get(0)?,
                stream_id: row.get(1)?,
                topic: row.get(2)?,
                name: row.get(3)?,
                scene: row.get(4)?,
            })
        })?
        .collect()
}

fn select_identity(db: &Connection, account: i64, user: i64) -> rusqlite::Result<Option<String>> {
    db.query_row(
        "SELECT identity FROM user_identity WHERE account = ?1 AND user_id = ?2",
        params![account, user],
        |row| row.get(0),
    )
    .optional()
}

// Deleted when identity is empty; the original name is kept when name is empty.
fn store_identity(
    db: &Connection,
    account: i64,
    user: i64,
    name: &str,
    identity: &str,
) -> rusqlite::Result<()> {
    if identity.is_empty() {
        db.execute(
            "DELETE FROM user_identity WHERE account = ?1 AND user_id = ?2",
            params![account, user],
        )?;
    } else {
        db.execute(
            "INSERT INTO user_identity(account, user_id, name, identity) VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(account, user_id) DO UPDATE SET identity = excluded.identity,
                 name = CASE WHEN excluded.name = '' THEN user_identity.name ELSE excluded.name END",
            params![account, user, name, identity],
        )?;
    }
    Ok(())
}

fn select_identities(db: &Connection) -> rusqlite::Result<Vec<UserIdentity>> {
    db.prepare("SELECT account, user_id, name, identity FROM user_identity ORDER BY name")?
        .query_map([], |row| {
            Ok(UserIdentity {
                account_id: row.get(0)?,
                user_id: row.get(1)?,
                name: row.get(2)?,
                identity: row.get(3)?,
            })
        })?
        .collect()
}

fn database() -> Option<&'static AnnotationStore> {
    static DB: OnceLock<Option<AnnotationStore>> = OnceLock::new();
    DB.get_or_init(|| AnnotationStore::open(DATABASE_FILE, ANALYSIS_TABLE, SCHEMA))
        .as_ref()
}

// On read failure, writes to the log and returns the default.
fn read<T: Default>(query: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> T {
    let Some(db) = database() else {
        return T::default();
    };
    db.with(query).unwrap_or_else(|error| {
        log(LOG_WARNING, &format!("读取数据失败：{error}"));
        T::default()
    })
}

fn write(update: impl FnOnce(&Connection) -> rusqlite::Result<()>) {
    if let Some(db) = database()
        && let Err(error) = db.with(update)
    {
        log(LOG_WARNING, &format!("保存数据失败：{error}"));
    }
}

pub fn get_topic_scene(account: i64, stream: i64, topic: &str) -> Option<String> {
    read(|db| select_topic_scene(db, account, stream, topic))
}

pub fn set_topic_scene(account: i64, stream: i64, topic: &str, name: &str, scene: &str) {
    write(|db| store_topic_scene(db, account, stream, topic, name, scene));
}

pub fn topic_scenes() -> Vec<TopicScene> {
    read(select_topic_scenes)
}

pub fn get_identity(account: i64, user: i64) -> Option<String> {
    read(|db| select_identity(db, account, user))
}

pub fn set_identity(account: i64, user: i64, name: &str, identity: &str) {
    write(|db| store_identity(db, account, user, name, identity));
}

pub fn identities() -> Vec<UserIdentity> {
    read(select_identities)
}

pub fn set_analysis(account: i64, message_id: i64, annotation: &str) {
    if let Some(db) = database()
        && let Err(error) = db.save(account, message_id, annotation)
    {
        log(LOG_WARNING, &format!("保存数据失败：{error}"));
    }
}

/// Among these messages, those with saved analysis results: (message id, JSON of the attached
/// content).
pub fn analyses(account: i64, message_ids: &[i64]) -> Vec<(i64, String)> {
    if message_ids.is_empty() {
        return Vec::new();
    }
    let Some(db) = database() else {
        return Vec::new();
    };
    db.load(account, message_ids).unwrap_or_else(|error| {
        log(LOG_WARNING, &format!("读取数据失败：{error}"));
        Vec::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(SCHEMA).unwrap();
        db
    }

    #[test]
    fn topic_scenes_keep_name_and_ignore_topic_case() {
        let db = memory();
        store_topic_scene(&db, 1, 2, "发布 A", "开发 / 发布", "work").unwrap();
        store_topic_scene(&db, 1, 2, "发布 a", "", "family").unwrap();
        assert_eq!(
            select_topic_scene(&db, 1, 2, "发布 A").unwrap().as_deref(),
            Some("family")
        );
        assert_eq!(select_topic_scenes(&db).unwrap()[0].name, "开发 / 发布");
        store_topic_scene(&db, 1, 2, "发布 A", "", "").unwrap();
        assert_eq!(select_topic_scene(&db, 1, 2, "发布 A").unwrap(), None);
    }

    #[test]
    fn identities_are_per_account() {
        let db = memory();
        store_identity(&db, 1, 5, "张三", "work/peer").unwrap();
        store_identity(&db, 2, 5, "张三", "family/sibling_or_peer").unwrap();
        store_identity(&db, 1, 5, "", "work/senior").unwrap();
        assert_eq!(
            select_identity(&db, 1, 5).unwrap().as_deref(),
            Some("work/senior")
        );
        assert_eq!(select_identities(&db).unwrap().len(), 2);
        store_identity(&db, 1, 5, "", "").unwrap();
        assert_eq!(select_identity(&db, 1, 5).unwrap(), None);
    }

    #[test]
    fn analyses_are_replaced() {
        let store = AnnotationStore::open_in_memory(ANALYSIS_TABLE, SCHEMA).unwrap();
        store.save(1, 10, "旧").unwrap();
        store.save(1, 10, "新").unwrap();
        store.save(2, 11, "别的账号").unwrap();
        assert_eq!(
            store.load(1, &[10, 11]).unwrap(),
            vec![(10, "新".to_owned())]
        );
    }
}
