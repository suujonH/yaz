/*
 * YaZ native plugin interface.
 *
 * - x64, __cdecl, UTF-8 strings.
 * - The host exports no symbols: the plugin exports zulip_plugin_init; the host passes in a function table, which the plugin stores and uses to call the host.
 *   The ctx in the table identifies "which plugin"; pass it back unchanged on every call. The table remains valid throughout.
 * - Pointers the host hands to the plugin (messages, users, strings) are valid only while the callback runs; copy them if you need them later.
 * - All exported functions are called one after another on the same plugin thread; return quickly and move slow work to your own thread.
 *   A plugin may call host functions from any thread (synchronous, blocking), but not from a tokio runtime thread (for example a worker thread of a runtime the plugin created itself,
 *   or inside block_on): host functions wait on the host's runtime internally, so such a call returns ZULIP_PLUGIN_ERR_UNKNOWN.
 * - Native plugins are not sandboxed; a crash takes down the whole program. The permissions listed in the enable confirmation only constrain the function table: the DLL runs inside the program process
 *   and can bypass the table to do anything, so enabling a plugin means trusting its DLL.
 * - A disabled plugin receives no more events; the DLL is only unloaded after the program restarts. A disabled plugin calling host functions gets ZULIP_PLUGIN_ERR_DISABLED,
 *   except add_log, get_data_dir and get_settings; host functions can still be called while the on_disable callback runs.
 * - Plugin directory: <program directory>\plugins\<id>\, containing plugin.json and the DLL.
 *
 * ABI rules (append-only within API version 1; nothing is changed):
 * - Existing fields of structs and of the function table are never renamed, retyped or reordered; new fields are only appended at the end; values are only added.
 * - The first field of a struct is struct_size (the function table has struct_size too). Before reading or calling a member added later, use
 *   offsetof(type, member) < struct_size to check whether the other side provides it. Structs the plugin passes to the host (conversation) must fill in struct_size.
 * - Nested structs are referenced through pointers, so any struct can grow independently.
 * - Parameters of host functions and exported functions never change; to add a parameter, add a new function. Callback JSON only gains keys; ignore keys you do not recognize.
 *
 * Multiple accounts: several accounts can be logged in at the same time; messages of all accounts are dispatched to plugins, and account_id is the local account id, which is stable across program restarts.
 * Account used by host functions: the account the callback belongs to > the account bound by set_thread_account > the account chosen by select_account >
 * the first account that logged in (the first item of get_accounts). If the account chosen by select_account has logged out, the first logged-in account is used;
 * if the thread-bound account has logged out, ZULIP_PLUGIN_ERR_NOT_LOGGED_IN is returned and no other account is substituted.
 *
 * plugin.json:
 * - id (same as the directory name), name, version, author, description, api_version, library (DLL file name).
 * - permissions: a plugin may only call host functions covered by the permissions it declares (otherwise ZULIP_PLUGIN_ERR_PERMISSION is returned).
 *   read_message, read_contacts, read_attachment, send_message, modify_message, upload_file, annotate_message,
 *   render_message, fill_composer, edit_message, delete_message, manage_channel, manage_user, manage_topic,
 *   open_conversation, notify, launch, manage_subscription, manage_preferences, ui_dialog, zulip_api (high risk),
 *   network (declaration only).
 *   Mapping between permissions and function-table entries:
 *     read_message: get_message, get_messages, search_messages, get_edit_history, get_message_source, get_read_receipts,
 *       get_message_actions, get_conversations, get_current_conversation, get_conversation_level, get_drafts,
 *       get_scheduled_messages, get_reminders, get_alert_words
 *     read_contacts: get_users, get_user, get_presence, get_user_groups, get_muted_users, get_channels, get_all_channels,
 *       get_topics, get_topic_visibility, get_subscribers, get_realm_info
 *     read_attachment: download_attachment, download_attachment_ex
 *     send_message: send_channel_message, send_direct_message, schedule_message, cancel_scheduled_message
 *     modify_message: set_outgoing_content, add_reaction, remove_reaction, set_starred, mark_read, mark_unread,
 *       create_reminder, cancel_reminder
 *     upload_file: upload_file, upload_file_ex
 *     annotate_message: set_annotation (replace additionally needs render_message, fill buttons need fill_composer, target buttons need launch)
 *     render_message: only for the replace field of set_annotation
 *     fill_composer: fill_composer, fill_composer_to, save_draft
 *     edit_message: edit_message
 *     delete_message: delete_message
 *     manage_channel: update_channel
 *     manage_user: update_user
 *     manage_topic: move_topic, set_topic_resolved, delete_topic
 *     open_conversation: open_conversation
 *     notify: show_toast, show_notification, set_status_text
 *     launch: launch
 *     manage_subscription: set_subscribed, set_subscription_property, set_topic_visibility, create_channel
 *     manage_preferences: set_user_muted, set_alert_word, set_own_status, set_conversation_level, set_conversation_pinned
 *     ui_dialog: show_dialog
 *     zulip_api: api_request
 *     network: no corresponding functions, so the host cannot restrict it
 *     No permission needed: get_accounts, get_current_account, select_account, set_thread_account, add_log, get_data_dir,
 *       get_settings, set_settings, set_settings_feedback, get_last_error, reply_panel, publish_resources,
 *       request_resource, reply_resource
 *   Newly installed plugins are disabled by default; when the user enables one, the host lists its permissions, events and launch allowlist and asks for confirmation. The host records fingerprints of all files in
 *   the plugin directory; any file change requires enabling again, so write runtime data to the directory returned by get_data_dir. Fingerprints are also recomputed when enabling at run time;
 *   the manifest and DLL changes re-confirmed that way only take full effect after the program restarts.
 * - events: {"event": priority}, 10000-40000; a lower number runs earlier, and 10000 is monitor-only (cannot intercept or rewrite). Only declared events are received.
 *   on_message and filter_incoming need read_message; filter_outgoing needs modify_message.
 *   Notification events (cannot intercept; the return value is always void; exported functions are listed at the end of this file): on_message_edited, on_message_deleted,
 *   on_reaction_changed, on_read_state_changed, on_topic_moved, on_conversation_opened, on_connection_changed
 *   need read_message; on_typing_changed, on_scheduled_changed, on_reminders_changed, on_draft_changed,
 *   on_user_topic_changed also need read_message; on_channel_changed, on_user_changed, on_presence_changed, on_muted_users_changed,
 *   on_realm_changed need read_contacts.
 *   The host waits at most 2 seconds for an event chain; a plugin that times out 3 times in a row receives no more events for the rest of this run; while a plugin has timed out and not yet returned, new events are skipped.
 *   filter_outgoing and filter_incoming let things through on timeout (content is sent as usual, messages are shown as usual), so they must not be used for compliance blocking.
 * - dependencies: ["id of a plugin depended on"]. Plugins depended on must be enabled first; only resources published by the plugins you depend on can be used. At startup a plugin whose dependency is not enabled
 *   stays inactive and a log entry is written; disabling a plugin also disables the plugins that depend on it (logged), and they are not restored automatically when it is enabled again.
 * - menus: [{"id": 1, "name": "Open", "shortcut": "Ctrl+Shift+T" or ""}], shown in the plugin list; clicking calls zulip_plugin_on_menu.
 *   Writing shortcut means a keyboard shortcut can be bound (needs Ctrl or Alt; F1-F24 may be used alone; "" means no default).
 *   When focus is somewhere text can be typed (such as the composer), the input method receives keys before the program, so key combinations it uses never reach the program and the shortcut does not fire
 *   (for example Ctrl+Shift+E under a Chinese input method). Do not pick combinations commonly used by input methods as defaults.
 * - message_menus: [{"id": 1, "name": "Translate", "shortcut": "middle_click" or ""}], shown in the message context menu; needs read_message,
 *   clicking calls zulip_plugin_on_message_menu. A submenu is written as {"name": "Settings", "children": [...]} (nested at most 2 levels).
 *   shortcut is a mouse gesture on a single message: [ctrl+][alt+][shift+]{left|middle|right}_{click|double};
 *   left_click, left_double, right_click, right_double and shift+left_click are used by the host and cannot be bound.
 * - conversation_menus (needs read_message), user_menus, channel_menus (need read_contacts): context-menu items for conversations, users and channels,
 *   written like message_menus (no shortcut); clicking calls zulip_plugin_on_context_menu.
 * - Items in message_menus and conversation_menus may have a display condition "when": {"conv": "channel" | "direct", "own": sent by yourself,
 *   "has_uploads": has uploaded files, "can_edit": can be edited}; all are optional, and the item is shown only if every given condition holds; unknown conditions are logged and ignored.
 *   A conversation menu has no message, so own, has_uploads and can_edit count as false. A submenu is hidden when none of its children is shown.
 * - composer_buttons: [{"id": 1, "icon": "edit" or "icons/a.svg", "tooltip": "description"}], composer toolbar buttons; need read_message,
 *   clicking calls zulip_plugin_on_composer_button. icon is a built-in icon (add, chat, check, edit, file, folder, history, image, lock,
 *   more, search, smile, time, user) or an SVG file inside the plugin directory.
 * - panels: [{"kind": "topic" | "user", "label": "topic scene"}], at most one of each kind. The host shows a group for the plugin in the topic info panel / user profile card,
 *   and in the plugin settings shows a list titled with label; the data is stored by the plugin and exchanged through zulip_plugin_on_panel and reply_panel.
 * - settings: the host generates the settings UI and stores the values (the plugin can only read them, via get_settings). Each item {"key", "label",
 *   "type": text|password|multiline|select|bool|list|button|tags|resource, "hint", "placeholder", "default",
 *   "options": [{"value", "label"}] (select), "fields": [...] (the settings of each row of a list; list cannot be nested),
 *   "visible_when": {"key of another item at the same level": "value" or ["value", ...]}}. The value of password is stored encrypted by the host.
 *   button has no value: clicking calls zulip_plugin_on_settings_button with the current form values, and the plugin shows the result with set_settings_feedback.
 *   The value of tags is an array of strings; "item_button": "Test" shows a button next to each item; the plugin can supply candidates with set_settings_feedback.
 *   resource selects a resource published by a plugin depended on, "plugin": "publisher id", "resource": "resource type";
 *   the value is {"type", "plugin", "resource", "item"}, or {} when nothing is selected.
 * - launch: allowlist; the user is told when enabling, and launch() can only open targets on the allowlist. Syntax: "todo:" (a protocol; cannot be http/https/file etc.),
 *   "https://example.com/path" (URL prefix; the host may start with *. to match subdomains), "C:\Tools\a.exe" (full path),
 *   "notepad.exe" (only matches targets that are also given as a bare file name). Targets outside the allowlist should be put in a set_annotation button, which the user clicks to open.
 *
 * Shared resources between plugins: the host is only a channel; the resource type (lowercase letters, digits, underscore) and the request / reply formats are agreed by the publisher. The publisher uses
 * publish_resources to publish and exports zulip_plugin_on_resource_request; consumers declare the publisher in dependencies and, on their own thread,
 * call request_resource; the publisher replies with reply_resource.
 *
 * Conversation in callback and query JSON (written <conv> below): {"type": "channel", "stream": channel id, "stream_name", "topic"} or
 * {"type": "direct", "users": [other user id]} (an empty array for a DM with yourself). conv is null for direct-message events whose message is not in the cache.
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

/* Error codes: a host function returns a negative number on failure */
#define ZULIP_PLUGIN_OK                 0
#define ZULIP_PLUGIN_ERR_UNKNOWN       -1
#define ZULIP_PLUGIN_ERR_INVALID_ARG   -2
#define ZULIP_PLUGIN_ERR_PERMISSION    -3   /* plugin.json does not declare the required permission */
#define ZULIP_PLUGIN_ERR_NOT_LOGGED_IN -4
#define ZULIP_PLUGIN_ERR_NETWORK       -5
#define ZULIP_PLUGIN_ERR_SERVER        -6   /* Zulip returned an error; see the log for details */
#define ZULIP_PLUGIN_ERR_NOT_FOUND     -7
#define ZULIP_PLUGIN_ERR_WRONG_CONTEXT -8   /* e.g. calling set_outgoing_content outside filter_outgoing */
#define ZULIP_PLUGIN_ERR_UNSUPPORTED   -9   /* host too old, function missing (returned by the SDK wrapper; C plugins check with offsetof) */
#define ZULIP_PLUGIN_ERR_DISABLED      -10  /* plugin is disabled */

