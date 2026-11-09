//! Permission catalog: shown in groups in the enable confirmation.

/// Category of a permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionGroup {
    /// Data the plugin can read.
    Read,
    /// Operations the plugin can perform.
    Act,
    /// Declaration only; the host cannot restrict it.
    Declared,
}

#[derive(Clone, Copy, Debug)]
pub struct Permission {
    pub id: &'static str,
    pub name: &'static str,
    pub group: PermissionGroup,
}

const fn permission(id: &'static str, name: &'static str, group: PermissionGroup) -> Permission {
    Permission { id, name, group }
}

pub const READ_MESSAGE: &str = "read_message";
pub const READ_CONTACTS: &str = "read_contacts";
pub const SEND_MESSAGE: &str = "send_message";
pub const MODIFY_MESSAGE: &str = "modify_message";
pub const UPLOAD_FILE: &str = "upload_file";
pub const ANNOTATE_MESSAGE: &str = "annotate_message";
pub const RENDER_MESSAGE: &str = "render_message";
pub const FILL_COMPOSER: &str = "fill_composer";
pub const EDIT_MESSAGE: &str = "edit_message";
pub const DELETE_MESSAGE: &str = "delete_message";
pub const MANAGE_CHANNEL: &str = "manage_channel";
pub const MANAGE_USER: &str = "manage_user";
pub const LAUNCH: &str = "launch";
pub const NETWORK: &str = "network";
pub const READ_ATTACHMENT: &str = "read_attachment";
pub const MANAGE_TOPIC: &str = "manage_topic";
pub const OPEN_CONVERSATION: &str = "open_conversation";
pub const NOTIFY: &str = "notify";
pub const ZULIP_API: &str = "zulip_api";
pub const MANAGE_SUBSCRIPTION: &str = "manage_subscription";
pub const MANAGE_PREFERENCES: &str = "manage_preferences";
pub const UI_DIALOG: &str = "ui_dialog";

pub const ALL: &[Permission] = &[
    permission(READ_MESSAGE, "读取消息", PermissionGroup::Read),
    permission(READ_CONTACTS, "读取联系人和频道", PermissionGroup::Read),
    permission(READ_ATTACHMENT, "下载消息中的附件", PermissionGroup::Read),
    permission(SEND_MESSAGE, "发送消息", PermissionGroup::Act),
    permission(
        MODIFY_MESSAGE,
        "改写发送的消息、回应和标记消息",
        PermissionGroup::Act,
    ),
    permission(UPLOAD_FILE, "上传文件", PermissionGroup::Act),
    permission(ANNOTATE_MESSAGE, "在消息下方显示内容", PermissionGroup::Act),
    permission(RENDER_MESSAGE, "改写气泡信息", PermissionGroup::Act),
    permission(FILL_COMPOSER, "填入输入框", PermissionGroup::Act),
    permission(EDIT_MESSAGE, "编辑消息", PermissionGroup::Act),
    permission(DELETE_MESSAGE, "删除消息", PermissionGroup::Act),
    permission(MANAGE_CHANNEL, "修改频道", PermissionGroup::Act),
    permission(MANAGE_USER, "修改用户", PermissionGroup::Act),
    permission(LAUNCH, "打开链接和程序", PermissionGroup::Act),
    permission(MANAGE_TOPIC, "移动、改名和删除话题", PermissionGroup::Act),
    permission(OPEN_CONVERSATION, "切换到其他会话", PermissionGroup::Act),
    permission(NOTIFY, "显示通知和状态栏提示", PermissionGroup::Act),
    permission(
        MANAGE_SUBSCRIPTION,
        "订阅和退订频道、创建频道、修改话题关注和频道个人设置",
        PermissionGroup::Act,
    ),
    permission(
        MANAGE_PREFERENCES,
        "修改免打扰、置顶、静音用户、提醒词和个人状态",
        PermissionGroup::Act,
    ),
    permission(UI_DIALOG, "弹出对话框", PermissionGroup::Act),
    permission(
        ZULIP_API,
        "【高危】以你的账号直接调用 Zulip API：可以读取、修改和删除你有权访问的任何内容",
        PermissionGroup::Act,
    ),
    permission(NETWORK, "访问网络", PermissionGroup::Declared),
];

pub fn find(id: &str) -> Option<&'static Permission> {
    ALL.iter().find(|p| p.id == id)
}
