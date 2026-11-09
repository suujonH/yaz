use rusqlite::{Row, params};

use crate::db::Db;
use crate::error::Result;

/// 保存的登录账号。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRow {
    pub id: i64,
    pub realm_url: String,
    pub email: String,
    pub user_id: i64,
    pub full_name: String,
    pub avatar_url: Option<String>,
    /// [`protect_key`](crate::protect_key) 加密后的数据；`None` 表示已退出，需要重新输入密码。
    pub api_key: Option<Vec<u8>>,
    pub allow_invalid_cert: bool,
    /// “启动”（启用）：开启时登录该账号，启动程序时也登录（账号需在列表中且保存了 key）。
    pub login_on_startup: bool,
    /// Unix 秒。
    pub last_login_at: i64,
}

impl AccountRow {
    pub fn has_key(&self) -> bool {
        self.api_key.is_some()
    }
}

const COLUMNS: &str = "id, realm_url, email, user_id, full_name, avatar_url, api_key, allow_invalid_cert, login_on_startup, last_login_at";

fn map(r: &Row<'_>) -> rusqlite::Result<AccountRow> {
    Ok(AccountRow {
        id: r.get(0)?,
        realm_url: r.get(1)?,
        email: r.get(2)?,
        user_id: r.get(3)?,
        full_name: r.get(4)?,
        avatar_url: r.get(5)?,
        api_key: r.get(6)?,
        allow_invalid_cert: r.get(7)?,
        login_on_startup: r.get(8)?,
        last_login_at: r.get(9)?,
    })
}

pub struct AccountRepo {
    db: Db,
}

impl AccountRepo {
    pub fn new(db: &Db) -> Self {
        Self { db: db.clone() }
    }

    /// 按最近登录时间降序。
    pub fn get_all(&self) -> Result<Vec<AccountRow>> {
        self.db.query(
            &format!("SELECT {COLUMNS} FROM account ORDER BY last_login_at DESC, id DESC"),
            [],
            map,
        )
    }

    pub fn find(&self, realm_url: &str, email: &str) -> Result<Option<AccountRow>> {
        self.db.query_one(
            &format!("SELECT {COLUMNS} FROM account WHERE realm_url = ?1 AND email = ?2"),
            params![realm_url, email],
            map,
        )
    }

    pub fn get(&self, id: i64) -> Result<Option<AccountRow>> {
        self.db.query_one(
            &format!("SELECT {COLUMNS} FROM account WHERE id = ?1"),
            [id],
            map,
        )
    }

    /// 按 (realm_url, email) 插入或更新资料和 key，返回行 id。
    /// `id` 字段忽略；已有的账号不改变“启动”和在账号列表中的位置。
    pub fn upsert(&self, row: &AccountRow) -> Result<i64> {
        self.db.with(|conn| {
            conn.prepare_cached(
                "INSERT INTO account(realm_url, email, user_id, full_name, avatar_url, api_key, allow_invalid_cert, login_on_startup, last_login_at)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(realm_url, email) DO UPDATE SET
                   user_id = excluded.user_id, full_name = excluded.full_name, avatar_url = excluded.avatar_url,
                   api_key = excluded.api_key, allow_invalid_cert = excluded.allow_invalid_cert,
                   last_login_at = excluded.last_login_at
                 RETURNING id",
            )?
            .query_row(
                params![
                    row.realm_url, row.email, row.user_id, row.full_name, row.avatar_url, row.api_key,
                    row.allow_invalid_cert, row.login_on_startup, row.last_login_at
                ],
                |r| r.get(0),
            )
        })
    }

    /// register 成功后用服务器返回的资料更新，并记录登录时间。
    pub fn update_profile(
        &self,
        id: i64,
        user_id: i64,
        full_name: &str,
        avatar_url: Option<&str>,
        last_login_at: i64,
    ) {
        self.db.write(
            "UPDATE account SET user_id = ?2, full_name = ?3, avatar_url = ?4, last_login_at = ?5 WHERE id = ?1",
            params![id, user_id, full_name, avatar_url, last_login_at],
        );
    }

    pub fn set_allow_invalid_cert(&self, id: i64, allow: bool) {
        self.db.write(
            "UPDATE account SET allow_invalid_cert = ?2 WHERE id = ?1",
            params![id, allow],
        );
    }

