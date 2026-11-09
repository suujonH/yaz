use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use rusqlite::types::Value;
use rusqlite::{Connection, Params, Row, ToSql, params_from_iter};

use crate::error::{Result, StoreError};

/// 迁移脚本，下标 + 1 即版本号。已发布的脚本不能修改，只能追加。
///
/// 每个账号的数据都以外键挂在 `account` 上，删除账号时级联删除。
/// `conversation_cache` 是缓存，可以随时丢弃，由 Zulip 上的消息重建；其余表是只保存在本机的数据。
const MIGRATIONS: &[&str] = &[r#"
CREATE TABLE account(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  realm_url TEXT NOT NULL,
  email TEXT NOT NULL,
  user_id INTEGER NOT NULL,
  full_name TEXT NOT NULL,
  avatar_url TEXT,
  api_key BLOB,
  allow_invalid_cert INTEGER NOT NULL DEFAULT 0,
  login_on_startup INTEGER NOT NULL DEFAULT 1,
  last_login_at INTEGER NOT NULL,
  in_list INTEGER NOT NULL DEFAULT 0,
  list_order INTEGER NOT NULL DEFAULT 0,
  UNIQUE(realm_url, email));
CREATE TABLE setting(
  account_id INTEGER REFERENCES account(id) ON DELETE CASCADE,
  key TEXT NOT NULL,
  value TEXT NOT NULL);
CREATE UNIQUE INDEX setting_owner_key ON setting(COALESCE(account_id, 0), key);
CREATE TABLE conversation_pref(
  account_id INTEGER NOT NULL REFERENCES account(id) ON DELETE CASCADE,
  conv_key TEXT NOT NULL,
  dnd_level INTEGER,
  in_list INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(account_id, conv_key));
CREATE TABLE conversation_cache(
  account_id INTEGER NOT NULL REFERENCES account(id) ON DELETE CASCADE,
  conv_key TEXT NOT NULL,
  title TEXT NOT NULL,
  last_message_id INTEGER NOT NULL,
  last_message_time INTEGER NOT NULL,
  last_sender_id INTEGER NOT NULL,
  last_preview TEXT NOT NULL,
  PRIMARY KEY(account_id, conv_key));
CREATE TABLE remark(
  account_id INTEGER NOT NULL REFERENCES account(id) ON DELETE CASCADE,
  target_kind TEXT NOT NULL,
  target_id TEXT NOT NULL,
  remark TEXT NOT NULL,
  PRIMARY KEY(account_id, target_kind, target_id));
CREATE TABLE pin(
  account_id INTEGER NOT NULL REFERENCES account(id) ON DELETE CASCADE,
  scope TEXT NOT NULL,
  item_key TEXT NOT NULL,
  pinned_at INTEGER NOT NULL,
  PRIMARY KEY(account_id, scope, item_key));
CREATE TABLE contact_group(
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  account_id INTEGER NOT NULL REFERENCES account(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  sort_order INTEGER NOT NULL);
CREATE TABLE contact_group_member(
  account_id INTEGER NOT NULL REFERENCES account(id) ON DELETE CASCADE,
  user_id INTEGER NOT NULL,
  group_id INTEGER NOT NULL REFERENCES contact_group(id) ON DELETE CASCADE,
  PRIMARY KEY(account_id, user_id));
CREATE TABLE downloaded_file(
  account_id INTEGER NOT NULL REFERENCES account(id) ON DELETE CASCADE,
  upload_path TEXT NOT NULL,
  local_path TEXT NOT NULL,
  file_name TEXT NOT NULL,
  size INTEGER NOT NULL,
  message_id INTEGER NOT NULL,
  conv_key TEXT NOT NULL,
  downloaded_at INTEGER NOT NULL,
  last_opened_at INTEGER,
  PRIMARY KEY(account_id, upload_path));
CREATE TABLE local_draft(
  account_id INTEGER NOT NULL REFERENCES account(id) ON DELETE CASCADE,
  conv_key TEXT NOT NULL,
  content TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY(account_id, conv_key));
CREATE TABLE message_version(
  account_id INTEGER NOT NULL REFERENCES account(id) ON DELETE CASCADE,
  message_id INTEGER NOT NULL,
  edited_at INTEGER NOT NULL,
  content TEXT NOT NULL,
  PRIMARY KEY(account_id, message_id, edited_at));
CREATE TABLE deleted_message(
  account_id INTEGER NOT NULL REFERENCES account(id) ON DELETE CASCADE,
  message_id INTEGER NOT NULL,
  conv_key TEXT NOT NULL,
  sender_id INTEGER NOT NULL,
  sender_name TEXT NOT NULL,
  avatar_url TEXT,
  content TEXT NOT NULL,
  timestamp INTEGER NOT NULL,
  last_edit_timestamp INTEGER,
  is_me_message INTEGER NOT NULL,
  topic TEXT NOT NULL,
  PRIMARY KEY(account_id, message_id));
CREATE TABLE plugin(
  plugin_id TEXT PRIMARY KEY,
  enabled INTEGER NOT NULL DEFAULT 0,
  fingerprint TEXT,
  approved_at INTEGER);
"#];

/// 当前的数据库版本。
pub const SCHEMA_VERSION: i32 = MIGRATIONS.len() as i32;

/// 设置的所有者（`None` 为程序级）→ 键 → 值。
type SettingCache = HashMap<Option<i64>, HashMap<String, String>>;
type Job = Box<dyn FnOnce() + Send>;

struct Shared {
    conn: Mutex<Connection>,
    settings: RwLock<SettingCache>,
    worker_id: ThreadId,
}

/// 专用线程。最后一个句柄释放时等待排队的操作执行完，之后数据库文件才可以移动。
struct Worker {
    jobs: Mutex<Option<Sender<Job>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.jobs.lock().take();
        if let Some(thread) = self.thread.lock().take()
            && thread.thread().id() != thread::current().id()
        {
            let _ = thread.join();
        }
    }
}

/// 本地 SQLite 数据库，只保存 Zulip 没有的数据。可以廉价地 `clone`，克隆体共用同一个连接。
///
/// 所有写操作按顺序放到专用线程上执行（[`write`](Self::write)、[`submit`](Self::submit)），
/// 不阻塞调用方，可以在持有任何锁时调用；失败时只写日志。
/// 读操作有三种：异步代码用 [`call`](Self::call) 放到专用线程上执行；启动和界面线程上的少量读取
/// 直接调用仓储的方法，它们先等待排队的写操作完成；设置（[`SettingRepo`](crate::SettingRepo)）在内存中。
#[derive(Clone)]
pub struct Db {
    // 在 shared 之前释放：线程退出后才关闭连接。专用线程上执行的操作拿到的 Db 没有它。
    worker: Option<Arc<Worker>>,
    shared: Arc<Shared>,
}

impl Db {
    /// 打开数据库（目录不存在时创建）并执行尚未执行的迁移。
    pub fn open(path: &Path) -> Result<Self> {
        let db = Self::open_with(path, MIGRATIONS)?;
        db.reload_settings()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        let db = Self::init(Connection::open_in_memory()?, MIGRATIONS)?;
        db.reload_settings()?;
        Ok(db)
    }

    pub(crate) fn open_with(path: &Path, migrations: &[&str]) -> Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        Self::init(Connection::open(path)?, migrations)
    }

    fn init(mut conn: Connection, migrations: &[&str]) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        migrate(&mut conn, migrations)?;

        let (sender, receiver) = mpsc::channel::<Job>();
        let thread = thread::Builder::new()
            .name("zulip-store".to_owned())
            .spawn(move || {
                while let Ok(job) = receiver.recv() {
                    job();
                }
            })?;
        Ok(Self {
            shared: Arc::new(Shared {
                conn: Mutex::new(conn),
                settings: RwLock::default(),
                worker_id: thread.thread().id(),
            }),
            worker: Some(Arc::new(Worker {
                jobs: Mutex::new(Some(sender)),
                thread: Mutex::new(Some(thread)),
            })),
        })
    }

    pub fn user_version(&self) -> Result<i32> {
        self.with(|conn| conn.pragma_query_value(None, "user_version", |r| r.get(0)))
    }

    fn enqueue(&self, job: Job) {
        match &self.worker {
            Some(worker) => {
                if let Some(jobs) = &*worker.jobs.lock() {
                    let _ = jobs.send(job);
                }
            }
            // 已经在专用线程上
            None => job(),
        }
    }

    /// 交给专用线程上的操作使用的句柄。
    fn on_worker(&self) -> Self {
        Self {
            worker: None,
            shared: self.shared.clone(),
        }
    }

    /// 排队执行 `f`，立即返回。失败时写日志。
    pub fn submit(&self, f: impl FnOnce(&Db) -> Result<()> + Send + 'static) {
        let db = self.on_worker();
        self.enqueue(Box::new(move || {
            if let Err(error) = f(&db) {
                log::warn!("写入数据库失败：{error}");
            }
        }));
    }

    /// 排队执行一条写语句，立即返回。失败时写日志。
    pub(crate) fn write(&self, sql: &'static str, params: &[&dyn ToSql]) {
        let mut values = Vec::with_capacity(params.len());
        for param in params {
            match param.to_sql() {
                Ok(output) => values.push(owned_value(output)),
                Err(error) => {
                    log::warn!("写入数据库失败：{error}");
                    return;
                }
            }
        }
        self.submit(move |db| {
            db.execute(sql, params_from_iter(values))?;
            Ok(())
        });
    }

    /// 在专用线程上执行 `f`（排在已提交的写操作之后）并等待结果。
    pub async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Db) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let db = self.on_worker();
        self.enqueue(Box::new(move || {
            let _ = sender.send(f(&db));
        }));
        receiver.await.map_err(|_| StoreError::Closed)?
    }

    /// 等待已提交的写操作完成。退出程序前调用。
    pub fn flush(&self) {
        if thread::current().id() == self.shared.worker_id {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.enqueue(Box::new(move || {
            let _ = sender.send(());
        }));
        let _ = receiver.recv();
    }

    /// 持有连接执行 `f`（等待已提交的写操作完成）。`f` 中不能再调用 `Db` 的方法。
    pub(crate) fn with<R>(
        &self,
        f: impl FnOnce(&mut Connection) -> rusqlite::Result<R>,
    ) -> Result<R> {
        self.flush();
        Ok(f(&mut self.shared.conn.lock())?)
    }

    pub(crate) fn execute(&self, sql: &str, params: impl Params) -> Result<usize> {
        self.with(|conn| conn.prepare_cached(sql)?.execute(params))
    }

    pub(crate) fn query<T>(
        &self,
        sql: &str,
        params: impl Params,
        map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>> {
        self.with(|conn| conn.prepare_cached(sql)?.query_map(params, map)?.collect())
    }

    /// 第一行；没有结果时为 `None`。
    pub(crate) fn query_one<T>(
        &self,
        sql: &str,
        params: impl Params,
        map: impl FnOnce(&Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<Option<T>> {
        use rusqlite::OptionalExtension;
        self.with(|conn| conn.prepare_cached(sql)?.query_row(params, map).optional())
    }

    pub(crate) fn settings(&self) -> &RwLock<SettingCache> {
        &self.shared.settings
    }

    /// 从数据库重新读取全部设置到内存。
    pub(crate) fn reload_settings(&self) -> Result<()> {
        let rows = self.query("SELECT account_id, key, value FROM setting", [], |r| {
            Ok((
                r.get::<_, Option<i64>>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut cache = SettingCache::new();
        for (owner, key, value) in rows {
            cache.entry(owner).or_default().insert(key, value);
        }
        *self.shared.settings.write() = cache;
        Ok(())
    }
}

fn owned_value(output: rusqlite::types::ToSqlOutput<'_>) -> Value {
    use rusqlite::types::ToSqlOutput;
    match output {
        ToSqlOutput::Borrowed(value) => value.into(),
        ToSqlOutput::Owned(value) => value,
        _ => Value::Null,
    }
}

fn migrate(conn: &mut Connection, migrations: &[&str]) -> rusqlite::Result<()> {
    let current: usize = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    // 自动更新回滚后，旧版程序仍要能用新版留下的数据库启动
    if current > migrations.len() {
        log::warn!(
            "数据库版本 {current} 高于程序支持的版本 {}，按现有结构继续运行",
            migrations.len()
        );
    }
    for (index, script) in migrations.iter().enumerate().skip(current) {
        let tx = conn.transaction()?;
        tx.execute_batch(script)?;
        tx.pragma_update(None, "user_version", index + 1)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_sets_version_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("test.db");
        let db = Db::open(&path).unwrap();
        assert_eq!(db.user_version().unwrap(), SCHEMA_VERSION);
        drop(db);
        let again = Db::open(&path).unwrap();
        assert_eq!(again.user_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn pending_migrations_run_once_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let first = ["CREATE TABLE a(x INTEGER);"];
        let second = [
            "CREATE TABLE a(x INTEGER);",
            "ALTER TABLE a ADD COLUMN y INTEGER DEFAULT 5;",
        ];

        let db = Db::open_with(&path, &first).unwrap();
        db.execute("INSERT INTO a(x) VALUES(1)", []).unwrap();
        assert_eq!(db.user_version().unwrap(), 1);
        drop(db);

        let db = Db::open_with(&path, &second).unwrap();
        assert_eq!(db.user_version().unwrap(), 2);
        assert_eq!(
            db.query("SELECT x, y FROM a", [], |r| Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?
            )))
            .unwrap(),
            [(1, 5)]
        );
        drop(db);

        let db = Db::open_with(&path, &second).unwrap();
        assert_eq!(db.user_version().unwrap(), 2);
    }

    #[test]
    fn newer_database_still_opens_and_keeps_its_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let newer = ["CREATE TABLE a(x INTEGER);", "CREATE TABLE b(y INTEGER);"];
        let db = Db::open_with(&path, &newer).unwrap();
        db.execute("INSERT INTO a(x) VALUES(1)", []).unwrap();
        drop(db);

        let db = Db::open_with(&path, &newer[..1]).unwrap();
        assert_eq!(db.user_version().unwrap(), 2);
        assert_eq!(
            db.query("SELECT x FROM a", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            [1]
        );
    }

    #[test]
    fn failed_migration_leaves_version_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        assert!(Db::open_with(&path, &["CREATE TABLE a(x);", "THIS IS NOT SQL;"]).is_err());
        let db = Db::open_with(&path, &["CREATE TABLE a(x);"]).unwrap();
        assert_eq!(db.user_version().unwrap(), 1);
    }

    #[test]
    fn file_database_uses_wal() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        let mode: String = db
            .with(|c| c.pragma_query_value(None, "journal_mode", |r| r.get(0)))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn writes_run_in_order_and_reads_see_them() {
        let db = Db::open_with(
            &tempfile::tempdir().unwrap().path().join("t.db"),
            &["CREATE TABLE a(x INTEGER);"],
        )
        .unwrap();
        for i in 0..100 {
            db.write("INSERT INTO a(x) VALUES(?1)", rusqlite::params![i]);
        }
        db.write("DELETE FROM a WHERE x < ?1", rusqlite::params![99]);
        assert_eq!(
            db.query("SELECT x FROM a", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            [99]
        );
    }

    #[test]
    fn failed_write_only_logs() {
        let db = Db::open_in_memory().unwrap();
        // 账号不存在，外键失败
        db.write(
            "INSERT INTO local_draft(account_id, conv_key, content, updated_at) VALUES(9, 'k', 'c', 1)",
            &[],
        );
        db.flush();
        assert_eq!(db.user_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn call_runs_on_the_worker_after_queued_writes() {
        let db = Db::open_with(
            &tempfile::tempdir().unwrap().path().join("t.db"),
            &["CREATE TABLE a(x INTEGER);"],
        )
        .unwrap();
        db.write("INSERT INTO a(x) VALUES(?1)", rusqlite::params![7]);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let (rows, thread) = runtime
            .block_on(db.call(|db| {
                Ok((
                    db.query("SELECT x FROM a", [], |r| r.get::<_, i64>(0))?,
                    std::thread::current().id(),
                ))
            }))
            .unwrap();
        assert_eq!(rows, [7]);
        assert_ne!(thread, std::thread::current().id());
    }

    #[test]
    fn dropping_the_last_handle_releases_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let db = Db::open(&path).unwrap();
        db.write("PRAGMA user_version", &[]);
        drop(db);
        std::fs::rename(&path, dir.path().join("moved.db")).unwrap();
    }

    #[test]
    fn deleting_account_cascades_to_every_account_table() {
        let db = Db::open_in_memory().unwrap();
        let tables: Vec<(String, i64)> = db
            .query(
                "SELECT m.name, p.\"notnull\" FROM sqlite_master m, pragma_table_info(m.name) p
                 WHERE m.type = 'table' AND p.name = 'account_id'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(tables.len() >= 10, "{tables:?}");
        let account = crate::account::tests::add_account(&db);
        db.execute(
            "INSERT INTO contact_group(id, account_id, name, sort_order) VALUES(77, ?1, 'x', 1)",
            [account],
        )
        .unwrap();
        for (table, _) in &tables {
            if table == "contact_group" {
                continue;
            }
            // 该账号的一行：各列填上类型对应的任意值
            let columns: Vec<(String, String)> = db
                .query(&format!("PRAGMA table_info(\"{table}\")"), [], |r| {
                    Ok((r.get(1)?, r.get(2)?))
                })
                .unwrap();
            let names: Vec<_> = columns.iter().map(|(n, _)| format!("\"{n}\"")).collect();
            let values: Vec<_> = columns
                .iter()
                .map(|(name, kind)| match (name.as_str(), kind.as_str()) {
                    ("account_id", _) => account.to_string(),
                    ("group_id", _) => "77".to_owned(),
                    (_, "TEXT") => "'x'".to_owned(),
                    _ => "1".to_owned(),
                })
                .collect();
            db.execute(
                &format!(
                    "INSERT INTO \"{table}\"({}) VALUES({})",
                    names.join(","),
                    values.join(",")
                ),
                [],
            )
            .unwrap();
        }

        crate::AccountRepo::new(&db).delete_with_data(account);
        for (table, _) in &tables {
            let count: i64 = db
                .with(|c| {
                    c.query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |r| {
                        r.get(0)
                    })
                })
                .unwrap();
            assert_eq!(count, 0, "{table}");
        }
    }
}
