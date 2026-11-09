/*
 * 最小的 C 插件：私聊收到 "ping" 时回复 "pong"；消息被编辑时写日志；工具栏按钮把输入框内容替换为固定文字。
 * 编译：clang -shared -o echo.dll echo.c，然后把 echo.dll 和 plugin.json 放到 <程序目录>\plugins\<id>\。
 */
#include <stddef.h>
#include <string.h>
#include "../zulip_plugin.h"

_Static_assert(sizeof(zulip_plugin_conversation) == 48, "conversation");
_Static_assert(offsetof(zulip_plugin_conversation, dm_user_count) == 40, "conversation.dm_user_count");
_Static_assert(sizeof(zulip_plugin_message) == 104, "message");
_Static_assert(offsetof(zulip_plugin_message, conv) == 72, "message.conv");
_Static_assert(offsetof(zulip_plugin_message, flags) == 4, "message.flags");
_Static_assert(offsetof(zulip_plugin_message, last_edit_timestamp) == 96, "message.last_edit_timestamp");
_Static_assert(sizeof(zulip_plugin_outgoing) == 32, "outgoing");
_Static_assert(sizeof(zulip_plugin_user) == 48, "user");
_Static_assert(sizeof(zulip_plugin_channel) == 40, "channel");
_Static_assert(sizeof(zulip_plugin_topic) == 32, "topic");
_Static_assert(sizeof(zulip_plugin_account) == 48, "account");
_Static_assert(sizeof(zulip_plugin_host_api) == 672, "host_api");
_Static_assert(offsetof(zulip_plugin_host_api, reply_panel) == 248, "host_api.reply_panel");
_Static_assert(offsetof(zulip_plugin_host_api, api_request) == 416, "host_api.api_request");
_Static_assert(offsetof(zulip_plugin_host_api, set_thread_account) == 424, "host_api.set_thread_account");
_Static_assert(offsetof(zulip_plugin_host_api, upload_file_ex) == 664, "host_api.upload_file_ex");

static const zulip_plugin_host_api* g_api;

ZULIP_PLUGIN_EXPORT int32_t ZULIP_PLUGIN_CALL zulip_plugin_init(const zulip_plugin_host_api* api)
{
    /* 只用到 v1 基础表（到 api_request）；后来追加的函数调用前用 offsetof 判断 */
    if (api == NULL || api->api_version < ZULIP_PLUGIN_API_VERSION
        || api->struct_size < offsetof(zulip_plugin_host_api, api_request) + sizeof(api->api_request))
        return ZULIP_PLUGIN_ERR_INVALID_ARG;
    g_api = api;
    return ZULIP_PLUGIN_OK;
}

ZULIP_PLUGIN_EXPORT int32_t ZULIP_PLUGIN_CALL zulip_plugin_on_message(const zulip_plugin_message* msg)
{
    if ((msg->flags & ZULIP_PLUGIN_MSG_OUTGOING) || msg->conv->type != ZULIP_PLUGIN_CONV_DIRECT || msg->conv->dm_user_count != 1)
        return ZULIP_PLUGIN_EVENT_IGNORE;
    if (strcmp(msg->content_text, "ping") == 0
        && g_api->send_direct_message(g_api->ctx, msg->conv->dm_user_ids, 1, "pong") <= 0)
        g_api->add_log(g_api->ctx, ZULIP_PLUGIN_LOG_WARNING, "echo", "回复失败");
    return ZULIP_PLUGIN_EVENT_IGNORE;
}

ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_message_edited(const char* json)
{
    g_api->add_log(g_api->ctx, ZULIP_PLUGIN_LOG_INFO, "echo", json);
}

ZULIP_PLUGIN_EXPORT void ZULIP_PLUGIN_CALL zulip_plugin_on_composer_button(int32_t button_id, const char* json)
{
    (void)button_id;
    (void)json;
    if (g_api->fill_composer(g_api->ctx, "回声", ZULIP_PLUGIN_FILL_REPLACE) < 0)
        g_api->add_log(g_api->ctx, ZULIP_PLUGIN_LOG_WARNING, "echo", "填入失败");
}
