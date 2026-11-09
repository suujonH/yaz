/*
 * YaZ 原生插件接口。
 *
 * - x64、__cdecl、UTF-8 字符串。
 * - 宿主不导出符号：插件导出 zulip_plugin_init，宿主把一张函数表传进来，插件保存后通过它调用宿主。
 *   表中的 ctx 代表“哪个插件”，每次调用原样传回。函数表一直有效。
 * - 宿主交给插件的指针（消息、用户、字符串）只在回调执行期间有效，需要的话请自行复制。
 * - 所有导出函数都在同一个插件线程上依次调用，请尽快返回，耗时操作放到自己的线程。
 *   插件可以在任何线程调用宿主函数（同步阻塞），但不能在 tokio 运行时线程（例如插件自己创建的运行时的工作线程，
 *   或 block_on 内部）上调用：宿主函数内部要等待宿主的运行时，这样调用会返回 ZULIP_PLUGIN_ERR_UNKNOWN。
 * - 原生插件不在沙箱中，崩溃会导致整个程序崩溃。开启确认中列出的权限只约束函数表：DLL 运行在程序进程中，
 *   可以绕过函数表做任何事，确认开启等于信任这个 DLL。
 * - 停用插件不再派发事件，DLL 在程序重启后才会卸载。停用的插件调用宿主函数返回 ZULIP_PLUGIN_ERR_DISABLED，
 *   只有 add_log、get_data_dir、get_settings 例外；on_disable 回调执行期间仍可调用。
 * - 插件目录：<程序目录>\plugins\<id>\，包含 plugin.json 与 DLL。
 *
 * ABI 规则（API 版本 1 内只追加、不改动）：
 * - 结构和函数表的已有字段不改名、不改类型、不改顺序；新字段只追加到末尾；取值只增加。
 * - 结构的第一个字段是 struct_size（函数表也有 struct_size）。读取或调用后来追加的成员前，用
 *   offsetof(类型, 成员) < struct_size 判断对方是否提供了它。插件传给宿主的结构（conversation）要填写 struct_size。
 * - 嵌套的结构通过指针引用，所以任何结构都可以独立增长。
 * - 宿主函数和导出函数的参数不变；需要新的参数时增加新的函数。回调 JSON 只增加键，请忽略不认识的键。
 *
 * 多账号：多个账号可以同时登录，所有账号的消息都派发给插件，account_id 是本地账号 id，程序重启后不变。
 * 宿主函数使用的账号：回调所属的账号 > set_thread_account 绑定的账号 > select_account 选择的账号 >
 * 最先登录的账号（get_accounts 的第一项）。select_account 选择的账号已退出时使用最先登录的账号；
 * 线程绑定的账号已退出时返回 ZULIP_PLUGIN_ERR_NOT_LOGGED_IN，不会改用其他账号。
 *
 * plugin.json：
 * - id（与目录名相同）、name、version、author、description、api_version、library（DLL 文件名）。
 * - permissions：插件只能调用声明过的权限对应的宿主函数（否则返回 ZULIP_PLUGIN_ERR_PERMISSION）。
 *   read_message、read_contacts、read_attachment、send_message、modify_message、upload_file、annotate_message、
 *   render_message、fill_composer、edit_message、delete_message、manage_channel、manage_user、manage_topic、
 *   open_conversation、notify、launch、manage_subscription、manage_preferences、ui_dialog、zulip_api（高危）、
 *   network（仅声明）。
 *   权限和函数表的对应关系：
 *     read_message：get_message、get_messages、search_messages、get_edit_history、get_message_source、get_read_receipts、
 *       get_message_actions、get_conversations、get_current_conversation、get_conversation_level、get_drafts、
 *       get_scheduled_messages、get_reminders、get_alert_words
 *     read_contacts：get_users、get_user、get_presence、get_user_groups、get_muted_users、get_channels、get_all_channels、
 *       get_topics、get_topic_visibility、get_subscribers、get_realm_info
 *     read_attachment：download_attachment、download_attachment_ex
 *     send_message：send_channel_message、send_direct_message、schedule_message、cancel_scheduled_message
 *     modify_message：set_outgoing_content、add_reaction、remove_reaction、set_starred、mark_read、mark_unread、
 *       create_reminder、cancel_reminder
 *     upload_file：upload_file、upload_file_ex
 *     annotate_message：set_annotation（其中 replace 还需要 render_message，fill 按钮需要 fill_composer，target 按钮需要 launch）
 *     render_message：只用于 set_annotation 的 replace
 *     fill_composer：fill_composer、fill_composer_to、save_draft
 *     edit_message：edit_message
 *     delete_message：delete_message
 *     manage_channel：update_channel
 *     manage_user：update_user
 *     manage_topic：move_topic、set_topic_resolved、delete_topic
 *     open_conversation：open_conversation
 *     notify：show_toast、show_notification、set_status_text
 *     launch：launch
 *     manage_subscription：set_subscribed、set_subscription_property、set_topic_visibility、create_channel
 *     manage_preferences：set_user_muted、set_alert_word、set_own_status、set_conversation_level、set_conversation_pinned
 *     ui_dialog：show_dialog
 *     zulip_api：api_request
 *     network：没有对应的函数，宿主无法限制
 *     不需要权限：get_accounts、get_current_account、select_account、set_thread_account、add_log、get_data_dir、
 *       get_settings、set_settings、set_settings_feedback、get_last_error、reply_panel、publish_resources、
 *       request_resource、reply_resource
 *   新安装的插件默认停用；用户开启时宿主列出权限、事件与 launch 白名单并要求确认。宿主记录插件目录中所有文件的
 *   指纹，任何文件变化后都需要重新开启，因此运行时数据请写到 get_data_dir 返回的目录。运行期间开启时同样重新计算指纹；
 *   此时重新确认的清单和 DLL 变化在程序重启后才完全生效。
 * - events：{"事件": 优先级}，10000～40000，数字越小越先执行，10000 只能监控（不能拦截、改写）。声明了才会收到。
 *   on_message、filter_incoming 需要 read_message，filter_outgoing 需要 modify_message。
 *   通知型事件（不能拦截，返回值固定为 void，导出函数见文件末尾）：on_message_edited、on_message_deleted、
 *   on_reaction_changed、on_read_state_changed、on_topic_moved、on_conversation_opened、on_connection_changed
 *   需要 read_message；on_typing_changed、on_scheduled_changed、on_reminders_changed、on_draft_changed、
 *   on_user_topic_changed 同样需要 read_message；on_channel_changed、on_user_changed、on_presence_changed、on_muted_users_changed、
 *   on_realm_changed 需要 read_contacts。
 *   宿主最多等待一条事件链 2 秒，连续 3 次超时的插件在本次运行内不再收到事件；有插件超时未返回期间，新的事件被跳过。
 *   filter_outgoing、filter_incoming 超时时放行（内容照常发送、消息照常显示），因此不能用于合规拦截。
 * - dependencies：["依赖的插件 id"]。开启前需要先开启依赖的插件；只能使用依赖的插件发布的资源。启动时依赖未开启的插件
 *   保持未激活并写入日志；停用插件时依赖它的插件随之停用并写入日志，重新开启时不会自动恢复。
 * - menus：[{"id": 1, "name": "打开", "shortcut": "Ctrl+Shift+T" 或 ""}]，显示在插件列表中，点击时调用 zulip_plugin_on_menu。
 *   写了 shortcut 表示可以绑定键盘快捷键（需要 Ctrl 或 Alt，F1～F24 可以单独使用，"" 为没有默认值）。
 *   焦点在能输入文字的地方（如输入框）时，输入法先于程序收到按键，它占用的组合键到不了程序，快捷键不会触发
 *   （例如中文输入法下的 Ctrl+Shift+E）。默认值不要选常见输入法占用的组合键。
 * - message_menus：[{"id": 1, "name": "翻译", "shortcut": "middle_click" 或 ""}]，显示在消息右键菜单中，需要 read_message，
 *   点击时调用 zulip_plugin_on_message_menu。子菜单写作 {"name": "设置", "children": [...]}（最多嵌套 2 层）。
 *   shortcut 为对单条消息的鼠标手势：[ctrl+][alt+][shift+]{left|middle|right}_{click|double}；
 *   left_click、left_double、right_click、right_double、shift+left_click 由宿主使用，不能绑定。
 * - conversation_menus（需要 read_message）、user_menus、channel_menus（需要 read_contacts）：会话、用户、频道的右键菜单项，
 *   写法同 message_menus（没有 shortcut），点击时调用 zulip_plugin_on_context_menu。
 * - message_menus、conversation_menus 的项可以写显示条件 "when": {"conv": "channel" | "direct", "own": 自己发送,
 *   "has_uploads": 带上传文件, "can_edit": 可以编辑}，都可以省略，写了的全部满足才显示；未知的条件写入日志并忽略。
 *   会话菜单没有消息，own、has_uploads、can_edit 视为 false。子菜单的所有子项都不显示时不显示子菜单。
 * - composer_buttons：[{"id": 1, "icon": "edit" 或 "icons/a.svg", "tooltip": "说明"}]，输入框工具栏按钮，需要 read_message，
 *   点击时调用 zulip_plugin_on_composer_button。icon 为内置图标（add、chat、check、edit、file、folder、history、image、lock、
 *   more、search、smile、time、user）或插件目录内的 SVG 文件。
 * - panels：[{"kind": "topic" | "user", "label": "话题场景"}]，每种最多一个。宿主在话题资料面板 / 用户资料卡中为插件显示
 *   一个分组，在插件设置中显示以 label 为标题的列表；数据由插件保存，通过 zulip_plugin_on_panel 与 reply_panel 交互。
 * - settings：宿主生成设置界面并保存（插件只能读取，get_settings）。每项 {"key", "label",
 *   "type": text|password|multiline|select|bool|list|button|tags|resource, "hint", "placeholder", "default",
 *   "options": [{"value", "label"}]（select）, "fields": [...]（list 每行的设置项，不能嵌套 list）,
 *   "visible_when": {"同一层另一项的 key": "值" 或 ["值", ...]}}。password 的值由宿主加密保存。
 *   button 没有值：点击时以表单当前的值调用 zulip_plugin_on_settings_button，插件用 set_settings_feedback 显示结果。
 *   tags 的值为字符串数组，"item_button": "测试" 在每一项旁边显示按钮；插件可以用 set_settings_feedback 提供候选。
 *   resource 选择依赖的插件发布的资源，"plugin": "发布者 id", "resource": "资源类型"；
 *   值为 {"type", "plugin", "resource", "item"}，没有选择时为 {}。
 * - launch：白名单，开启时告知用户，launch() 只能打开白名单中的目标。写法："todo:"（协议，不能是 http/https/file 等）、
 *   "https://example.com/path"（网址前缀，主机可以用 *. 开头匹配子域名）、"C:\Tools\a.exe"（完整路径）、
 *   "notepad.exe"（只匹配同样只写文件名的目标）。白名单之外的目标请放在 set_annotation 的按钮中，由用户点击后打开。
 *
 * 插件间共享资源：宿主只是通道，资源类型（小写字母、数字、下划线）与请求、回复的格式由发布者约定。发布者用
 * publish_resources 发布并导出 zulip_plugin_on_resource_request；使用者在 dependencies 中声明发布者，在自己的线程上
 * 调用 request_resource，发布者用 reply_resource 回复。
 *
 * 回调和查询 JSON 中的会话（下文写作 <conv>）：{"type": "channel", "stream": 频道 id, "stream_name", "topic"} 或
 * {"type": "direct", "users": [对方 id]}（与自己私聊时为空数组）。消息不在缓存中的私聊事件 conv 为 null。
 */