#define ZULIP_PLUGIN_LOG_DEBUG 0
#define ZULIP_PLUGIN_LOG_INFO 10
#define ZULIP_PLUGIN_LOG_INFO_SUCCESS 11
#define ZULIP_PLUGIN_LOG_INFO_RECV 12
#define ZULIP_PLUGIN_LOG_INFO_SEND 13
#define ZULIP_PLUGIN_LOG_WARNING 20
#define ZULIP_PLUGIN_LOG_ERROR 30
#define ZULIP_PLUGIN_LOG_FATAL 40

#define ZULIP_PLUGIN_CONV_CHANNEL 1   /* channel topic */
#define ZULIP_PLUGIN_CONV_DIRECT  2   /* direct message (including group DMs and DM with self) */

/* zulip_plugin_message.flags */
#define ZULIP_PLUGIN_MSG_OUTGOING          0x01  /* sent by yourself */
#define ZULIP_PLUGIN_MSG_MENTIONED         0x02  /* mentions me (including user groups) */
#define ZULIP_PLUGIN_MSG_WILDCARD_MENTION  0x04  /* @all / @topic wildcard mention */
#define ZULIP_PLUGIN_MSG_STARRED           0x08
#define ZULIP_PLUGIN_MSG_READ              0x10
#define ZULIP_PLUGIN_MSG_HISTORICAL        0x20  /* obtained via get_message(s) or search, not from a new-message event */