    pub fn set_login_on_startup(&self, id: i64, enabled: bool) {
        self.db.write(
            "UPDATE account SET login_on_startup = ?2 WHERE id = ?1",
            params![id, enabled],
        );
    }

    /// 主窗口账号列表中的账号（登录中的账号），按加入列表的顺序；第一个是主账号。
    pub fn get_listed(&self) -> Result<Vec<AccountRow>> {
        self.db.query(
            &format!("SELECT {COLUMNS} FROM account WHERE in_list = 1 ORDER BY list_order, id"),
            [],
            map,
        )
    }

    /// 登录成功：不在列表中时加到列表末尾，已在列表中时位置不变。
    pub fn add_to_list(&self, id: i64) {
        self.db.write(
            "UPDATE account SET in_list = 1,
               list_order = (SELECT COALESCE(MAX(list_order), 0) + 1 FROM account)
             WHERE id = ?1 AND in_list = 0",
            params![id],
        );
    }

    /// 退出账号：移出列表，下次启动不再登录。
    pub fn remove_from_list(&self, id: i64) {
        self.db
            .write("UPDATE account SET in_list = 0 WHERE id = ?1", params![id]);
    }

    /// 启动程序时登录的账号：列表中开启了“启动”且保存了 key 的账号。没有时显示登录窗。
    pub fn get_startup_accounts(&self) -> Result<Vec<AccountRow>> {
        self.db.query(
            &format!(
                "SELECT {COLUMNS} FROM account
                 WHERE in_list = 1 AND login_on_startup = 1 AND api_key IS NOT NULL
                 ORDER BY list_order, id"
            ),
            [],
            map,
        )
    }

    /// 退出登录：清除保存的 key；服务器地址与邮箱保留，下次登录时关联已有的本地数据。
    pub fn clear_key(&self, id: i64) {
        self.db.write(
            "UPDATE account SET api_key = NULL WHERE id = ?1",
            params![id],
        );
    }