#pragma once
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif
#define ZULIP_PLUGIN_API_VERSION 1
#define ZULIP_PLUGIN_EXPORT __declspec(dllexport)
#define ZULIP_PLUGIN_CALL __cdecl

#define ZULIP_PLUGIN_EVENT_IGNORE 0
#define ZULIP_PLUGIN_EVENT_BLOCK  1

/* 错误码：宿主函数返回负数表示失败 */
#define ZULIP_PLUGIN_OK                 0
#define ZULIP_PLUGIN_ERR_UNKNOWN       -1
#define ZULIP_PLUGIN_ERR_INVALID_ARG   -2
#define ZULIP_PLUGIN_ERR_PERMISSION    -3   /* plugin.json 未声明对应权限 */
#define ZULIP_PLUGIN_ERR_NOT_LOGGED_IN -4
#define ZULIP_PLUGIN_ERR_NETWORK       -5
#define ZULIP_PLUGIN_ERR_SERVER        -6   /* Zulip 返回错误，详情见日志 */
#define ZULIP_PLUGIN_ERR_NOT_FOUND     -7
#define ZULIP_PLUGIN_ERR_WRONG_CONTEXT -8   /* 例如在 filter_outgoing 之外调用 set_outgoing_content */
#define ZULIP_PLUGIN_ERR_UNSUPPORTED   -9   /* 宿主太旧，没有这个函数（SDK 封装返回；C 插件用 offsetof 判断） */
#define ZULIP_PLUGIN_ERR_DISABLED      -10  /* 插件已停用 */