/* zulip_plugin_outgoing.kind */
#define ZULIP_PLUGIN_OUTGOING_TEXT    0   /* text in the composer */
#define ZULIP_PLUGIN_OUTGOING_FILE    1   /* sending a file (content is Markdown with the upload link) */
#define ZULIP_PLUGIN_OUTGOING_FORWARD 2   /* forward */

/* mode of fill_composer */
#define ZULIP_PLUGIN_FILL_INSERT  0   /* insert at the cursor */
#define ZULIP_PLUGIN_FILL_REPLACE 1   /* replace existing content */

/* return values of get_topic_visibility */
#define ZULIP_PLUGIN_TOPIC_INHERIT 0
#define ZULIP_PLUGIN_TOPIC_MUTED   1
#define ZULIP_PLUGIN_TOPIC_UNMUTED 2
#define ZULIP_PLUGIN_TOPIC_FOLLOWED 3

/* levels of get_conversation_level and set_conversation_level */
#define ZULIP_PLUGIN_LEVEL_NOTIFY    0   /* message notifications allowed */
#define ZULIP_PLUGIN_LEVEL_SILENT    1   /* receive messages without notification */
#define ZULIP_PLUGIN_LEVEL_ASSISTANT 2   /* moved into the topic assistant, no notification */
#define ZULIP_PLUGIN_LEVEL_BLOCKED   3   /* blocked: not shown, no notification, not counted as unread */

typedef struct zulip_plugin_conversation {
    uint32_t struct_size;
    int32_t type;                 /* ZULIP_PLUGIN_CONV_* */
    int64_t stream_id;            /* channel: channel id */
    const char* stream_name;      /* channel: channel name; NULL for a DM */
    const char* topic;            /* channel: topic name; NULL for a DM */
    const int64_t* dm_user_ids;   /* DM: array of the other users' ids (ascending); NULL for a channel */
    int32_t dm_user_count;        /* DM: number of other users (excluding yourself); 0 for a DM with yourself */
} zulip_plugin_conversation;

typedef struct zulip_plugin_message {
    uint32_t struct_size;
    uint32_t flags;               /* ZULIP_PLUGIN_MSG_* */
    int64_t message_id;
    int64_t account_id;           /* account the message belongs to */
    int64_t timestamp;            /* Unix seconds */
    int64_t sender_id;
    const char* sender_name;
    const char* sender_email;
    const char* content_html;     /* HTML rendered by the server */
    const char* content_text;     /* plain text converted by the host */
    const zulip_plugin_conversation* conv;
    const char* reactions_json;   /* [{"emoji_name", "emoji_code", "reaction_type", "user_id"}] */
    const char* uploads_json;     /* [{"path": "/user_uploads/...", "file_name", "is_image"}] */
    int64_t last_edit_timestamp;  /* Unix seconds; 0 if never edited */
} zulip_plugin_message;

typedef struct zulip_plugin_outgoing {
    uint32_t struct_size;
    int32_t kind;                 /* ZULIP_PLUGIN_OUTGOING_* */
    int64_t account_id;           /* account sending the message */
    const zulip_plugin_conversation* conv;
    const char* content;
} zulip_plugin_outgoing;