    /// 删除账号及其所有本地数据（外键级联）。
    pub fn delete_with_data(&self, id: i64) {
        self.db.settings().write().remove(&Some(id));
        self.db
            .write("DELETE FROM account WHERE id = ?1", params![id]);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::draft::LocalDraftRepo;
    use crate::secret::{protect_key, unprotect_key};
    use crate::setting::SettingRepo;

    /// 外键要求数据属于一个存在的账号。
    pub(crate) fn add_named_account(db: &Db, email: &str) -> i64 {
        AccountRepo::new(db)
            .upsert(&row("https://test/", email, b"k", false, 1))
            .unwrap()
    }

    pub(crate) fn add_account(db: &Db) -> i64 {
        add_named_account(db, "me@test")
    }

    fn row(realm: &str, email: &str, key: &[u8], cert: bool, last: i64) -> AccountRow {
        AccountRow {
            id: 0,
            realm_url: realm.into(),
            email: email.into(),
            user_id: 0,
            full_name: String::new(),
            avatar_url: None,
            api_key: Some(key.to_vec()),
            allow_invalid_cert: cert,
            login_on_startup: true,
            last_login_at: last,
        }
    }

    #[test]
    fn round_trip() {
        let db = Db::open_in_memory().unwrap();
        let repo = AccountRepo::new(&db);
        let key = protect_key("secret-key").unwrap();
        let id = repo
            .upsert(&row("https://a/", "x@a", &key, true, 100))
            .unwrap();
        repo.upsert(&row("https://b/", "y@b", &key, false, 200))
            .unwrap();

        let all = repo.get_all().unwrap();
        assert_eq!(
            all.iter().map(|a| a.email.as_str()).collect::<Vec<_>>(),
            ["y@b", "x@a"]
        );
        let found = repo.find("https://a/", "x@a").unwrap().unwrap();
        assert_eq!(
            unprotect_key(&found.api_key.unwrap()).unwrap(),
            "secret-key"
        );
        assert!(repo.find("https://a/", "nobody").unwrap().is_none());

        repo.update_profile(id, 8, "群主", Some("/avatar.png"), 300);
        let updated = repo.get(id).unwrap().unwrap();
        assert_eq!(
            (
                updated.user_id,
                updated.full_name.as_str(),
                updated.avatar_url.as_deref(),
                updated.last_login_at
            ),
            (8, "群主", Some("/avatar.png"), 300)
        );

        // 重复登录同一账号更新原行
        let renamed = AccountRow {
            full_name: "新名字".into(),
            ..updated
        };
        assert_eq!(repo.upsert(&renamed).unwrap(), id);
        assert_eq!(repo.get_all().unwrap().len(), 2);
        assert_eq!(repo.get(id).unwrap().unwrap().full_name, "新名字");
    }

    #[test]
    fn startup_accounts_and_delete_with_data() {
        let db = Db::open_in_memory().unwrap();
        let repo = AccountRepo::new(&db);
        let key = protect_key("k").unwrap();
        let a = repo
            .upsert(&row("https://a/", "x@a", &key, true, 100))
            .unwrap();
        let b = repo
            .upsert(&row("https://b/", "y@b", &key, false, 200))
            .unwrap();
        repo.add_to_list(a);
        repo.add_to_list(b);
        let ids = |rows: Vec<AccountRow>| rows.into_iter().map(|r| r.id).collect::<Vec<_>>();

        // “启动”默认开启；关闭后启动时不登录，重新登录不改变它
        assert_eq!(ids(repo.get_startup_accounts().unwrap()), [a, b]);
        repo.set_login_on_startup(a, false);
        let off = repo.get(a).unwrap().unwrap();
        assert!(!off.login_on_startup);
        assert_eq!(
            repo.upsert(&AccountRow {
                login_on_startup: true,
                ..off
            })
            .unwrap(),
            a
        );
        assert!(!repo.get(a).unwrap().unwrap().login_on_startup);
        assert_eq!(ids(repo.get_startup_accounts().unwrap()), [b]);
        repo.set_login_on_startup(a, true);

        // 退出登录：保留账号，清除 key；没有 key 的账号启动时不登录
        repo.clear_key(a);
        let cleared = repo.get(a).unwrap().unwrap();
        assert!(!cleared.has_key());
        assert_eq!(ids(repo.get_startup_accounts().unwrap()), [b]);
        // 重新登录（upsert）后关联同一个账号
        assert_eq!(
            repo.upsert(&AccountRow {
                api_key: Some(key),
                ..cleared
            })
            .unwrap(),
            a
        );
        assert!(repo.get(a).unwrap().unwrap().has_key());

        // 删除所有数据：只删除该账号的行
        SettingRepo::account(&db, a).set("k", "v");
        SettingRepo::account(&db, b).set("k", "v");
        LocalDraftRepo::new(&db, a).set("t:4:x", "草稿", 1);
        repo.delete_with_data(a);
        assert!(repo.get(a).unwrap().is_none());
        assert_eq!(SettingRepo::account(&db, a).get("k"), None);
        assert!(LocalDraftRepo::new(&db, a).load_all().unwrap().is_empty());
        assert_eq!(SettingRepo::account(&db, b).get("k").as_deref(), Some("v"));
        assert!(repo.get(b).unwrap().is_some());
    }

    #[test]
    fn list_keeps_login_order() {
        let db = Db::open_in_memory().unwrap();
        let repo = AccountRepo::new(&db);
        let key = protect_key("k").unwrap();
        let a = repo
            .upsert(&row("https://a/", "x@a", &key, false, 100))
            .unwrap();
        let b = repo
            .upsert(&row("https://b/", "y@b", &key, false, 200))
            .unwrap();
        let c = repo
            .upsert(&row("https://c/", "z@c", &key, false, 300))
            .unwrap();
        let listed = || {
            repo.get_listed()
                .unwrap()
                .into_iter()
                .map(|r| r.id)
                .collect::<Vec<_>>()
        };
        assert!(listed().is_empty());

        repo.add_to_list(b);
        repo.add_to_list(a);
        // 已在列表中的账号再次登录，位置不变
        repo.add_to_list(b);
        assert_eq!(listed(), [b, a]);

        // 退出后再登录排到最后；重新登录（upsert）不改变位置
        repo.remove_from_list(b);
        repo.add_to_list(c);
        repo.add_to_list(b);
        let relogin = AccountRow {
            full_name: "新名字".into(),
            ..repo.get(a).unwrap().unwrap()
        };
        repo.upsert(&relogin).unwrap();
        assert_eq!(listed(), [a, c, b]);

        repo.set_allow_invalid_cert(c, true);
        assert!(repo.get(c).unwrap().unwrap().allow_invalid_cert);
    }
}