#define ZULIP_PLUGIN_LOG_DEBUG 0
#define ZULIP_PLUGIN_LOG_INFO 10
#define ZULIP_PLUGIN_LOG_INFO_SUCCESS 11
#define ZULIP_PLUGIN_LOG_INFO_RECV 12
#define ZULIP_PLUGIN_LOG_INFO_SEND 13
#define ZULIP_PLUGIN_LOG_WARNING 20
#define ZULIP_PLUGIN_LOG_ERROR 30
#define ZULIP_PLUGIN_LOG_FATAL 40

#define ZULIP_PLUGIN_CONV_CHANNEL 1   /* 频道话题 */
#define ZULIP_PLUGIN_CONV_DIRECT  2   /* 私聊（含多人、与自己） */

/* zulip_plugin_message.flags */
#define ZULIP_PLUGIN_MSG_OUTGOING          0x01  /* 自己发送 */
#define ZULIP_PLUGIN_MSG_MENTIONED         0x02  /* @我（含用户组） */
#define ZULIP_PLUGIN_MSG_WILDCARD_MENTION  0x04  /* @全体 / @topic */
#define ZULIP_PLUGIN_MSG_STARRED           0x08
#define ZULIP_PLUGIN_MSG_READ              0x10
#define ZULIP_PLUGIN_MSG_HISTORICAL        0x20  /* 通过 get_message(s) 或搜索获取，而非新消息事件 */

/* zulip_plugin_outgoing.kind */
#define ZULIP_PLUGIN_OUTGOING_TEXT    0   /* 输入框的文字 */
#define ZULIP_PLUGIN_OUTGOING_FILE    1   /* 发送文件（内容为带上传链接的 Markdown） */
#define ZULIP_PLUGIN_OUTGOING_FORWARD 2   /* 转发 */

/* fill_composer 的 mode */
#define ZULIP_PLUGIN_FILL_INSERT  0   /* 插入到光标处 */
#define ZULIP_PLUGIN_FILL_REPLACE 1   /* 替换已有内容 */

/* get_topic_visibility 的返回值 */
#define ZULIP_PLUGIN_TOPIC_INHERIT 0
#define ZULIP_PLUGIN_TOPIC_MUTED   1
#define ZULIP_PLUGIN_TOPIC_UNMUTED 2
#define ZULIP_PLUGIN_TOPIC_FOLLOWED 3

/* get_conversation_level、set_conversation_level 的级别 */
#define ZULIP_PLUGIN_LEVEL_NOTIFY    0   /* 允许消息提醒 */
#define ZULIP_PLUGIN_LEVEL_SILENT    1   /* 接收消息但不提醒 */
#define ZULIP_PLUGIN_LEVEL_ASSISTANT 2   /* 收进群助手且不提醒 */
#define ZULIP_PLUGIN_LEVEL_BLOCKED   3   /* 屏蔽：不显示、不提醒、不计入未读 */

typedef struct zulip_plugin_conversation {
    uint32_t struct_size;
    int32_t type;                 /* ZULIP_PLUGIN_CONV_* */
    int64_t stream_id;            /* 频道：频道 id */
    const char* stream_name;      /* 频道：频道名；私聊为 NULL */
    const char* topic;            /* 频道：话题名；私聊为 NULL */
    const int64_t* dm_user_ids;   /* 私聊：对方 id 数组（升序）；频道为 NULL */
    int32_t dm_user_count;        /* 私聊：对方人数（不含自己），与自己私聊为 0 */
} zulip_plugin_conversation;

typedef struct zulip_plugin_message {
    uint32_t struct_size;
    uint32_t flags;               /* ZULIP_PLUGIN_MSG_* */
    int64_t message_id;
    int64_t account_id;           /* 消息所属的账号 */
    int64_t timestamp;            /* Unix 秒 */
    int64_t sender_id;
    const char* sender_name;
    const char* sender_email;
    const char* content_html;     /* 服务器渲染的 HTML */
    const char* content_text;     /* 宿主转换的纯文本 */
    const zulip_plugin_conversation* conv;
    const char* reactions_json;   /* [{"emoji_name", "emoji_code", "reaction_type", "user_id"}] */
    const char* uploads_json;     /* [{"path": "/user_uploads/...", "file_name", "is_image"}] */
    int64_t last_edit_timestamp;  /* Unix 秒；没有编辑过为 0 */
} zulip_plugin_message;

typedef struct zulip_plugin_outgoing {
    uint32_t struct_size;
    int32_t kind;                 /* ZULIP_PLUGIN_OUTGOING_* */
    int64_t account_id;           /* 发送消息的账号 */
    const zulip_plugin_conversation* conv;
    const char* content;
} zulip_plugin_outgoing;

typedef struct zulip_plugin_user {
    uint32_t struct_size;
    int32_t role;                 /* 100 所有者 … 600 访客 */
    int64_t user_id;
    const char* full_name;
    const char* email;
    const char* remark;           /* 本地备注，可能为 NULL */
    int32_t is_bot;
    int32_t is_active;
} zulip_plugin_user;

typedef struct zulip_plugin_channel {
    uint32_t struct_size;
    int32_t is_private;
    int64_t stream_id;
    const char* name;
    const char* description;
    int32_t is_subscribed;
} zulip_plugin_channel;

typedef struct zulip_plugin_topic {
    uint32_t struct_size;
    int64_t stream_id;
    int64_t max_message_id;
    const char* name;
} zulip_plugin_topic;

typedef struct zulip_plugin_account {
    uint32_t struct_size;
    int64_t account_id;           /* 本地账号 id */
    int64_t user_id;              /* 该账号在其组织中的用户 id */
    const char* full_name;
    const char* email;
    const char* realm_url;
} zulip_plugin_account;