typedef struct zulip_plugin_user {
    uint32_t struct_size;
    int32_t role;                 /* 100 owner ... 600 guest */
    int64_t user_id;
    const char* full_name;
    const char* email;
    const char* remark;           /* local remark, may be NULL */
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
    int64_t account_id;           /* local account id */
    int64_t user_id;              /* user id of this account in its organization */
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
/* Progress: done and total are in bytes, total is 0 if unknown. user is the same as for the result callback */
typedef void (ZULIP_PLUGIN_CALL *zulip_plugin_progress_cb)(int64_t done, int64_t total, void* user);

typedef struct zulip_plugin_host_api {
    uint32_t struct_size;
    uint32_t api_version;          /* = ZULIP_PLUGIN_API_VERSION */
    void* ctx;                     /* passed back unchanged on every call */
    void    (ZULIP_PLUGIN_CALL *add_log)(void* ctx, int32_t level, const char* category, const char* text);
    /* Logged-in accounts; the first item is the account that logged in first; returns the number of accounts */
    int32_t (ZULIP_PLUGIN_CALL *get_accounts)(void* ctx, zulip_plugin_account_cb cb, void* user);
    /* The account host functions use right now; ZULIP_PLUGIN_ERR_NOT_LOGGED_IN if no account is logged in */
    int64_t (ZULIP_PLUGIN_CALL *get_current_account)(void* ctx);
    /* Select the account used outside callbacks; when called inside a callback, the remaining calls of that callback also use it. ZULIP_PLUGIN_ERR_NOT_FOUND if the account is not logged in */
    int32_t (ZULIP_PLUGIN_CALL *select_account)(void* ctx, int64_t account_id);
    int32_t (ZULIP_PLUGIN_CALL *get_data_dir)(void* ctx, zulip_plugin_string_cb cb, void* user);  /* ends with a backslash */
    int32_t (ZULIP_PLUGIN_CALL *get_settings)(void* ctx, zulip_plugin_string_cb cb, void* user);  /* JSON object (key -> value) */
    /* Reply to the settings window (any thread; ignored if the window is already closed):
       {"key": "setting key", "row": row number received by zulip_plugin_on_settings_button (omit at top level), "text": "text shown below the setting",
        "options": ["candidates for a tags setting"]};
       the result of a button next to a tags item is written as {"key", "row"?, "item": "that item", "status": "ok" | "error", "text": "reason for failure"} */
    int32_t (ZULIP_PLUGIN_CALL *set_settings_feedback)(void* ctx, const char* json);
    /* Needs send_message; returns the message id (>0). A count of 0 in send_direct_message means a DM with yourself */
    int64_t (ZULIP_PLUGIN_CALL *send_channel_message)(void* ctx, int64_t stream_id, const char* topic, const char* markdown);
    int64_t (ZULIP_PLUGIN_CALL *send_direct_message)(void* ctx, const int64_t* user_ids, int32_t count, const char* markdown);
    /* Needs read_message. get_messages: conv NULL means all; anchor: 0 = newest, -1 = oldest, anything else = message id (inclusive); returns the number of messages */
    int32_t (ZULIP_PLUGIN_CALL *get_message)(void* ctx, int64_t message_id, zulip_plugin_message_cb cb, void* user);
    int32_t (ZULIP_PLUGIN_CALL *get_messages)(void* ctx, const zulip_plugin_conversation* conv, int64_t anchor,
                                              int32_t num_before, int32_t num_after, zulip_plugin_message_cb cb, void* user);
    /* Needs read_contacts */
    int32_t (ZULIP_PLUGIN_CALL *get_users)(void* ctx, zulip_plugin_user_cb cb, void* user);
    int32_t (ZULIP_PLUGIN_CALL *get_user)(void* ctx, int64_t user_id, zulip_plugin_user_cb cb, void* user);
    int32_t (ZULIP_PLUGIN_CALL *get_channels)(void* ctx, zulip_plugin_channel_cb cb, void* user);  /* subscribed channels */
    int32_t (ZULIP_PLUGIN_CALL *get_topics)(void* ctx, int64_t stream_id, zulip_plugin_topic_cb cb, void* user);
    /* Needs modify_message */
    int32_t (ZULIP_PLUGIN_CALL *add_reaction)(void* ctx, int64_t message_id, const char* emoji_name);
    int32_t (ZULIP_PLUGIN_CALL *set_starred)(void* ctx, int64_t message_id, int32_t starred);
    int32_t (ZULIP_PLUGIN_CALL *mark_read)(void* ctx, int64_t message_id);
    /* Needs upload_file; the callback receives /user_uploads/... */
    int32_t (ZULIP_PLUGIN_CALL *upload_file)(void* ctx, const char* local_path, zulip_plugin_string_cb cb, void* user);
    /* Needs modify_message; only valid inside zulip_plugin_filter_outgoing */
    int32_t (ZULIP_PLUGIN_CALL *set_outgoing_content)(void* ctx, const char* markdown);
    /* Attach content to a message (does not modify the original; only valid for the current run and hidden when the plugin is disabled; if it must be kept, the plugin saves it and re-attaches it in
       zulip_plugin_on_messages_shown):
       {"text": "text below the bubble, restricted Markdown supported (bold, italic, inline code, code blocks, lists, headings, http(s) links; everything else is escaped)",
        "replace": "Zulip Markdown that temporarily replaces what the bubble shows (rendered by the server like a message: quotes, code blocks, lists, @mentions, emoji; plain text until it is rendered)",
        "actions": [{"label": "button", "target": "URI or program", "args": "program arguments", "style": "primary" | "secondary"} or
                    {"label": "button", "fill": "text to put into the composer", "style": ...}],
        "tags": [{"text": "short label", "tone": "neutral" | "success" | "warning" | "danger" | "info" | "accent", "tip": "hover text"}],
        "title": "headline in bold",
        "fields": [{"label": "name", "value": "plain text", "items": ["short label shown as a chip", ...], "tone": same as tags (color of the chips)}],
        "replies_title": "heading above the candidate replies",
        "replies": [{"label": "#1", "fill": "candidate text, shown in full; clicking it appends it to the end of the composer, replacing the candidate chosen before (not added twice)"}],
        "tools": [{"menu": 2, "icon": "refresh", "tooltip": "hover text"}]}
       tools are icon buttons in the card header (icon names as composer_buttons, plus refresh, delete and close; a tool with icon close is the X at the right of the card header, which otherwise removes the card for this run); clicking calls zulip_plugin_on_message_menu
       with that menu id and this message (the id need not be declared in message_menus).
       (at most 5 buttons, 6 tags, 12 fields with at most 8 items each, 5 replies, 3 tools; shown in the order tags, title, fields, text, replies, buttons); a json of NULL or an empty string clears it, and calling again overwrites this plugin's previous content. One card per plugin on a message.
       Needs annotate_message; replace also needs render_message, fill buttons need fill_composer, target buttons need launch */
    int32_t (ZULIP_PLUGIN_CALL *set_annotation)(void* ctx, int64_t message_id, const char* json);
    /* Open a URI on the allowlist or run a program on the allowlist (args is for programs only, may be NULL); ZULIP_PLUGIN_ERR_PERMISSION if not on the allowlist. Needs launch */
    int32_t (ZULIP_PLUGIN_CALL *launch)(void* ctx, const char* target, const char* args);
    int32_t (ZULIP_PLUGIN_CALL *edit_message)(void* ctx, int64_t message_id, const char* markdown);  /* Needs edit_message */
    int32_t (ZULIP_PLUGIN_CALL *delete_message)(void* ctx, int64_t message_id);                      /* Needs delete_message */
    /* Change channel name / description, NULL means leave unchanged. Needs manage_channel */
    int32_t (ZULIP_PLUGIN_CALL *update_channel)(void* ctx, int64_t stream_id, const char* name, const char* description);
    /* Change user name / role (100 owner, 200 administrator, 300 moderator, 400 member, 600 guest), NULL / 0 means leave unchanged. Needs manage_user */
    int32_t (ZULIP_PLUGIN_CALL *update_user)(void* ctx, int64_t user_id, const char* full_name, int32_t role);
    /* Publish resources of type type, replacing those published before; a json of NULL or an empty string clears them. Kept in memory only, so publish in init and whenever settings change.
       json: [{"id", "name", "items": ["optional items"]}] */
    int32_t (ZULIP_PLUGIN_CALL *publish_resources)(void* ctx, const char* type, const char* json);
    /* Use another plugin's resource: {"resource": value of the resource setting, "request": {...}}; blocks until the publisher replies (cb is called once) or times out (120 seconds,
       ZULIP_PLUGIN_ERR_NETWORK). ZULIP_PLUGIN_ERR_NOT_FOUND if the resource does not exist or the publisher is not enabled; must not be called inside a callback (on the plugin thread)
       (ZULIP_PLUGIN_ERR_WRONG_CONTEXT). ZULIP_PLUGIN_ERR_PERMISSION if the publisher is not in dependencies */
    int32_t (ZULIP_PLUGIN_CALL *request_resource)(void* ctx, const char* json, zulip_plugin_string_cb cb, void* user);
    /* Reply to a request received by zulip_plugin_on_resource_request (any thread); ZULIP_PLUGIN_ERR_NOT_FOUND if the request does not exist or has timed out */
    int32_t (ZULIP_PLUGIN_CALL *reply_resource)(void* ctx, int64_t request_id, const char* json);
    /* Reply to zulip_plugin_on_panel (may only be called inside that callback, otherwise ZULIP_PLUGIN_ERR_WRONG_CONTEXT; calling again overwrites).
       get / set / click: {"fields": [field]} (the group is not shown when there are no settings); a field is one of:
         {"key", "label", "type": "select", "options": [{"value", "label"}], "value": "selected value"}
         {"key", "label", "type": "bool", "value": true}
         {"key", "label", "type": "text", "value": "text", "placeholder": "hint"}
         {"key", "label", "type": "button"} (action is click when it is clicked)
         {"key", "label", "type": "info", "value": "read-only text"}
       list: {"entries": [{"target": same as target in zulip_plugin_on_panel, "text": "description shown after the name"}]}; remove needs no reply */
    int32_t (ZULIP_PLUGIN_CALL *reply_panel)(void* ctx, const char* json);
    /* Search messages (newest first), needs read_message. query:
       {"conv": <conv> (search only this conversation), "starred": true (starred only), "sender": user id, "text": "keywords",
         "filter": "all" | "plain" | "media" | "files" | "attachments" | "links", "until": "ISO 8601, only this time and earlier",
         "page_size": 1-200 (default 50), "cursor": next_cursor of the previous page}, all optional.
       next_cursor (may be NULL) receives the position of the next page, 0 means there is no next page; returns the number of messages on this page */
    int32_t (ZULIP_PLUGIN_CALL *search_messages)(void* ctx, const char* query, int64_t* next_cursor, zulip_plugin_message_cb cb, void* user);
    /* Conversations and unread counts of the messages page, needs read_message:
       {"total_unread": n, "conversations": [{"conv": <conv>, "title", "preview", "time": Unix seconds, "unread", "pinned"}]} */
    int32_t (ZULIP_PLUGIN_CALL *get_conversations)(void* ctx, zulip_plugin_string_cb cb, void* user);
    /* The conversation <conv> the user last switched to; ZULIP_PLUGIN_ERR_NOT_FOUND if none is open. Needs read_message */
    int32_t (ZULIP_PLUGIN_CALL *get_current_conversation)(void* ctx, zulip_plugin_string_cb cb, void* user);
    /* Presence and status text; all active users when user_id is 0. Needs read_contacts:
       [{"user": id, "presence": "online" | "idle" | "offline", "status_text": "status" or null}] */
    int32_t (ZULIP_PLUGIN_CALL *get_presence)(void* ctx, int64_t user_id, zulip_plugin_string_cb cb, void* user);
    /* User groups, needs read_contacts: [{"id", "name", "description", "members": [user id], "direct_subgroup_ids", "is_system_group", "deactivated"}] */
    int32_t (ZULIP_PLUGIN_CALL *get_user_groups)(void* ctx, zulip_plugin_string_cb cb, void* user);
    /* All channels visible in the organization (including unsubscribed ones, see is_subscribed). Needs read_contacts */
    int32_t (ZULIP_PLUGIN_CALL *get_all_channels)(void* ctx, zulip_plugin_channel_cb cb, void* user);
    /* Follow state of a topic, ZULIP_PLUGIN_TOPIC_*. Needs read_contacts */
    int32_t (ZULIP_PLUGIN_CALL *get_topic_visibility)(void* ctx, int64_t stream_id, const char* topic);
    /* Edit history on the server (the first item is the original version), needs read_message:
       [{"topic", "prev_topic", "stream", "prev_stream", "content", "rendered_content", "prev_content", "prev_rendered_content",
         "content_html_diff", "user_id", "timestamp"}] */
    int32_t (ZULIP_PLUGIN_CALL *get_edit_history)(void* ctx, int64_t message_id, zulip_plugin_string_cb cb, void* user);
    /* Remove your own reaction. Needs modify_message */
    int32_t (ZULIP_PLUGIN_CALL *remove_reaction)(void* ctx, int64_t message_id, const char* emoji_name);
    /* Mark this message and all later messages in the conversation as unread. Needs modify_message */
    int32_t (ZULIP_PLUGIN_CALL *mark_unread)(void* ctx, int64_t message_id);
    /* Move or rename a whole topic; new_stream_id 0 means the same channel. ZULIP_PLUGIN_ERR_SERVER if some messages are beyond the move time limit. Needs manage_topic */
    int32_t (ZULIP_PLUGIN_CALL *move_topic)(void* ctx, int64_t stream_id, const char* topic, int64_t new_stream_id, const char* new_topic);
    /* Mark a topic resolved / unresolved. Needs manage_topic */
    int32_t (ZULIP_PLUGIN_CALL *set_topic_resolved)(void* ctx, int64_t stream_id, const char* topic, int32_t resolved);
    /* Delete a topic and all its messages. Needs manage_topic */
    int32_t (ZULIP_PLUGIN_CALL *delete_topic)(void* ctx, int64_t stream_id, const char* topic);
    /* Switch the UI to the conversation; when message_id is not 0, jump to that message. Needs open_conversation */
    int32_t (ZULIP_PLUGIN_CALL *open_conversation)(void* ctx, const zulip_plugin_conversation* conv, int64_t message_id);
    /* Put text into the composer of the current conversation of the current account (does not send); mode is ZULIP_PLUGIN_FILL_*. Needs fill_composer */
    int32_t (ZULIP_PLUGIN_CALL *fill_composer)(void* ctx, const char* text, int32_t mode);
    /* Scheduled send, deliver_at is Unix seconds, returns the scheduled message id. Needs send_message */
    int64_t (ZULIP_PLUGIN_CALL *schedule_message)(void* ctx, const zulip_plugin_conversation* conv, const char* markdown, int64_t deliver_at);
    /* In-app toast. Needs notify */
    int32_t (ZULIP_PLUGIN_CALL *show_toast)(void* ctx, const char* text);
    /* System notification, body may be NULL. Needs notify */
    int32_t (ZULIP_PLUGIN_CALL *show_notification)(void* ctx, const char* title, const char* body);
    /* Text shown where the main window shows the connection status; NULL or an empty string clears it; with several plugins the last one set is shown. Needs notify */
    int32_t (ZULIP_PLUGIN_CALL *set_status_text)(void* ctx, const char* text);
    /* Download an attachment (/user_uploads/...) to the cache and pass the local path to the callback; not downloaded again if already present. Needs read_attachment */
    int32_t (ZULIP_PLUGIN_CALL *download_attachment)(void* ctx, const char* upload_path, zulip_plugin_string_cb cb, void* user);
    /* Call the Zulip API as the current account; the callback receives the server's JSON reply. Needs zulip_api (high risk: the user is explicitly warned when enabling).
       method: GET, POST, PUT, PATCH, DELETE; path must be under /api/v1/, without a query string, and must not touch API-key related paths (otherwise ZULIP_PLUGIN_ERR_PERMISSION);
       params: a JSON object or NULL; string values are sent as-is, other values are JSON-encoded. Every call is written to the plugin log */
    int32_t (ZULIP_PLUGIN_CALL *api_request)(void* ctx, const char* method, const char* path, const char* params, zulip_plugin_string_cb cb, void* user);

    /* ---- Functions appended after the v1 base table: before calling, check offsetof(zulip_plugin_host_api, function) < struct_size ---- */

    /* Bind the account used by this thread (the calling thread), 0 unbinds. ZULIP_PLUGIN_ERR_NOT_FOUND if the account is not logged in. See the top of this file for precedence */
    int32_t (ZULIP_PLUGIN_CALL *set_thread_account)(void* ctx, int64_t account_id);
    /* Details of this thread's last failure; an empty string if it never failed */
    int32_t (ZULIP_PLUGIN_CALL *get_last_error)(void* ctx, zulip_plugin_string_cb cb, void* user);
    /* Markdown source of the message. Needs read_message */
    int32_t (ZULIP_PLUGIN_CALL *get_message_source)(void* ctx, int64_t message_id, zulip_plugin_string_cb cb, void* user);
    /* Scheduled messages, needs read_message: [{"id", "conv": <conv>, "content", "rendered_content", "deliver_at": Unix seconds, "failed"}]; returns the count */
    int32_t (ZULIP_PLUGIN_CALL *get_scheduled_messages)(void* ctx, zulip_plugin_string_cb cb, void* user);
    /* Cancel a scheduled message. Needs send_message */
    int32_t (ZULIP_PLUGIN_CALL *cancel_scheduled_message)(void* ctx, int64_t scheduled_id);
    /* Show a dialog and block until the user acts, needs ui_dialog; must not be called inside a callback (on the plugin thread) (ZULIP_PLUGIN_ERR_WRONG_CONTEXT).
       json: {"kind": "confirm" | "input" | "form", "title", "text", "fields": [same fields as reply_panel]}.
       confirm does not use fields; input takes the first text field in fields (key is "value" if there is none); form needs at least one field.
       A button field does not close the dialog when clicked, and its key is true in the result's values; info fields do not appear in values.
       cb receives {"ok": the user clicked OK, "values": {"key": value}}. ZULIP_PLUGIN_ERR_NOT_FOUND if there is no result after 5 minutes */
    int32_t (ZULIP_PLUGIN_CALL *show_dialog)(void* ctx, const char* json, zulip_plugin_string_cb cb, void* user);
    /* Change this plugin's settings: json is {"key": value}, merged into the existing settings; keys not in the manifest are ignored; password is encrypted as usual.
       Does not call back zulip_plugin_on_settings_changed. No permission needed */
    int32_t (ZULIP_PLUGIN_CALL *set_settings)(void* ctx, const char* json);
    /* Put text into the composer of the given conversation (does not send); mode is ZULIP_PLUGIN_FILL_*. ZULIP_PLUGIN_ERR_NOT_FOUND if the channel does not exist. Needs fill_composer */
    int32_t (ZULIP_PLUGIN_CALL *fill_composer_to)(void* ctx, const zulip_plugin_conversation* conv, const char* text, int32_t mode);
    /* Set the follow state of a topic, ZULIP_PLUGIN_TOPIC_*. Needs manage_subscription */
    int32_t (ZULIP_PLUGIN_CALL *set_topic_visibility)(void* ctx, int64_t stream_id, const char* topic, int32_t visibility);
    /* Subscribe (subscribed non-zero) to or unsubscribe from a channel; ZULIP_PLUGIN_ERR_NOT_FOUND if the channel is not visible. Needs manage_subscription */
    int32_t (ZULIP_PLUGIN_CALL *set_subscribed)(void* ctx, int64_t stream_id, int32_t subscribed);
    /* Change a personal setting of a channel: name is a Zulip property name (color, is_muted, desktop_notifications, etc.), json is the JSON of the new value. Needs manage_subscription */
    int32_t (ZULIP_PLUGIN_CALL *set_subscription_property)(void* ctx, int64_t stream_id, const char* name, const char* json);
    /* Create a channel: {"name", "description", "private": false, "subscribers": [user id]}, only name is required; returns the channel id. Needs manage_subscription */
    int64_t (ZULIP_PLUGIN_CALL *create_channel)(void* ctx, const char* json);
    /* Subscribers of a channel: [user id]; returns the count. Needs read_contacts */
    int32_t (ZULIP_PLUGIN_CALL *get_subscribers)(void* ctx, int64_t stream_id, zulip_plugin_string_cb cb, void* user);
    /* Create a reminder for a message, deliver_at is Unix seconds, returns the reminder id. Needs modify_message */
    int64_t (ZULIP_PLUGIN_CALL *create_reminder)(void* ctx, int64_t message_id, int64_t deliver_at);
    /* Cancel a reminder. Needs modify_message */
    int32_t (ZULIP_PLUGIN_CALL *cancel_reminder)(void* ctx, int64_t reminder_id);
    /* Reminders not yet delivered, needs read_message: [{"id", "message_id", "content", "rendered_content", "deliver_at", "failed"}]; returns the count */
    int32_t (ZULIP_PLUGIN_CALL *get_reminders)(void* ctx, zulip_plugin_string_cb cb, void* user);
    /* Drafts, needs read_message: [{"conv": <conv>, "content", "updated_at": Unix seconds}]; returns the count */
    int32_t (ZULIP_PLUGIN_CALL *get_drafts)(void* ctx, zulip_plugin_string_cb cb, void* user);
    /* Save a draft; an empty string deletes it. Needs fill_composer */
    int32_t (ZULIP_PLUGIN_CALL *save_draft)(void* ctx, const zulip_plugin_conversation* conv, const char* markdown);
    /* Do-not-disturb level of a conversation, ZULIP_PLUGIN_LEVEL_*. Needs read_message */
    int32_t (ZULIP_PLUGIN_CALL *get_conversation_level)(void* ctx, const zulip_plugin_conversation* conv);
    /* Needs manage_preferences */
    int32_t (ZULIP_PLUGIN_CALL *set_conversation_level)(void* ctx, const zulip_plugin_conversation* conv, int32_t level);
    /* Pin a conversation on the messages page. Needs manage_preferences */
    int32_t (ZULIP_PLUGIN_CALL *set_conversation_pinned)(void* ctx, const zulip_plugin_conversation* conv, int32_t pinned);
    /* Mute a user. Needs manage_preferences */
    int32_t (ZULIP_PLUGIN_CALL *set_user_muted)(void* ctx, int64_t user_id, int32_t muted);
    /* Add (on non-zero) or remove an alert word. Needs manage_preferences */
    int32_t (ZULIP_PLUGIN_CALL *set_alert_word)(void* ctx, const char* word, int32_t on);
    /* Set your own status text; NULL or an empty string clears it. Needs manage_preferences */
    int32_t (ZULIP_PLUGIN_CALL *set_own_status)(void* ctx, const char* text);
    /* Muted users: [user id]; returns the count. Needs read_contacts */
    int32_t (ZULIP_PLUGIN_CALL *get_muted_users)(void* ctx, zulip_plugin_string_cb cb, void* user);
    /* Alert words: ["word"]; returns the count. Needs read_message */
    int32_t (ZULIP_PLUGIN_CALL *get_alert_words)(void* ctx, zulip_plugin_string_cb cb, void* user);
    /* Users who have read this message: [user id]; returns the count. Needs read_message */
    int32_t (ZULIP_PLUGIN_CALL *get_read_receipts)(void* ctx, int64_t message_id, zulip_plugin_string_cb cb, void* user);
    /* Actions available on this message, needs read_message: {"can_edit", "can_delete", "can_show_edit_history", "can_show_read_receipts"}.
       Only messages already loaded by the program are considered; for other messages ZULIP_PLUGIN_ERR_NOT_FOUND */
    int32_t (ZULIP_PLUGIN_CALL *get_message_actions)(void* ctx, int64_t message_id, zulip_plugin_string_cb cb, void* user);
    /* Organization (realm) info, needs read_contacts: {"url", "name", "icon_url", "zulip_version", "feature_level", "max_message_length",
       "max_topic_length", "max_file_upload_size_mib", "empty_topic_display_name", "allow_message_editing",
       "enable_read_receipts", "edit_limit_seconds", "delete_limit_seconds"} */
    int32_t (ZULIP_PLUGIN_CALL *get_realm_info)(void* ctx, zulip_plugin_string_cb cb, void* user);
    /* download_attachment with progress (progress may be NULL and is not called if already downloaded). progress is called back on the calling thread. Needs read_attachment */
    int32_t (ZULIP_PLUGIN_CALL *download_attachment_ex)(void* ctx, const char* upload_path, zulip_plugin_progress_cb progress, zulip_plugin_string_cb cb, void* user);
    /* upload_file with progress (progress may be NULL). progress may be called on another thread and is not called after the function returns. Needs upload_file */
    int32_t (ZULIP_PLUGIN_CALL *upload_file_ex)(void* ctx, const char* local_path, zulip_plugin_progress_cb progress, zulip_plugin_string_cb cb, void* user);
} zulip_plugin_host_api;

/* ---- Plugin exports (init is required, the rest are optional) ---- */
ZULIP_PLUGIN_EXPORT int32_t ZULIP_PLUGIN_CALL zulip_plugin_init(const zulip_plugin_host_api* api);  /* returns ZULIP_PLUGIN_OK on success */
/* The plugin is enabled and at least one account is logged in; paired with on_disable */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_enable(void);
/* The plugin is disabled, or the last account ended. From then on all accounts count as ended (on_account_logout is no longer called individually);
   when enabled again, on_enable is called again and on_account_login is called for every logged-in account */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_disable(void);
/* Account logged in. If the plugin is enabled after the account logged in, it is called after enabling for every logged-in account */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_account_login(int64_t account_id);
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_account_logout(int64_t account_id);
/* New message (including messages you sent); BLOCK = later plugins no longer receive it */
ZULIP_PLUGIN_EXPORT int32_t ZULIP_PLUGIN_CALL zulip_plugin_on_message(const zulip_plugin_message* msg);
/* Filter for incoming messages: BLOCK = not shown (also not shown when history is loaded during this run), no notification, marked as read; messages you sent do not pass through it.
   Lets the message through on timeout; must not be used for compliance blocking */
ZULIP_PLUGIN_EXPORT int32_t ZULIP_PLUGIN_CALL zulip_plugin_filter_incoming(const zulip_plugin_message* msg);
/* Called before the user sends (text, file, forward, see kind; messages sent by plugins themselves and scheduled messages do not pass through);
   BLOCK = cancel sending; the content can be rewritten with set_outgoing_content. Sent unchanged on timeout; must not be used for compliance blocking */
ZULIP_PLUGIN_EXPORT int32_t ZULIP_PLUGIN_CALL zulip_plugin_filter_outgoing(const zulip_plugin_outgoing* out);
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_menu(int32_t menu_id);
/* The user clicked an item of message_menus (host functions inside the callback use the message's account) */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_message_menu(int32_t menu_id, const zulip_plugin_message* msg);
/* The user clicked an item of conversation_menus, user_menus or channel_menus (host functions inside the callback use the target's account):
   {"account": account id, "kind": "conversation" | "user" | "channel",
    "target": <conv> or {"user": user id, "name": "display name"} or {"stream": channel id, "name": "display name"}} */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_context_menu(int32_t menu_id, const char* json);
/* The user clicked a button of composer_buttons: {"account": account id, "conv": <conv>, "text": "text in the composer"};
   use fill_composer to replace the composer content (needs the fill_composer permission) */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_composer_button(int32_t button_id, const char* json);
/* These messages are now shown in the chat window (needs read_message): {"account": account id, "messages": [message id]}.
   Host functions inside the callback use that account; set_annotation can re-attach the results the plugin saved */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_messages_shown(const char* json);
/* The user saved the plugin settings (get_settings returns the new values afterwards) */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_settings_changed(void);
/* The user clicked a button setting or the button next to a tags item:
   {"key": "setting key", "row": row number (only for rows of a list), "item": "item of tags (only for item_button)",
    "values": current values of the level it is on, "settings": all current values} (not saved yet) */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_settings_button(const char* json);
/* Another plugin requests a resource published by this plugin; handle it on your own thread and then call reply_resource:
   {"type", "resource": resource id, "item", "request": {...}, "from": "id of the consuming plugin"} */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_resource_request(int64_t request_id, const char* json);
/* The host needs the panel data (the panel is not shown if there is no reply within 2 seconds); reply with reply_panel inside the callback:
   {"action": "get" | "set" | "click" | "list" | "remove", "kind": "user" | "topic",
    "target": {"account": account id, "user": user id, "name": "display name"} or {"account", "stream": channel id, "topic": "topic", "name": "channel / topic"},
    "key": "setting key" (set, click), "value": new value (set)}; list has no target. Host functions inside the callback use the target's account */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_panel(const char* json);

/* Notification events: only received if declared in the events of plugin.json; all carry "account": account id, and host functions inside the callback use that account */
/* {"message_id", "conv": <conv> or null, "edit_timestamp", "content_html", "content_text"} */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_message_edited(const char* json);
/* {"message_ids": [...], "conv": <conv> or null} */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_message_deleted(const char* json);
/* {"message_id", "conv": <conv> or null, "added": true | false, "reaction": {"emoji_name", "emoji_code", "reaction_type", "user_id"}} */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_reaction_changed(const char* json);
/* {"message_ids": [...], "read": true | false, "all": true when it applies to all messages (message_ids may then be empty)} */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_read_state_changed(const char* json);
/* {"old": <conv>, "new": <conv>, "message_ids": [...], "whole": true when the whole topic moved} */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_topic_moved(const char* json);
/* {"streams": [channel id], "op": operation of the server event (create, update, add, remove, etc.)} */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_channel_changed(const char* json);
/* {"user": user id} (profile or status changed) */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_user_changed(const char* json);
/* {"users": [user id]}, read with get_presence */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_presence_changed(const char* json);
/* {"conv": <conv>} the user switched to this conversation */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_conversation_opened(const char* json);
/* {"state": "connected" | "reconnecting" | "auth_failed"}; events may have been missed after recovery, re-read if needed */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_connection_changed(const char* json);
/* {"conv": <conv>, "user": user id, "typing": true | false} (typing event from the server, excluding yourself and muted users);
   false is not always received when the other side leaves abnormally, so time out on your own if needed */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_typing_changed(const char* json);
/* {} scheduled messages changed, read with get_scheduled_messages */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_scheduled_changed(const char* json);
/* {} reminders changed, read with get_reminders */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_reminders_changed(const char* json);
/* {"conv": <conv>} draft content changed: triggered by local saves and draft deletion when sync is off, and by server-synced changes when sync is on. Read with get_drafts */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_draft_changed(const char* json);
/* {"conv": <conv>, "visibility": ZULIP_PLUGIN_TOPIC_*} follow state of a topic changed */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_user_topic_changed(const char* json);
/* {"users": [ids of all currently muted users]} */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_muted_users_changed(const char* json);
/* {"properties": ["names of the changed organization settings"]}, read with get_realm_info */
ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_realm_changed(const char* json);
#ifdef __cplusplus
}
#endif
