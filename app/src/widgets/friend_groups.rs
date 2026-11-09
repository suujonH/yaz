//! 好友分组的共用流程：新建、重命名、删除。

use crate::ui::*;
use dioxus::dioxus_core::spawn_forever;
use std::sync::Arc;
use zulip_core::model::FriendGroup;
use zulip_session::{DeleteTarget, RenameTarget, Session};

/// 输入名称并新建分组；取消或失败时为 `None`。
pub async fn add_friend_group(overlays: Overlays, session: &Arc<Session>) -> Option<FriendGroup> {
    let name = overlays
        .prompt_text(TextPrompt::new("添加分组", "填写分组名称", ""))
        .await?;
    overlays
        .attempt("添加失败", session.add_friend_group(&name))
        .await
}

/// 分组的右键菜单：重命名、删除。
pub fn friend_group_menu(
    overlays: Overlays,
    session: &Arc<Session>,
    group: &FriendGroup,
) -> Vec<MenuEntry> {
    let id = group.id;
    let rename = {
        let (session, name) = (session.clone(), group.name.clone());
        MenuEntry::item("重命名分组", move || {
            let (session, name) = (session.clone(), name.clone());
            spawn_forever(async move {
                let prompt = TextPrompt::new("重命名分组", "填写分组名称", name);
                if let Some(name) = overlays.prompt_text(prompt).await {
                    overlays
                        .attempt(
                            "重命名失败",
                            session.rename(RenameTarget::FriendGroup(id), &name),
                        )
                        .await;
                }
            });
        })
    };
    let delete = {
        let session = session.clone();
        let target = format!("分组“{}”", group.name);
        MenuEntry::item("删除分组", move || {
            let (session, target) = (session.clone(), target.clone());
            spawn_forever(async move {
                overlays
                    .delete(
                        &target,
                        Some("删除后，该分组中的好友将移至我的好友"),
                        || session.delete(DeleteTarget::FriendGroup(id)),
                    )
                    .await;
            });
        })
        .danger()
    };
    vec![rename, delete]
}