typedef void (ZULIP_PLUGIN_CALL *zulip_plugin_message_cb)(const zulip_plugin_message* msg, void* user);
typedef void (ZULIP_PLUGIN_CALL *zulip_plugin_user_cb)(const zulip_plugin_user* u, void* user);
typedef void (ZULIP_PLUGIN_CALL *zulip_plugin_channel_cb)(const zulip_plugin_channel* c, void* user);
typedef void (ZULIP_PLUGIN_CALL *zulip_plugin_topic_cb)(const zulip_plugin_topic* t, void* user);
typedef void (ZULIP_PLUGIN_CALL *zulip_plugin_string_cb)(const char* s, void* user);
typedef void (ZULIP_PLUGIN_CALL *zulip_plugin_account_cb)(const zulip_plugin_account* a, void* user);
/* 进度：done、total 为字节数，total 未知时为 0。user 与结果回调相同 */
typedef void (ZULIP_PLUGIN_CALL *zulip_plugin_progress_cb)(int64_t done, int64_t total, void* user);

typedef struct zulip_plugin_host_api {
    uint32_t struct_size;
    uint32_t api_version;          /* = ZULIP_PLUGIN_API_VERSION */
    void* ctx;                     /* 每次调用原样传回 */
    void    (ZULIP_PLUGIN_CALL *add_log)(void* ctx, int32_t level, const char* category, const char* text);
    /* 已登录的账号，第一项是最先登录的账号；返回账号数 */
    int32_t (ZULIP_PLUGIN_CALL *get_accounts)(void* ctx, zulip_plugin_account_cb cb, void* user);
    /* 宿主函数现在使用的账号；没有账号登录时 ZULIP_PLUGIN_ERR_NOT_LOGGED_IN */
    int64_t (ZULIP_PLUGIN_CALL *get_current_account)(void* ctx);
    /* 选择回调之外使用的账号；在回调中调用时，本次回调剩余的调用也使用该账号。账号未登录时 ZULIP_PLUGIN_ERR_NOT_FOUND */
    int32_t (ZULIP_PLUGIN_CALL *select_account)(void* ctx, int64_t account_id);
    int32_t (ZULIP_PLUGIN_CALL *get_data_dir)(void* ctx, zulip_plugin_string_cb cb, void* user);  /* 以 \ 结尾 */
    int32_t (ZULIP_PLUGIN_CALL *get_settings)(void* ctx, zulip_plugin_string_cb cb, void* user);  /* JSON 对象（key -> 值） */
    /* 回复设置窗口（任何线程；窗口已关闭时忽略）：
       {"key": "设置项", "row": zulip_plugin_on_settings_button 收到的行号（顶层不写）, "text": "显示在设置项下方的文字",
        "options": ["tags 设置项的候选"]}；
       tags 某一项旁的按钮的结果写作 {"key", "row"?, "item": "该项", "status": "ok" | "error", "text": "失败的原因"} */
    int32_t (ZULIP_PLUGIN_CALL *set_settings_feedback)(void* ctx, const char* json);
    /* 需要 send_message；返回消息 id（>0）。send_direct_message 的 count 为 0 时与自己私聊 */
    int64_t (ZULIP_PLUGIN_CALL *send_channel_message)(void* ctx, int64_t stream_id, const char* topic, const char* markdown);
    int64_t (ZULIP_PLUGIN_CALL *send_direct_message)(void* ctx, const int64_t* user_ids, int32_t count, const char* markdown);
    /* 需要 read_message。get_messages：conv 为 NULL 表示全部；anchor：0 = 最新，-1 = 最旧，其它 = 消息 id（含它本身）；返回条数 */
    int32_t (ZULIP_PLUGIN_CALL *get_message)(void* ctx, int64_t message_id, zulip_plugin_message_cb cb, void* user);
    int32_t (ZULIP_PLUGIN_CALL *get_messages)(void* ctx, const zulip_plugin_conversation* conv, int64_t anchor,
                                              int32_t num_before, int32_t num_after, zulip_plugin_message_cb cb, void* user);
    /* 需要 read_contacts */
    int32_t (ZULIP_PLUGIN_CALL *get_users)(void* ctx, zulip_plugin_user_cb cb, void* user);
    int32_t (ZULIP_PLUGIN_CALL *get_user)(void* ctx, int64_t user_id, zulip_plugin_user_cb cb, void* user);
    int32_t (ZULIP_PLUGIN_CALL *get_channels)(void* ctx, zulip_plugin_channel_cb cb, void* user);  /* 已订阅 */
    int32_t (ZULIP_PLUGIN_CALL *get_topics)(void* ctx, int64_t stream_id, zulip_plugin_topic_cb cb, void* user);
    /* 需要 modify_message */
    int32_t (ZULIP_PLUGIN_CALL *add_reaction)(void* ctx, int64_t message_id, const char* emoji_name);
    int32_t (ZULIP_PLUGIN_CALL *set_starred)(void* ctx, int64_t message_id, int32_t starred);
    int32_t (ZULIP_PLUGIN_CALL *mark_read)(void* ctx, int64_t message_id);
    /* 需要 upload_file；回调 /user_uploads/... */
    int32_t (ZULIP_PLUGIN_CALL *upload_file)(void* ctx, const char* local_path, zulip_plugin_string_cb cb, void* user);
    /* 需要 modify_message；只在 zulip_plugin_filter_outgoing 内有效 */
    int32_t (ZULIP_PLUGIN_CALL *set_outgoing_content)(void* ctx, const char* markdown);
    /* 给消息附加内容（不修改原文，只在本次运行中有效，停用插件时隐藏；需要保留时由插件保存，在
       zulip_plugin_on_messages_shown 中重新附加）：
       {"text": "气泡下方的文字，支持受限 Markdown（粗体、斜体、行内代码、代码块、列表、标题、http(s) 链接，其余转义）",
        "replace": "临时替换气泡显示内容的纯文字",
        "actions": [{"label": "按钮", "target": "URI 或程序", "args": "程序参数", "style": "primary" | "secondary"} 或
                    {"label": "按钮", "fill": "填入输入框的文字", "style": ...}]}
       （最多 5 个按钮）；json 为 NULL 或空字符串表示清除，再次调用覆盖本插件之前的内容。一条消息上每个插件一张卡片。
       需要 annotate_message；replace 还需要 render_message，fill 按钮需要 fill_composer，target 按钮需要 launch */
    int32_t (ZULIP_PLUGIN_CALL *set_annotation)(void* ctx, int64_t message_id, const char* json);
    /* 打开白名单中的 URI 或运行白名单中的程序（args 只用于程序，可为 NULL）；不在白名单中返回 ZULIP_PLUGIN_ERR_PERMISSION。需要 launch */
    int32_t (ZULIP_PLUGIN_CALL *launch)(void* ctx, const char* target, const char* args);
    int32_t (ZULIP_PLUGIN_CALL *edit_message)(void* ctx, int64_t message_id, const char* markdown);  /* 需要 edit_message */
    int32_t (ZULIP_PLUGIN_CALL *delete_message)(void* ctx, int64_t message_id);                      /* 需要 delete_message */
    /* 修改频道名称 / 介绍，NULL 表示不修改。需要 manage_channel */
    int32_t (ZULIP_PLUGIN_CALL *update_channel)(void* ctx, int64_t stream_id, const char* name, const char* description);
    /* 修改用户名称 / 角色（100 所有者、200 管理员、300 版主、400 成员、600 访客），NULL / 0 表示不修改。需要 manage_user */
    int32_t (ZULIP_PLUGIN_CALL *update_user)(void* ctx, int64_t user_id, const char* full_name, int32_t role);
    /* 发布 type 类资源，替换之前发布的；json 为 NULL 或空字符串表示清除。只保存在内存中，请在 init 和设置变化时发布。
       json：[{"id", "name", "items": ["可选的项"]}] */
    int32_t (ZULIP_PLUGIN_CALL *publish_resources)(void* ctx, const char* type, const char* json);
    /* 使用其他插件的资源：{"resource": resource 设置项的值, "request": {...}}；阻塞到发布者回复（cb 回调 1 次）或超时（120 秒，
       ZULIP_PLUGIN_ERR_NETWORK）。资源不存在或发布者未开启时 ZULIP_PLUGIN_ERR_NOT_FOUND；不能在回调中（插件线程上）调用
       （ZULIP_PLUGIN_ERR_WRONG_CONTEXT）。发布者不在 dependencies 中时 ZULIP_PLUGIN_ERR_PERMISSION */
    int32_t (ZULIP_PLUGIN_CALL *request_resource)(void* ctx, const char* json, zulip_plugin_string_cb cb, void* user);
    /* 回复 zulip_plugin_on_resource_request 收到的请求（任何线程）；请求不存在或已超时 ZULIP_PLUGIN_ERR_NOT_FOUND */
    int32_t (ZULIP_PLUGIN_CALL *reply_resource)(void* ctx, int64_t request_id, const char* json);
    /* 回复 zulip_plugin_on_panel（只能在该回调中调用，否则 ZULIP_PLUGIN_ERR_WRONG_CONTEXT；再次调用覆盖）。
       get / set / click：{"fields": [字段]}（没有设置项时不显示分组），字段是以下之一：
         {"key", "label", "type": "select", "options": [{"value", "label"}], "value": "选中的值"}
         {"key", "label", "type": "bool", "value": true}
         {"key", "label", "type": "text", "value": "文字", "placeholder": "提示"}
         {"key", "label", "type": "button"}（点击时 action 为 click）
         {"key", "label", "type": "info", "value": "只读文字"}
       list：{"entries": [{"target": 与 zulip_plugin_on_panel 的 target 相同, "text": "显示在名称后的说明"}]}；remove 不需要回复 */
    int32_t (ZULIP_PLUGIN_CALL *reply_panel)(void* ctx, const char* json);
    /* 搜索消息（新的在前），需要 read_message。query：
       {"conv": <conv>（只查这个会话）, "starred": true（只查收藏）, "sender": 用户 id, "text": "关键词",
        "filter": "all" | "plain" | "media" | "files" | "attachments" | "links", "until": "ISO 8601，只看这个时间及以前",
        "page_size": 1～200（默认 50）, "cursor": 上一页 next_cursor}，都可以省略。
       next_cursor（可为 NULL）写入下一页的位置，0 表示没有下一页；返回本页条数 */
    int32_t (ZULIP_PLUGIN_CALL *search_messages)(void* ctx, const char* query, int64_t* next_cursor, zulip_plugin_message_cb cb, void* user);
    /* 消息页的会话和未读数，需要 read_message：
       {"total_unread": n, "conversations": [{"conv": <conv>, "title", "preview", "time": Unix 秒, "unread", "pinned"}]} */
    int32_t (ZULIP_PLUGIN_CALL *get_conversations)(void* ctx, zulip_plugin_string_cb cb, void* user);
    /* 用户最后切换到的会话 <conv>，没有打开时 ZULIP_PLUGIN_ERR_NOT_FOUND。需要 read_message */
    int32_t (ZULIP_PLUGIN_CALL *get_current_conversation)(void* ctx, zulip_plugin_string_cb cb, void* user);
    /* 在线状态和签名，user_id 为 0 时是所有活跃用户。需要 read_contacts：
       [{"user": id, "presence": "online" | "idle" | "offline", "status_text": "签名" 或 null}] */
    int32_t (ZULIP_PLUGIN_CALL *get_presence)(void* ctx, int64_t user_id, zulip_plugin_string_cb cb, void* user);
    /* 用户组，需要 read_contacts：[{"id", "name", "description", "members": [用户 id], "direct_subgroup_ids", "is_system_group", "deactivated"}] */
    int32_t (ZULIP_PLUGIN_CALL *get_user_groups)(void* ctx, zulip_plugin_string_cb cb, void* user);
    /* 组织内可见的全部频道（含未订阅，见 is_subscribed）。需要 read_contacts */
    int32_t (ZULIP_PLUGIN_CALL *get_all_channels)(void* ctx, zulip_plugin_channel_cb cb, void* user);
    /* 话题的关注状态 ZULIP_PLUGIN_TOPIC_*。需要 read_contacts */
    int32_t (ZULIP_PLUGIN_CALL *get_topic_visibility)(void* ctx, int64_t stream_id, const char* topic);
    /* 服务器上的编辑历史（第一项是原始版本），需要 read_message：
       [{"topic", "prev_topic", "stream", "prev_stream", "content", "rendered_content", "prev_content", "prev_rendered_content",
         "content_html_diff", "user_id", "timestamp"}] */
    int32_t (ZULIP_PLUGIN_CALL *get_edit_history)(void* ctx, int64_t message_id, zulip_plugin_string_cb cb, void* user);
    /* 取消自己的表情回应。需要 modify_message */
    int32_t (ZULIP_PLUGIN_CALL *remove_reaction)(void* ctx, int64_t message_id, const char* emoji_name);
    /* 从这条消息开始，会话中之后的消息全部标记为未读。需要 modify_message */
    int32_t (ZULIP_PLUGIN_CALL *mark_unread)(void* ctx, int64_t message_id);
    /* 移动或改名整个话题，new_stream_id 为 0 表示同一频道。部分消息超出移动时限时 ZULIP_PLUGIN_ERR_SERVER。需要 manage_topic */
    int32_t (ZULIP_PLUGIN_CALL *move_topic)(void* ctx, int64_t stream_id, const char* topic, int64_t new_stream_id, const char* new_topic);
    /* 标记话题已解决 / 未解决。需要 manage_topic */
    int32_t (ZULIP_PLUGIN_CALL *set_topic_resolved)(void* ctx, int64_t stream_id, const char* topic, int32_t resolved);
    /* 删除话题及其全部消息。需要 manage_topic */
    int32_t (ZULIP_PLUGIN_CALL *delete_topic)(void* ctx, int64_t stream_id, const char* topic);
    /* 让界面切换到会话，message_id 不为 0 时定位到消息。需要 open_conversation */
    int32_t (ZULIP_PLUGIN_CALL *open_conversation)(void* ctx, const zulip_plugin_conversation* conv, int64_t message_id);
    /* 把文字填入当前账号当前会话的输入框（不发送）；mode 为 ZULIP_PLUGIN_FILL_*。需要 fill_composer */
    int32_t (ZULIP_PLUGIN_CALL *fill_composer)(void* ctx, const char* text, int32_t mode);
    /* 定时发送，deliver_at 为 Unix 秒，返回定时消息 id。需要 send_message */
    int64_t (ZULIP_PLUGIN_CALL *schedule_message)(void* ctx, const zulip_plugin_conversation* conv, const char* markdown, int64_t deliver_at);
    /* 程序内的提示。需要 notify */
    int32_t (ZULIP_PLUGIN_CALL *show_toast)(void* ctx, const char* text);
    /* 系统通知，body 可为 NULL。需要 notify */
    int32_t (ZULIP_PLUGIN_CALL *show_notification)(void* ctx, const char* title, const char* body);
    /* 显示在主窗口连接提示位置的文字，NULL 或空字符串表示清除；多个插件时显示最后设置的。需要 notify */
    int32_t (ZULIP_PLUGIN_CALL *set_status_text)(void* ctx, const char* text);
    /* 下载附件（/user_uploads/...）到缓存，回调本地路径；已下载时不重复下载。需要 read_attachment */
    int32_t (ZULIP_PLUGIN_CALL *download_attachment)(void* ctx, const char* upload_path, zulip_plugin_string_cb cb, void* user);
    /* 以当前账号调用 Zulip API，回调服务器的 JSON 回复。需要 zulip_api（高危：开启时明确警示用户）。
       method：GET、POST、PUT、PATCH、DELETE；path 必须在 /api/v1/ 下，不能带查询串，不能访问 API key 相关的路径（否则 ZULIP_PLUGIN_ERR_PERMISSION）；
       params：JSON 对象或 NULL，字符串原样发送，其它值按 JSON 编码。每次调用写入插件日志 */
    int32_t (ZULIP_PLUGIN_CALL *api_request)(void* ctx, const char* method, const char* path, const char* params, zulip_plugin_string_cb cb, void* user);

    /* ---- 以下是 v1 基础表之后追加的函数：调用前用 offsetof(zulip_plugin_host_api, 函数) < struct_size 判断 ---- */

    /* 绑定本线程（调用线程）使用的账号，0 解除。账号未登录时 ZULIP_PLUGIN_ERR_NOT_FOUND。优先级见文件开头 */
    int32_t (ZULIP_PLUGIN_CALL *set_thread_account)(void* ctx, int64_t account_id);
    /* 本线程上次失败的详情文字；没有失败过时为空字符串 */
    int32_t (ZULIP_PLUGIN_CALL *get_last_error)(void* ctx, zulip_plugin_string_cb cb, void* user);
    /* 消息的 Markdown 原文。需要 read_message */
    int32_t (ZULIP_PLUGIN_CALL *get_message_source)(void* ctx, int64_t message_id, zulip_plugin_string_cb cb, void* user);
    /* 定时消息，需要 read_message：[{"id", "conv": <conv>, "content", "rendered_content", "deliver_at": Unix 秒, "failed"}]；返回条数 */
    int32_t (ZULIP_PLUGIN_CALL *get_scheduled_messages)(void* ctx, zulip_plugin_string_cb cb, void* user);
    /* 取消定时消息。需要 send_message */
    int32_t (ZULIP_PLUGIN_CALL *cancel_scheduled_message)(void* ctx, int64_t scheduled_id);
    /* 显示对话框并阻塞到用户操作，需要 ui_dialog；不能在回调中（插件线程上）调用（ZULIP_PLUGIN_ERR_WRONG_CONTEXT）。
       json：{"kind": "confirm" | "input" | "form", "title", "text", "fields": [与 reply_panel 相同的字段]}。
       confirm 不使用 fields；input 取 fields 中第一个 text 字段（没有时 key 为 "value"）；form 至少要有一个字段。
       button 字段点击后不关闭对话框，结果的 values 中该 key 为 true；info 字段不进入 values。
       cb 回调 {"ok": 用户点击了确定, "values": {"key": 值}}。5 分钟没有结果时 ZULIP_PLUGIN_ERR_NOT_FOUND */
    int32_t (ZULIP_PLUGIN_CALL *show_dialog)(void* ctx, const char* json, zulip_plugin_string_cb cb, void* user);
    /* 修改本插件的设置：json 为 {"key": 值}，合并到现有设置，不在清单中的 key 忽略；password 照常加密。
       不回调 zulip_plugin_on_settings_changed。不需要权限 */
    int32_t (ZULIP_PLUGIN_CALL *set_settings)(void* ctx, const char* json);
    /* 把文字填入指定会话的输入框（不发送）；mode 为 ZULIP_PLUGIN_FILL_*。频道不存在时 ZULIP_PLUGIN_ERR_NOT_FOUND。需要 fill_composer */
    int32_t (ZULIP_PLUGIN_CALL *fill_composer_to)(void* ctx, const zulip_plugin_conversation* conv, const char* text, int32_t mode);
    /* 设置话题的关注状态 ZULIP_PLUGIN_TOPIC_*。需要 manage_subscription */
    int32_t (ZULIP_PLUGIN_CALL *set_topic_visibility)(void* ctx, int64_t stream_id, const char* topic, int32_t visibility);
    /* 订阅（subscribed 非 0）或退订频道；频道不可见时 ZULIP_PLUGIN_ERR_NOT_FOUND。需要 manage_subscription */
    int32_t (ZULIP_PLUGIN_CALL *set_subscribed)(void* ctx, int64_t stream_id, int32_t subscribed);
    /* 修改频道的个人设置：name 为 Zulip 的属性名（color、is_muted、desktop_notifications 等），json 为新值的 JSON。需要 manage_subscription */
    int32_t (ZULIP_PLUGIN_CALL *set_subscription_property)(void* ctx, int64_t stream_id, const char* name, const char* json);
    /* 创建频道：{"name", "description", "private": false, "subscribers": [用户 id]}，只有 name 必须；返回频道 id。需要 manage_subscription */
    int64_t (ZULIP_PLUGIN_CALL *create_channel)(void* ctx, const char* json);
    /* 频道的订阅者：[用户 id]；返回人数。需要 read_contacts */
    int32_t (ZULIP_PLUGIN_CALL *get_subscribers)(void* ctx, int64_t stream_id, zulip_plugin_string_cb cb, void* user);
    /* 为消息创建提醒，deliver_at 为 Unix 秒，返回提醒 id。需要 modify_message */
    int64_t (ZULIP_PLUGIN_CALL *create_reminder)(void* ctx, int64_t message_id, int64_t deliver_at);
    /* 取消提醒。需要 modify_message */
    int32_t (ZULIP_PLUGIN_CALL *cancel_reminder)(void* ctx, int64_t reminder_id);
    /* 尚未送达的提醒，需要 read_message：[{"id", "message_id", "content", "rendered_content", "deliver_at", "failed"}]；返回条数 */
    int32_t (ZULIP_PLUGIN_CALL *get_reminders)(void* ctx, zulip_plugin_string_cb cb, void* user);
    /* 草稿，需要 read_message：[{"conv": <conv>, "content", "updated_at": Unix 秒}]；返回条数 */
    int32_t (ZULIP_PLUGIN_CALL *get_drafts)(void* ctx, zulip_plugin_string_cb cb, void* user);
    /* 保存草稿，空字符串表示删除。需要 fill_composer */
    int32_t (ZULIP_PLUGIN_CALL *save_draft)(void* ctx, const zulip_plugin_conversation* conv, const char* markdown);
    /* 会话的免打扰级别 ZULIP_PLUGIN_LEVEL_*。需要 read_message */
    int32_t (ZULIP_PLUGIN_CALL *get_conversation_level)(void* ctx, const zulip_plugin_conversation* conv);
    /* 需要 manage_preferences */
    int32_t (ZULIP_PLUGIN_CALL *set_conversation_level)(void* ctx, const zulip_plugin_conversation* conv, int32_t level);
    /* 在消息页置顶会话。需要 manage_preferences */
    int32_t (ZULIP_PLUGIN_CALL *set_conversation_pinned)(void* ctx, const zulip_plugin_conversation* conv, int32_t pinned);
    /* 静音用户。需要 manage_preferences */
    int32_t (ZULIP_PLUGIN_CALL *set_user_muted)(void* ctx, int64_t user_id, int32_t muted);
    /* 添加（on 非 0）或删除提醒词。需要 manage_preferences */
    int32_t (ZULIP_PLUGIN_CALL *set_alert_word)(void* ctx, const char* word, int32_t on);
    /* 设置自己的签名，NULL 或空字符串表示清除。需要 manage_preferences */
    int32_t (ZULIP_PLUGIN_CALL *set_own_status)(void* ctx, const char* text);
    /* 静音的用户：[用户 id]；返回人数。需要 read_contacts */
    int32_t (ZULIP_PLUGIN_CALL *get_muted_users)(void* ctx, zulip_plugin_string_cb cb, void* user);
    /* 提醒词：["词"]；返回个数。需要 read_message */
    int32_t (ZULIP_PLUGIN_CALL *get_alert_words)(void* ctx, zulip_plugin_string_cb cb, void* user);
    /* 已读这条消息的用户：[用户 id]；返回人数。需要 read_message */
    int32_t (ZULIP_PLUGIN_CALL *get_read_receipts)(void* ctx, int64_t message_id, zulip_plugin_string_cb cb, void* user);
    /* 这条消息上可以执行的操作，需要 read_message：{"can_edit", "can_delete", "can_show_edit_history", "can_show_read_receipts"}。
       只看程序已加载的消息，其他消息 ZULIP_PLUGIN_ERR_NOT_FOUND */
    int32_t (ZULIP_PLUGIN_CALL *get_message_actions)(void* ctx, int64_t message_id, zulip_plugin_string_cb cb, void* user);
    /* 组织信息，需要 read_contacts：{"url", "name", "icon_url", "zulip_version", "feature_level", "max_message_length",
       "max_topic_length", "max_file_upload_size_mib", "empty_topic_display_name", "allow_message_editing",
       "enable_read_receipts", "edit_limit_seconds", "delete_limit_seconds"} */
    int32_t (ZULIP_PLUGIN_CALL *get_realm_info)(void* ctx, zulip_plugin_string_cb cb, void* user);
    /* download_attachment 带进度（progress 可为 NULL，已下载过时不回调）。progress 在调用线程上回调。需要 read_attachment */
    int32_t (ZULIP_PLUGIN_CALL *download_attachment_ex)(void* ctx, const char* upload_path, zulip_plugin_progress_cb progress, zulip_plugin_string_cb cb, void* user);
    /* upload_file 带进度（progress 可为 NULL）。progress 可能在其他线程上回调，函数返回后不再回调。需要 upload_file */
    int32_t (ZULIP_PLUGIN_CALL *upload_file_ex)(void* ctx, const char* local_path, zulip_plugin_progress_cb progress, zulip_plugin_string_cb cb, void* user);
} zulip_plugin_host_api;

