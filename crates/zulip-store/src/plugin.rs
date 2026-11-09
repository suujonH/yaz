use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::params;

use crate::db::Db;
use crate::error::Result;

/// 插件的本地记录（程序级）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginRecord {
    pub enabled: bool,
    /// 用户确认开启时的文件指纹；从未确认时为 `None`。
    pub fingerprint: Option<String>,
}

/// 没有记录的插件默认停用；只有启用且确认时的指纹与当前文件一致的插件才会加载。
pub struct PluginRepo {
    db: Db,
}

impl PluginRepo {
    pub fn new(db: &Db) -> Self {
        Self { db: db.clone() }
    }

    pub fn load_all(&self) -> Result<HashMap<String, PluginRecord>> {
        let rows = self.db.query(
            "SELECT plugin_id, enabled, fingerprint FROM plugin",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    PluginRecord {
                        enabled: r.get(1)?,
                        fingerprint: r.get(2)?,
                    },
                ))
            },
        )?;
        Ok(rows.into_iter().collect())
    }

    pub fn set_enabled(&self, plugin_id: &str, enabled: bool) {
        self.db.write(
            "INSERT INTO plugin(plugin_id, enabled) VALUES(?1, ?2)
             ON CONFLICT(plugin_id) DO UPDATE SET enabled = excluded.enabled",
            params![plugin_id, enabled],
        );
    }

    /// 用户确认开启：记录指纹并启用。
    pub fn approve(&self, plugin_id: &str, fingerprint: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        self.db.write(
            "INSERT INTO plugin(plugin_id, enabled, fingerprint, approved_at) VALUES(?1, 1, ?2, ?3)
             ON CONFLICT(plugin_id) DO UPDATE SET enabled = 1, fingerprint = excluded.fingerprint,
               approved_at = excluded.approved_at",
            params![plugin_id, fingerprint, now],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_and_approval_live_in_one_record() {
        let db = Db::open_in_memory().unwrap();
        let repo = PluginRepo::new(&db);
        assert!(repo.load_all().unwrap().is_empty());
        repo.set_enabled("a", false);
        assert_eq!(repo.load_all().unwrap()["a"], PluginRecord::default());

        repo.approve("a", "f1");
        assert_eq!(
            repo.load_all().unwrap()["a"],
            PluginRecord {
                enabled: true,
                fingerprint: Some("f1".into())
            }
        );

        // 停用后指纹保留
        repo.set_enabled("a", false);
        let record = &repo.load_all().unwrap()["a"];
        assert!(!record.enabled);
        assert_eq!(record.fingerprint.as_deref(), Some("f1"));
    }
}