/* ---- 插件导出（init 必须，其余可选） ---- */
ZULIP_PLUGIN_EXPORT int32_t ZULIP_PLUGIN_CALL zulip_plugin_init(const zulip_plugin_host_api* api);  /* 返回 ZULIP_PLUGIN_OK 表示成功 */
/* 插件已开启且至少有一个账号登录；与 on_disable 成对 */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_enable(void);
/* 插件被停用，或最后一个账号结束。此后视为所有账号都已结束（不再单独调用 on_account_logout）；
   再次开启时重新调用 on_enable，并为每个已登录账号调用 on_account_login */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_disable(void);
/* 账号已登录。插件在账号登录之后才开启时，开启后为每个已登录账号补发 */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_account_login(int64_t account_id);
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_account_logout(int64_t account_id);
/* 新消息（含自己发送的消息）；BLOCK = 后面的插件不再收到 */
ZULIP_PLUGIN_EXPORT int32_t ZULIP_PLUGIN_CALL zulip_plugin_on_message(const zulip_plugin_message* msg);
/* 收到的消息过滤：BLOCK = 不显示（本次运行内加载历史时也不显示）、不提醒、标记已读；自己发送的消息不经过。
   超时时放行，不能用于合规拦截 */
ZULIP_PLUGIN_EXPORT int32_t ZULIP_PLUGIN_CALL zulip_plugin_filter_incoming(const zulip_plugin_message* msg);
/* 用户发送前调用（文字、文件、转发，见 kind；插件自己发送的消息和定时消息不经过）；
   BLOCK = 取消发送；可以用 set_outgoing_content 改写内容。超时时按原内容发送，不能用于合规拦截 */
ZULIP_PLUGIN_EXPORT int32_t ZULIP_PLUGIN_CALL zulip_plugin_filter_outgoing(const zulip_plugin_outgoing* out);
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_menu(int32_t menu_id);
/* 用户点击了 message_menus 中的菜单项（回调中的宿主函数使用消息所属的账号） */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_message_menu(int32_t menu_id, const zulip_plugin_message* msg);
/* 用户点击了 conversation_menus、user_menus、channel_menus 中的菜单项（回调中的宿主函数使用目标所属的账号）：
   {"account": 账号 id, "kind": "conversation" | "user" | "channel",
    "target": <conv> 或 {"user": 用户 id, "name": "显示名"} 或 {"stream": 频道 id, "name": "显示名"}} */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_context_menu(int32_t menu_id, const char* json);
/* 用户点击了 composer_buttons 中的按钮：{"account": 账号 id, "conv": <conv>, "text": "输入框的文字"}；
   用 fill_composer 替换输入框内容（需要 fill_composer 权限） */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_composer_button(int32_t button_id, const char* json);
/* 聊天窗口中显示了这些消息（需要 read_message）：{"account": 账号 id, "messages": [消息 id]}。
   回调中的宿主函数使用该账号，可以用 set_annotation 重新附加插件保存的结果 */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_messages_shown(const char* json);
/* 用户保存了插件设置（之后 get_settings 返回新值） */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_settings_changed(void);
/* 用户点击了设置中的 button 或 tags 项旁的按钮：
   {"key": "设置项", "row": 行号（list 的行中才有）, "item": "tags 的项（item_button 才有）",
    "values": 所在一层当前的值, "settings": 全部当前的值}（尚未保存） */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_settings_button(const char* json);
/* 其他插件请求本插件发布的资源；在自己的线程上处理后调用 reply_resource：
   {"type", "resource": 资源 id, "item", "request": {...}, "from": "使用者的插件 id"} */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_resource_request(int64_t request_id, const char* json);
/* 宿主需要面板的数据（2 秒内没有回复时不显示），在回调中用 reply_panel 回复：
   {"action": "get" | "set" | "click" | "list" | "remove", "kind": "user" | "topic",
    "target": {"account": 账号 id, "user": 用户 id, "name": "显示名"} 或 {"account", "stream": 频道 id, "topic": "话题", "name": "频道 / 话题"},
    "key": "设置项"（set、click）, "value": 新的值（set）}；list 没有 target。回调中的宿主函数使用 target 所属的账号 */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_panel(const char* json);

/* 通知型事件：在 plugin.json 的 events 中声明后才会收到；都带 "account": 账号 id，回调中的宿主函数使用该账号 */
/* {"message_id", "conv": <conv> 或 null, "edit_timestamp", "content_html", "content_text"} */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_message_edited(const char* json);
/* {"message_ids": [...], "conv": <conv> 或 null} */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_message_deleted(const char* json);
/* {"message_id", "conv": <conv> 或 null, "added": true | false, "reaction": {"emoji_name", "emoji_code", "reaction_type", "user_id"}} */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_reaction_changed(const char* json);
/* {"message_ids": [...], "read": true | false, "all": 全部消息时为 true（此时 message_ids 可能为空）} */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_read_state_changed(const char* json);
/* {"old": <conv>, "new": <conv>, "message_ids": [...], "whole": 整个话题时为 true} */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_topic_moved(const char* json);
/* {"streams": [频道 id], "op": 服务器事件的操作（create、update、add、remove 等）} */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_channel_changed(const char* json);
/* {"user": 用户 id}（资料或签名变化） */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_user_changed(const char* json);
/* {"users": [用户 id]}，用 get_presence 读取 */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_presence_changed(const char* json);
/* {"conv": <conv>} 用户切换到了这个会话 */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_conversation_opened(const char* json);
/* {"state": "connected" | "reconnecting" | "auth_failed"}；恢复后可能错过了事件，需要时重新读取 */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_connection_changed(const char* json);
/* {"conv": <conv>, "user": 用户 id, "typing": true | false}（服务器的输入事件，不含自己和静音用户）；
   对方异常离开时不一定收到 false，需要时自己超时 */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_typing_changed(const char* json);
/* {} 定时消息变化，用 get_scheduled_messages 读取 */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_scheduled_changed(const char* json);
/* {} 提醒变化，用 get_reminders 读取 */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_reminders_changed(const char* json);
/* {"conv": <conv>} 草稿内容变化：关闭同步时本机保存、删除草稿触发，开启同步时服务器同步的变化触发。用 get_drafts 读取 */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_draft_changed(const char* json);
/* {"conv": <conv>, "visibility": ZULIP_PLUGIN_TOPIC_*} 话题的关注状态变化 */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_user_topic_changed(const char* json);
/* {"users": [现在的全部静音用户 id]} */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_muted_users_changed(const char* json);
/* {"properties": ["变化的组织设置名"]}，用 get_realm_info 读取 */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_realm_changed(const char* json);
#ifdef __cplusplus
}
#endif
