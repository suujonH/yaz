//! 真实加载 plugin-echo 的 DLL，走开启确认、权限和钩子流程。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use tempfile::TempDir;
use test_server::{AJIE, DEV, ME_EMAIL, ME_ID, Target, TestServer, XIAOYU};
use tokio::runtime::Handle;
use tokio::sync::broadcast::error::TryRecvError;
use zulip_core::ConversationKey;
use zulip_plugin_host::annotation::ActionStyle;
use zulip_plugin_host::{
    ComposerIcon, ContextMenuKind, ContextMenuTarget, DialogKind, DialogResult, MenuContext,
    MenuConv, NativePlugin, OutgoingRejected, PanelError, PanelFieldValue, PanelRequest,
    PluginEvent, PluginHost, PluginHostConfig, PluginTarget, PluginTargetKind, SettingsButtonClick,
    ToggleOutcome, parse_fields,
};
use zulip_session::{Hub, OutgoingKind, PasswordLogin, Session, SessionError};
use zulip_store::{AppPaths, Db, SettingRepo};

const ID: &str = "com.yaz.echo";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

// 单独的目录，不与外层 cargo test 争用构建锁。
fn built_echo() -> &'static PathBuf {
    static DLL: OnceLock<PathBuf> = OnceLock::new();
    DLL.get_or_init(|| {
        let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("plugin-build");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
        let status = Command::new(cargo)
            .args(["build", "-p", "plugin-echo"])
            .current_dir(workspace_root())
            .env("CARGO_TARGET_DIR", &target)
            .status()
            .unwrap();
        assert!(status.success());
        target.join("debug").join("plugin_echo.dll")
    })
}

fn temp_dir() -> TempDir {
    // DLL 不会卸载，删不掉的目录留在 target 下。
    tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap()
}

struct Fixture {
    dir: TempDir,
    host: Arc<PluginHost>,
    hub: Arc<Hub>,
    db: Db,
    paths: AppPaths,
}

fn manifest() -> Value {
    let path = workspace_root().join("plugins/echo/plugin.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn plugin_dir(dir: &TempDir) -> PathBuf {
    dir.path().join("plugins").join(ID)
}

fn install(dir: &TempDir, manifest: &Value) {
    let target = plugin_dir(dir);
    std::fs::create_dir_all(&target).unwrap();
    std::fs::copy(built_echo(), target.join("plugin_echo.dll")).unwrap();
    std::fs::write(target.join("plugin.json"), manifest.to_string()).unwrap();
}

async fn start_host(dir: &TempDir, db: &Db, paths: &AppPaths) -> (Arc<PluginHost>, Arc<Hub>) {
    start_host_with(dir, db, paths, zulip_plugin_host::DIALOG_TIMEOUT).await
}

async fn start_host_with(
    dir: &TempDir,
    db: &Db,
    paths: &AppPaths,
    dialog_timeout: Duration,
) -> (Arc<PluginHost>, Arc<Hub>) {
    let host = PluginHost::new(PluginHostConfig {
        plugins_dir: dir.path().join("plugins"),
        db: db.clone(),
        paths: paths.clone(),
        runtime: Handle::current(),
        dialog_timeout,
    });
    let hub = Hub::new(db.clone(), paths.clone(), None);
    host.attach(&hub);
    host.load_all().await.unwrap();
    (host, hub)
}

impl Fixture {
    async fn new(manifest: &Value) -> Self {
        Self::with_dialog_timeout(manifest, zulip_plugin_host::DIALOG_TIMEOUT).await
    }

    async fn with_dialog_timeout(manifest: &Value, dialog_timeout: Duration) -> Self {
        let dir = temp_dir();
        install(&dir, manifest);
        let paths = AppPaths::new(dir.path().join("data"));
        std::fs::create_dir_all(paths.root()).unwrap();
        let db = Db::open(&paths.database()).unwrap();
        let (host, hub) = start_host_with(&dir, &db, &paths, dialog_timeout).await;
        Self {
            dir,
            host,
            hub,
            db,
            paths,
        }
    }

    fn plugin(&self) -> Arc<NativePlugin> {
        self.host.plugin(ID).unwrap()
    }

    async fn enable(&self) -> Arc<NativePlugin> {
        let plugin = self.plugin();
        let outcome = self.host.toggle(&plugin, true).await;
        assert!(matches!(outcome, ToggleOutcome::NeedsApproval(_)));
        assert!(self.host.approve(&plugin).await);
        assert!(plugin.is_loaded(), "{:?}", plugin.load_error());
        plugin
    }

    async fn login(&self, server: &TestServer) -> Arc<Session> {
        self.hub
            .login_with_password(PasswordLogin {
                realm: server.url(),
                email: ME_EMAIL.to_owned(),
                password: "任意密码".to_owned(),
                allow_invalid_cert: false,
            })
            .await
            .unwrap()
    }
}

async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "等待超时：{what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn has_log(plugin: &NativePlugin, text: &str) -> bool {
    plugin.logs().iter().any(|l| l.text.contains(text))
}

async fn wait_log(plugin: &NativePlugin, text: &str) {
    wait_until(text, || has_log(plugin, text)).await;
}

fn dm_key(session: &Session) -> ConversationKey {
    ConversationKey::dm([XIAOYU], session.self_id())
}

#[tokio::test(flavor = "multi_thread")]
async fn approval_flow_and_fingerprint() {
    let fixture = Fixture::new(&manifest()).await;
    let plugin = fixture.plugin();
    assert!(!plugin.is_enabled());
    assert!(!plugin.is_loaded());

    let ToggleOutcome::NeedsApproval(info) = fixture.host.toggle(&plugin, true).await else {
        panic!("新插件应要求确认");
    };
    assert_eq!(info.title, "开启插件“回声示例”");
    assert_eq!(info.meta, format!("1.0.0 · YaZ · {ID}"));
    assert_eq!(info.notice, None);
    assert_eq!(info.reads, vec!["读取消息", "读取联系人和频道"]);
    assert!(info.acts.iter().any(|a| a == "发送消息"));
    assert!(info.events.iter().any(|e| e == "接收新消息"));
    assert!(info.events.iter().any(|e| e == "消息右键菜单“附加回声”"));
    for expected in [
        "消息被编辑",
        "话题改名或移动",
        "在线状态变化",
        "会话右键菜单“回声：会话”",
        "用户右键菜单“回声”",
        "频道右键菜单“回声：频道”",
    ] {
        assert!(info.events.iter().any(|e| e == expected), "{expected}");
    }
    assert!(
        info.events
            .iter()
            .any(|e| e.starts_with("输入框工具栏按钮“回声：替换输入框”"))
    );
    assert!(!info.acts.iter().any(|a| a.contains("高危")));
    assert!(!plugin.is_enabled());

    assert!(fixture.host.approve(&plugin).await);
    assert!(plugin.is_enabled());
    assert!(plugin.is_loaded());
    assert_eq!(plugin.status_text(), "");
    assert_eq!(plugin.active_menus().len(), 1);

    // 重启后保持开启。
    let (again, _hub) = start_host(&fixture.dir, &fixture.db, &fixture.paths).await;
    let reloaded = again.plugin(ID).unwrap();
    assert!(reloaded.is_enabled() && reloaded.is_loaded());

    // 文件变化后需要重新确认。
    let mut changed = manifest();
    changed["description"] = json!("改过");
    std::fs::write(
        plugin_dir(&fixture.dir).join("plugin.json"),
        changed.to_string(),
    )
    .unwrap();
    let (host, _hub) = start_host(&fixture.dir, &fixture.db, &fixture.paths).await;
    let stale = host.plugin(ID).unwrap();
    assert!(stale.needs_reapproval());
    assert!(!stale.is_enabled() && !stale.is_loaded());
    assert_eq!(
        stale.status_text(),
        "插件文件已变化（更新或被修改），需要重新开启"
    );
    assert!(has_log(&stale, "需要重新开启"));
    let ToggleOutcome::NeedsApproval(info) = host.toggle(&stale, true).await else {
        panic!("应要求重新确认");
    };
    assert!(info.notice.is_some());

    // 确认框打开期间文件变化：不开启。
    std::fs::write(plugin_dir(&fixture.dir).join("extra.txt"), "x").unwrap();
    assert!(!host.approve(&stale).await);
    assert!(!stale.is_enabled());
}

#[tokio::test(flavor = "multi_thread")]
async fn dependencies_and_load_errors() {
    let mut value = manifest();
    value["dependencies"] = json!(["com.example.missing"]);
    let fixture = Fixture::new(&value).await;
    let plugin = fixture.plugin();
    let outcome = fixture.host.toggle(&plugin, true).await;
    assert_eq!(
        outcome.message().unwrap(),
        "请先开启依赖的插件：com.example.missing（未安装）"
    );

    let mut value = manifest();
    value["library"] = json!("missing.dll");
    let fixture = Fixture::new(&value).await;
    let plugin = fixture.plugin();
    let _ = fixture.host.toggle(&plugin, true).await;
    assert!(fixture.host.approve(&plugin).await);
    assert!(!plugin.is_loaded());
    assert!(
        plugin
            .status_text()
            .starts_with("加载失败：无法加载 missing.dll")
    );
    assert!(plugin.is_error());

    let dir = temp_dir();
    std::fs::create_dir_all(dir.path().join("plugins/com.bad.plugin")).unwrap();
    let paths = AppPaths::new(dir.path().join("data"));
    std::fs::create_dir_all(paths.root()).unwrap();
    let db = Db::open(&paths.database()).unwrap();
    let (host, _hub) = start_host(&dir, &db, &paths).await;
    let bad = host.plugin("com.bad.plugin").unwrap();
    assert_eq!(bad.status_text(), "加载失败：缺少 plugin.json");
}

#[tokio::test(flavor = "multi_thread")]
async fn hooks_and_lifecycle() {
    let server = TestServer::start(0).await.unwrap();
    let fixture = Fixture::new(&manifest()).await;
    let plugin = fixture.enable().await;
    assert!(!has_log(&plugin, "已开启"));

    let session = fixture.login(&server).await;
    wait_log(&plugin, "已开启").await;
    wait_log(&plugin, &format!("账号 {} 已登录", session.account_id())).await;

    // on_message：回复 pong。
    let ping = server.send_as(XIAOYU, test_server::Target::Dm(vec![ME_ID]), "ping");
    wait_until("pong", || {
        let latest = server.latest_message_id();
        latest > ping && server.content_of(latest).as_deref() == Some("pong")
    })
    .await;

    // filter_incoming：拦截后标记已读。
    let blocked = server.send_as(XIAOYU, test_server::Target::Dm(vec![ME_ID]), "echo:block x");
    wait_until("已读", || {
        server.flags_of(blocked).iter().any(|f| f == "read")
    })
    .await;

    // filter_outgoing：拦截、改写。
    let key = dm_key(&session);
    let host = &fixture.host;
    assert_eq!(
        host.filter_outgoing(&session, &key, "echo:block").await,
        Err(OutgoingRejected::Blocked)
    );
    assert_eq!(
        host.filter_outgoing(&session, &key, "echo:rewrite 你好")
            .await,
        Ok("你好".to_owned())
    );
    assert_eq!(
        host.filter_outgoing(&session, &key, "echo:rewrite ").await,
        Err(OutgoingRejected::Emptied)
    );
    assert_eq!(
        host.filter_outgoing(&session, &key, "普通").await,
        Ok("普通".to_owned())
    );
    let long = format!("echo:rewrite {}", "长".repeat(100_000));
    assert_eq!(
        host.filter_outgoing(&session, &key, &long).await,
        Err(OutgoingRejected::TooLong)
    );

    // 停用后不再派发。
    assert!(matches!(
        host.toggle(&plugin, false).await,
        ToggleOutcome::Done
    ));
    wait_log(&plugin, "已停用").await;
    assert_eq!(
        host.filter_outgoing(&session, &key, "echo:block").await,
        Ok("echo:block".to_owned())
    );

    // 已确认的插件再次开启不需要确认。
    assert!(matches!(
        host.toggle(&plugin, true).await,
        ToggleOutcome::Done
    ));
    assert_eq!(
        host.filter_outgoing(&session, &key, "echo:block").await,
        Err(OutgoingRejected::Blocked)
    );

    // 账号结束：通知插件。
    plugin.clear_logs();
    fixture.hub.logout(session.account_id(), false).await;
    wait_log(&plugin, &format!("账号 {} 已退出", session.account_id())).await;
    wait_log(&plugin, "已停用").await;
    assert!(host.sessions().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_permission_is_denied() {
    let server = TestServer::start(0).await.unwrap();
    let mut value = manifest();
    value["permissions"] = json!(["read_message"]);
    let fixture = Fixture::new(&value).await;
    let plugin = fixture.enable().await;
    let session = fixture.login(&server).await;

    let ping = server.send_as(XIAOYU, test_server::Target::Dm(vec![ME_ID]), "ping");
    wait_log(
        &plugin,
        "send_direct_message：plugin.json 未声明权限 send_message",
    )
    .await;
    wait_log(&plugin, "回复失败：没有权限").await;
    assert_eq!(server.latest_message_id(), ping);

    // 没有 modify_message 时不接收 filter_outgoing。
    assert_eq!(
        fixture
            .host
            .filter_outgoing(&session, &dm_key(&session), "echo:block")
            .await,
        Ok("echo:block".to_owned())
    );

    // 没有 annotate_message 时附加失败。
    let message = message_of(&session, ping).await;
    fixture
        .host
        .invoke_message_menu(&plugin, 1, &message, &session);
    wait_log(
        &plugin,
        "set_annotation：plugin.json 未声明权限 annotate_message",
    )
    .await;
}

async fn message_of(session: &Session, id: i64) -> zulip_core::model::ChatMessage {
    let dto = session.client().get_message(id, true).await.unwrap();
    zulip_session::convert::message_from_dto(&dto, session.self_id())
}

#[tokio::test(flavor = "multi_thread")]
async fn monitor_priority_cannot_block_or_rewrite() {
    let server = TestServer::start(0).await.unwrap();
    let mut value = manifest();
    value["events"] = json!({"filter_outgoing": 10000, "filter_incoming": 10000});
    let fixture = Fixture::new(&value).await;
    fixture.enable().await;
    let session = fixture.login(&server).await;
    let key = dm_key(&session);

    assert_eq!(
        fixture
            .host
            .filter_outgoing(&session, &key, "echo:block")
            .await,
        Ok("echo:block".to_owned())
    );
    assert_eq!(
        fixture
            .host
            .filter_outgoing(&session, &key, "echo:rewrite x")
            .await,
        Ok("echo:rewrite x".to_owned())
    );
    let id = server.send_as(XIAOYU, test_server::Target::Dm(vec![ME_ID]), "echo:block");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!server.flags_of(id).iter().any(|f| f == "read"));
}

#[tokio::test(flavor = "multi_thread")]
async fn menus_annotations_panel_and_settings() {
    let server = TestServer::start(0).await.unwrap();
    let fixture = Fixture::new(&manifest()).await;
    let plugin = fixture.enable().await;
    let session = fixture.login(&server).await;
    let host = &fixture.host;
    let mut events = host.subscribe();

    host.invoke_menu(&plugin, 1);
    wait_log(&plugin, "点击了菜单 1").await;

    // 消息菜单 → 附加内容。
    let id = server.send_as(XIAOYU, test_server::Target::Dm(vec![ME_ID]), "hello");
    let message = message_of(&session, id).await;
    assert_eq!(plugin.active_message_menus().len(), 2);
    host.invoke_message_menu(&plugin, 1, &message, &session);
    let account = session.account_id();
    wait_until("附加内容", || {
        !host.message_annotations(account, id).cards.is_empty()
    })
    .await;
    let annotations = host.message_annotations(account, id);
    assert_eq!(annotations.cards[0].text, "**回声**：hello");
    assert_eq!(
        annotations.cards[0].text_html,
        "<p><strong>回声</strong>：hello</p>"
    );
    assert_eq!(annotations.cards[0].plugin_name, "回声示例");
    assert_eq!(annotations.cards[0].actions[0].style, ActionStyle::Primary);
    assert!(annotations.rendered.is_none());

    let mut changed = false;
    loop {
        match events.try_recv() {
            Ok(PluginEvent::AnnotationChanged { message_id, .. }) if message_id == id => {
                changed = true;
            }
            Ok(_) => {}
            Err(TryRecvError::Empty) if changed => break,
            Err(TryRecvError::Empty) => tokio::time::sleep(Duration::from_millis(10)).await,
            Err(error) => panic!("{error}"),
        }
    }

    host.set_annotation_collapsed(account, id, ID, true);
    assert!(host.message_annotations(account, id).cards[0].is_collapsed);

    // 停用的插件不显示附加内容。
    host.toggle(&plugin, false).await;
    assert!(host.message_annotations(account, id).cards.is_empty());
    host.toggle(&plugin, true).await;

    // 面板。
    let target = PluginTarget::User {
        account_id: account,
        user_id: XIAOYU,
        name: "小雨".to_owned(),
    };
    assert_eq!(plugin.active_panels()[0].kind, PluginTargetKind::User);
    let reply = host
        .panel(&plugin, PanelRequest::Get(target.clone()))
        .await
        .unwrap()
        .unwrap();
    let fields = parse_fields(&reply);
    let PanelFieldValue::Select { selected, options } = &fields[0].value else {
        panic!("应为下拉框");
    };
    assert_eq!(selected, "");
    assert_eq!(options.len(), 4);
    let reply = host
        .panel(
            &plugin,
            PanelRequest::Set {
                target,
                key: "nickname".to_owned(),
                value: json!("friend"),
            },
        )
        .await
        .unwrap()
        .unwrap();
    let PanelFieldValue::Select { selected, .. } = &parse_fields(&reply)[0].value else {
        panic!("应为下拉框");
    };
    assert_eq!(selected, "friend");

    // 设置：默认值、保存后插件读到新值。
    let mut values = host.load_settings(&plugin);
    assert_eq!(values["reply"], "pong");
    values.insert("reply".to_owned(), json!("收到"));
    host.save_settings(&plugin, &values).unwrap();
    let ping = server.send_as(XIAOYU, test_server::Target::Dm(vec![ME_ID]), "ping");
    wait_until("新回复", || {
        let latest = server.latest_message_id();
        latest > ping && server.content_of(latest).as_deref() == Some("收到")
    })
    .await;

    // 设置中的按钮 → 回复设置窗口。
    let mut events = host.subscribe();
    let click = SettingsButtonClick {
        key: "test".to_owned(),
        values: Map::from_iter([("reply".to_owned(), json!("未保存"))]),
        ..SettingsButtonClick::default()
    };
    assert!(host.invoke_settings_button(&plugin, &click));
    let start = Instant::now();
    loop {
        match events.try_recv() {
            Ok(PluginEvent::SettingsFeedback {
                plugin_id,
                feedback,
            }) => {
                assert_eq!(plugin_id, ID);
                assert_eq!(feedback.key, "test");
                assert_eq!(feedback.text.as_deref(), Some("回复内容：未保存"));
                break;
            }
            Ok(_) => {}
            Err(_) => {
                assert!(start.elapsed() < Duration::from_secs(5), "没有收到回复");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shortcuts_bind_and_invoke() {
    let server = TestServer::start(0).await.unwrap();
    let fixture = Fixture::new(&manifest()).await;
    let plugin = fixture.enable().await;
    let session = fixture.login(&server).await;
    let host = &fixture.host;

    let keys = host.key_shortcuts();
    assert_eq!(keys[0].title, "回声示例 · 写一条日志");
    assert_eq!(keys[0].gesture, "");
    host.set_shortcut(&keys[0], "Ctrl+Shift+E");
    assert!(host.invoke_key_shortcut("Ctrl+Shift+E"));
    assert!(!host.invoke_key_shortcut("Ctrl+Shift+Q"));
    wait_log(&plugin, "点击了菜单 1").await;

    let mouse = host.message_shortcuts();
    let toasts = host.set_shortcut(&mouse[0], "left_click");
    assert!(toasts[0].contains("已用于选中消息"));
    assert!(!host.has_mouse_shortcut("left_click"));
    assert!(host.set_shortcut(&mouse[0], "ctrl+middle_click").is_empty());
    assert!(host.has_mouse_shortcut("ctrl+middle_click"));

    let id = server.send_as(XIAOYU, test_server::Target::Dm(vec![ME_ID]), "hello");
    let message = message_of(&session, id).await;
    assert!(host.invoke_mouse_shortcut(
        "ctrl+middle_click",
        &message,
        &session,
        &MenuContext::default()
    ));
    wait_until("附加内容", || {
        !host
            .message_annotations(session.account_id(), id)
            .cards
            .is_empty()
    })
    .await;

    // 绑定保存在设置中。
    let (again, _hub) = start_host(&fixture.dir, &fixture.db, &fixture.paths).await;
    let reloaded = again.plugin(ID).unwrap();
    assert!(reloaded.is_loaded());
    assert_eq!(again.key_shortcuts()[0].gesture, "Ctrl+Shift+E");
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_plugin_times_out_and_becomes_unresponsive() {
    let server = TestServer::start(0).await.unwrap();
    let fixture = Fixture::new(&manifest()).await;
    let plugin = fixture.enable().await;
    let session = fixture.login(&server).await;
    let key = dm_key(&session);
    let host = &fixture.host;

    for round in 1..=3 {
        assert_eq!(
            host.filter_outgoing(&session, &key, "echo:slow").await,
            Ok("echo:slow".to_owned()),
            "第 {round} 次"
        );
        // 卡住期间新的事件直接放行。
        if round == 1 {
            assert_eq!(
                host.filter_outgoing(&session, &key, "echo:block").await,
                Ok("echo:block".to_owned())
            );
        }
        // 等插件线程上的调用结束。
        tokio::time::sleep(Duration::from_millis(600)).await;
    }
    assert!(plugin.is_unresponsive());
    assert_eq!(plugin.status_text(), "无响应");
    assert!(plugin.is_error());
    assert!(has_log(&plugin, "filter_outgoing 超过 2 秒没有返回"));
    assert!(has_log(&plugin, "本次运行内不再派发事件"));
    assert!(plugin.active_menus().is_empty());
    assert_eq!(
        host.filter_outgoing(&session, &key, "echo:block").await,
        Ok("echo:block".to_owned())
    );
}

// 没有 C 编译器时跳过。
#[tokio::test(flavor = "multi_thread")]
async fn c_example_loads_and_replies() {
    let source = workspace_root().join("sdk/examples/echo.c");
    let dir = temp_dir();
    let target = plugin_dir(&dir);
    std::fs::create_dir_all(&target).unwrap();
    let dll = target.join("echo_c.dll");
    let compiled = ["clang", "gcc"].iter().any(|compiler| {
        Command::new(compiler)
            .arg("-shared")
            .arg("-o")
            .arg(&dll)
            .arg(&source)
            .status()
            .is_ok_and(|s| s.success())
    });
    if !compiled {
        eprintln!("没有找到 C 编译器，跳过");
        return;
    }
    let mut value = manifest();
    value["library"] = json!("echo_c.dll");
    value["events"] = json!({"on_message": 30000, "on_message_edited": 30000});
    std::fs::write(target.join("plugin.json"), value.to_string()).unwrap();

    let paths = AppPaths::new(dir.path().join("data"));
    std::fs::create_dir_all(paths.root()).unwrap();
    let db = Db::open(&paths.database()).unwrap();
    let (host, hub) = start_host(&dir, &db, &paths).await;
    let fixture = Fixture {
        dir,
        host,
        hub,
        db,
        paths,
    };
    let plugin = fixture.enable().await;
    let server = TestServer::start(0).await.unwrap();
    let session = fixture.login(&server).await;

    let ping = server.send_as(XIAOYU, test_server::Target::Dm(vec![ME_ID]), "ping");
    wait_until("pong", || {
        let latest = server.latest_message_id();
        latest > ping && server.content_of(latest).as_deref() == Some("pong")
    })
    .await;

    // 通知型事件和工具栏按钮（确认 C 结构的布局和导出函数）。
    let own = session
        .send_message(&dm_key(&session), "原文")
        .await
        .unwrap();
    session.edit_message(own, "改过").await.unwrap();
    wait_log(&plugin, "\"message_id\":").await;

    let mut events = fixture.host.subscribe();
    let key = dm_key(&session);
    fixture
        .host
        .invoke_composer_button(&plugin, 1, &session, &key, "");
    let (text, replace) = next_fill(&mut events).await;
    assert_eq!(text, "回声");
    assert!(replace);
}

fn with_all_permissions() -> Value {
    let mut value = manifest();
    value["permissions"] = json!([
        "read_message",
        "read_contacts",
        "read_attachment",
        "send_message",
        "modify_message",
        "annotate_message",
        "fill_composer",
        "notify",
        "manage_topic",
        "open_conversation",
        "zulip_api",
        "upload_file",
        "manage_subscription",
        "manage_preferences",
        "ui_dialog",
    ]);
    value
}

async fn next_fill(events: &mut tokio::sync::broadcast::Receiver<PluginEvent>) -> (String, bool) {
    let start = Instant::now();
    loop {
        match events.try_recv() {
            Ok(PluginEvent::FillComposer { text, replace, .. }) => return (text, replace),
            Ok(_) => {}
            Err(_) => {
                assert!(start.elapsed() < Duration::from_secs(5), "没有收到填入事件");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

// 通过工具栏按钮让示例插件调用一个宿主函数，等结果写入日志。
async fn plugin_call(
    fixture: &Fixture,
    plugin: &Arc<NativePlugin>,
    session: &Arc<Session>,
    op: &str,
    args: Value,
) -> String {
    plugin.clear_logs();
    let key = dm_key(session);
    let text = format!("echo:call {op} {args}");
    fixture
        .host
        .invoke_composer_button(plugin, 1, session, &key, &text);
    let done = format!("调用 {op}");
    wait_log(plugin, &done).await;
    plugin
        .logs()
        .iter()
        .find(|l| l.text.starts_with(&done))
        .map(|l| l.text.clone())
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn notifications_reach_the_plugin() {
    let server = TestServer::start(0).await.unwrap();
    let fixture = Fixture::new(&with_all_permissions()).await;
    let plugin = fixture.enable().await;
    let session = fixture.login(&server).await;
    let key = dm_key(&session);

    // 切换到会话。
    session.open_conversation(&key);
    wait_log(&plugin, "切换到 私聊").await;
    plugin.clear_logs();
    session.set_actively_viewing(&key, true);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!has_log(&plugin, "切换到"), "同一会话不重复通知");

    wait_until("表情目录", || {
        session.emoji_catalog().get("+1").is_some()
    })
    .await;

    // 编辑、回应、已读、删除。
    let id = session.send_message(&key, "原文").await.unwrap();
    session.edit_message(id, "改过").await.unwrap();
    wait_log(&plugin, &format!("编辑 {id}：改过")).await;
    plugin_call(
        &fixture,
        &plugin,
        &session,
        "react",
        json!({"id": id, "emoji": "+1"}),
    )
    .await;
    wait_log(&plugin, &format!("回应 {id} +1 true")).await;
    plugin_call(
        &fixture,
        &plugin,
        &session,
        "unreact",
        json!({"id": id, "emoji": "+1"}),
    )
    .await;
    wait_log(&plugin, &format!("回应 {id} +1 false")).await;
    let other = server.send_as(XIAOYU, Target::Dm(vec![ME_ID]), "x");
    session.mark_read(&[other]).await.unwrap();
    wait_log(&plugin, &format!("已读 [{other}] true")).await;
    plugin_call(
        &fixture,
        &plugin,
        &session,
        "mark_unread",
        json!({"id": other}),
    )
    .await;
    wait_log(&plugin, "false").await;
    session
        .delete(zulip_session::DeleteTarget::Message(id))
        .await
        .unwrap();
    wait_log(&plugin, &format!("删除 [{id}]")).await;

    // 话题改名（含旧键和新键）。
    let topic = "发布计划";
    plugin_call(
        &fixture,
        &plugin,
        &session,
        "move_topic",
        json!({"stream": DEV, "topic": topic, "new_topic": "新名字"}),
    )
    .await;
    wait_log(&plugin, &format!("移动 {DEV}/{topic} -> {DEV}/新名字")).await;

    // 用户、在线状态、频道。
    server.push_event(json!({"type": "user_status", "user_id": AJIE, "status_text": "忙"}));
    wait_log(&plugin, &format!("用户 {AJIE}")).await;
    server.push_event(json!({
        "type": "presence", "presences": {
            AJIE.to_string(): {"active_timestamp": chrono::Utc::now().timestamp(), "idle_timestamp": 0}
        },
    }));
    wait_log(&plugin, &format!("在线 [{AJIE}]")).await;
    server.push_event(json!({
        "type": "stream", "op": "update", "stream_id": DEV, "property": "description",
        "value": "新介绍", "rendered_description": "<p>新介绍</p>",
    }));
    wait_log(&plugin, "频道 ").await;
    let presence = plugin_call(&fixture, &plugin, &session, "presence", json!({"id": AJIE})).await;
    assert!(presence.contains("\"presence\":\"online\""), "{presence}");
    assert!(presence.contains("\"status_text\":\"忙\""), "{presence}");

    // 连接断开与恢复。
    let mut server = server;
    server.stop().await;
    wait_log(&plugin, "连接 reconnecting").await;
    server.restart().await.unwrap();
    wait_log(&plugin, "连接 connected").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reads_through_the_plugin() {
    let server = TestServer::start(0).await.unwrap();
    let fixture = Fixture::new(&with_all_permissions()).await;
    let plugin = fixture.enable().await;
    let session = fixture.login(&server).await;
    let key = dm_key(&session);

    wait_until("表情目录", || {
        session.emoji_catalog().get("+1").is_some()
    })
    .await;

    // 消息结构：回应、编辑时间、上传文件。
    let file = plugin_dir(&fixture.dir).join("plugin.json");
    let uploaded = session.upload_file(&file, |_| {}).await.unwrap();
    let id = session
        .send_message(&key, &format!("[附件.json]({})", uploaded.url))
        .await
        .unwrap();
    session
        .edit_message(id, &format!("[附件.json]({}) 改过", uploaded.url))
        .await
        .unwrap();
    plugin_call(
        &fixture,
        &plugin,
        &session,
        "react",
        json!({"id": id, "emoji": "+1"}),
    )
    .await;
    wait_until("回应进入服务器", || has_log(&plugin, "回应")).await;
    let read = plugin_call(&fixture, &plugin, &session, "message", json!({"id": id})).await;
    assert!(read.contains("回应 1 "), "{read}");
    assert!(read.contains("附件.json"), "{read}");
    assert!(read.ends_with("编辑 true"), "{read}");

    // 下载附件。
    let local = plugin_call(
        &fixture,
        &plugin,
        &session,
        "download",
        json!({"path": uploaded.url}),
    )
    .await;
    let local = local.trim_start_matches("调用 download：");
    assert!(Path::new(local).is_file(), "{local}");
    let refused = plugin_call(
        &fixture,
        &plugin,
        &session,
        "download",
        json!({"path": "/api/v1/users"}),
    )
    .await;
    assert!(refused.contains("失败"), "{refused}");

    // 搜索和分页。
    server.send_as(XIAOYU, Target::Dm(vec![ME_ID]), "关键词一");
    server.send_as(XIAOYU, Target::Dm(vec![ME_ID]), "关键词二");
    let page = plugin_call(
        &fixture,
        &plugin,
        &session,
        "search",
        json!({"text": "关键词", "page_size": 1}),
    )
    .await;
    let cursor: i64 = page
        .split("Some(")
        .nth(1)
        .and_then(|rest| rest.trim_end_matches(')').parse().ok())
        .unwrap_or_else(|| panic!("{page}"));
    let next = plugin_call(
        &fixture,
        &plugin,
        &session,
        "search",
        json!({"text": "关键词", "page_size": 1, "cursor": cursor}),
    )
    .await;
    assert!(next.contains("1 条"), "{next}");
    let scoped = plugin_call(
        &fixture,
        &plugin,
        &session,
        "search",
        json!({"text": "关键词", "conv": {"type": "channel", "stream": DEV, "topic": "发布计划"}}),
    )
    .await;
    assert!(scoped.contains("0 条"), "{scoped}");

    // 其余读取。
    let list = plugin_call(&fixture, &plugin, &session, "conversations", json!({})).await;
    assert!(list.contains("\"total_unread\""), "{list}");
    assert!(list.contains("发布计划"), "{list}");
    let none = plugin_call(&fixture, &plugin, &session, "current", json!({})).await;
    assert!(none.ends_with("无"), "{none}");
    session.open_conversation(&key);
    let current = plugin_call(&fixture, &plugin, &session, "current", json!({})).await;
    assert!(current.contains(&format!("[{XIAOYU}]")), "{current}");
    session.close_conversation(&key);
    let none = plugin_call(&fixture, &plugin, &session, "current", json!({})).await;
    assert!(none.ends_with("无"), "{none}");
    let groups = plugin_call(&fixture, &plugin, &session, "groups", json!({})).await;
    assert!(groups.contains("role:everyone"), "{groups}");
    let all = plugin_call(&fixture, &plugin, &session, "all_channels", json!({})).await;
    assert_eq!(
        all,
        format!(
            "调用 all_channels：{} 个，未订阅 {}",
            session.channels().len(),
            session
                .channels()
                .iter()
                .filter(|c| !c.is_subscribed())
                .count()
        )
    );
    let visibility = plugin_call(
        &fixture,
        &plugin,
        &session,
        "visibility",
        json!({"stream": DEV, "topic": "发布计划"}),
    )
    .await;
    assert_eq!(visibility, "调用 visibility：0");
    let history = plugin_call(&fixture, &plugin, &session, "history", json!({"id": id})).await;
    assert!(history.contains("改过"), "{history}");
}

#[tokio::test(flavor = "multi_thread")]
async fn actions_through_the_plugin() {
    let server = TestServer::start(0).await.unwrap();
    let fixture = Fixture::new(&with_all_permissions()).await;
    let plugin = fixture.enable().await;
    let session = fixture.login(&server).await;
    let key = dm_key(&session);
    let topic = "发布计划";

    // 标记未读。
    let id = server.send_as(XIAOYU, Target::Dm(vec![ME_ID]), "未读");
    session.mark_read(&[id]).await.unwrap();
    let result = plugin_call(
        &fixture,
        &plugin,
        &session,
        "mark_unread",
        json!({"id": id}),
    )
    .await;
    assert_eq!(result, "调用 mark_unread：ok");
    assert!(!server.flags_of(id).iter().any(|f| f == "read"));

    // 定时消息。
    let at = chrono::Utc::now().timestamp() + 3600;
    let conv = json!({"type": "direct", "users": [XIAOYU]});
    plugin_call(
        &fixture,
        &plugin,
        &session,
        "schedule",
        json!({"conv": conv, "text": "以后再发", "at": at}),
    )
    .await;
    wait_until("定时消息", || {
        session
            .scheduled_for(&key)
            .iter()
            .any(|m| m.content == "以后再发")
    })
    .await;

    // 话题：标记已解决、取消、删除。
    let resolved = plugin_call(
        &fixture,
        &plugin,
        &session,
        "resolve",
        json!({"stream": DEV, "topic": topic}),
    )
    .await;
    assert_eq!(resolved, "调用 resolve：ok");
    let new_name = format!("✔ {topic}");
    wait_log(&plugin, &format!("移动 {DEV}/{topic} -> {DEV}/{new_name}")).await;
    let deleted = plugin_call(
        &fixture,
        &plugin,
        &session,
        "delete_topic",
        json!({"stream": DEV, "topic": new_name}),
    )
    .await;
    assert_eq!(deleted, "调用 delete_topic：ok");

    // 打开会话、通知（走宿主事件）。
    let mut events = fixture.host.subscribe();
    plugin_call(
        &fixture,
        &plugin,
        &session,
        "open",
        json!({"conv": conv, "id": id}),
    )
    .await;
    plugin_call(
        &fixture,
        &plugin,
        &session,
        "toast",
        json!({"text": "提示"}),
    )
    .await;
    plugin_call(
        &fixture,
        &plugin,
        &session,
        "notify",
        json!({"title": "标题", "text": "正文"}),
    )
    .await;
    let missing = plugin_call(
        &fixture,
        &plugin,
        &session,
        "open",
        json!({"conv": {"type": "channel", "stream": 987654, "topic": "x"}}),
    )
    .await;
    assert!(missing.contains("失败"), "{missing}");
    let (mut opened, mut toast, mut notice) = (false, false, false);
    while let Ok(event) = events.try_recv() {
        match event {
            PluginEvent::OpenConversation {
                key: opened_key,
                message_id,
                ..
            } => {
                assert_eq!(opened_key, key);
                assert_eq!(message_id, Some(id));
                opened = true;
            }
            PluginEvent::Toast { text, .. } => {
                assert_eq!(text, "提示");
                toast = true;
            }
            PluginEvent::SystemNotification { title, body, .. } => {
                assert_eq!((title.as_str(), body.as_str()), ("标题", "正文"));
                notice = true;
            }
            _ => {}
        }
    }
    assert!(opened && toast && notice);

    // 受限请求。
    let reply = plugin_call(
        &fixture, &plugin, &session, "api",
        json!({"method": "GET", "path": format!("/api/v1/messages/{id}"), "params": {"apply_markdown": false}}),
    )
    .await;
    assert!(reply.contains("\"result\":\"success\""), "{reply}");
    assert!(has_log(&plugin, "api_request：GET"));
    for path in ["/user_uploads/x", "/api/v1/../x", "/api/v1/fetch_api_key"] {
        let reply = plugin_call(
            &fixture,
            &plugin,
            &session,
            "api",
            json!({"method": "GET", "path": path}),
        )
        .await;
        assert!(reply.contains("没有权限"), "{path}：{reply}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn new_permissions_are_enforced_and_warned() {
    let server = TestServer::start(0).await.unwrap();
    let mut without_dialog = manifest();
    without_dialog["permissions"]
        .as_array_mut()
        .unwrap()
        .retain(|permission| permission != "ui_dialog");
    let fixture = Fixture::new(&without_dialog).await;
    let plugin = fixture.enable().await;
    let session = fixture.login(&server).await;
    for (op, args, permission) in [
        (
            "api",
            json!({"method": "GET", "path": "/api/v1/users"}),
            "zulip_api",
        ),
        (
            "download",
            json!({"path": "/user_uploads/a"}),
            "read_attachment",
        ),
        (
            "delete_topic",
            json!({"stream": DEV, "topic": "发布计划"}),
            "manage_topic",
        ),
        (
            "open",
            json!({"conv": {"type": "direct", "users": []}}),
            "open_conversation",
        ),
        (
            "subscribed",
            json!({"stream": DEV, "on": false}),
            "manage_subscription",
        ),
        (
            "set_visibility",
            json!({"stream": DEV, "topic": "发布计划", "v": 1}),
            "manage_subscription",
        ),
        (
            "subscription_property",
            json!({"stream": DEV, "name": "is_muted", "value": true}),
            "manage_subscription",
        ),
        (
            "create_channel",
            json!({"name": "x"}),
            "manage_subscription",
        ),
        (
            "mute",
            json!({"id": XIAOYU, "on": true}),
            "manage_preferences",
        ),
        (
            "alert_word",
            json!({"word": "x", "on": true}),
            "manage_preferences",
        ),
        ("status", json!({"text": "x"}), "manage_preferences"),
        (
            "set_level",
            json!({"conv": {"type": "direct", "users": [XIAOYU]}, "level": 1}),
            "manage_preferences",
        ),
        (
            "pin",
            json!({"conv": {"type": "direct", "users": [XIAOYU]}, "on": true}),
            "manage_preferences",
        ),
        ("dialog_here", json!({"kind": "confirm"}), "ui_dialog"),
        ("upload_ex", json!({"path": "plugin.json"}), "upload_file"),
        (
            "download_ex",
            json!({"path": "/user_uploads/a"}),
            "read_attachment",
        ),
    ] {
        let result = plugin_call(&fixture, &plugin, &session, op, args).await;
        assert!(result.contains("没有权限"), "{op}：{result}");
        assert!(
            has_log(&plugin, &format!("未声明权限 {permission}")),
            "{op}"
        );
    }

    let fixture = Fixture::new(&with_all_permissions()).await;
    let plugin = fixture.plugin();
    let ToggleOutcome::NeedsApproval(info) = fixture.host.toggle(&plugin, true).await else {
        panic!("新插件应要求确认");
    };
    assert!(
        info.acts
            .iter()
            .any(|a| a.starts_with("【高危】") && a.contains("Zulip API"))
    );
    assert!(info.reads.iter().any(|r| r == "下载消息中的附件"));
    assert!(info.acts.iter().any(|a| a == "显示通知和状态栏提示"));
    assert!(info.acts.iter().any(|a| a.starts_with("订阅和退订频道")));
    assert!(info.acts.iter().any(|a| a.starts_with("修改免打扰")));
    assert!(info.acts.iter().any(|a| a == "弹出对话框"));
    for expected in ["对方正在输入", "草稿变化", "静音用户变化", "组织设置变化"]
    {
        assert!(info.events.iter().any(|e| e == expected), "{expected}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn context_menus_composer_panel_and_status() {
    let server = TestServer::start(0).await.unwrap();
    let fixture = Fixture::new(&manifest()).await;
    let plugin = fixture.enable().await;
    let session = fixture.login(&server).await;
    let host = &fixture.host;
    let mut events = host.subscribe();

    // 右键菜单：子菜单、目标 JSON、状态栏提示。
    assert_eq!(
        plugin.active_context_menus(ContextMenuKind::Conversation)[0].id,
        1
    );
    assert_eq!(
        plugin.active_context_menus(ContextMenuKind::User)[0].children[0].id,
        2
    );
    assert_eq!(
        plugin.active_context_menus(ContextMenuKind::Channel)[0].id,
        3
    );
    host.invoke_context_menu(&plugin, 2, &session, &ContextMenuTarget::User(XIAOYU));
    wait_log(&plugin, "右键菜单 2：用户 ").await;
    host.invoke_context_menu(
        &plugin,
        1,
        &session,
        &ContextMenuTarget::Conversation(ConversationKey::topic(DEV, "发布计划")),
    );
    wait_log(&plugin, "右键菜单 1：频道").await;
    host.invoke_context_menu(&plugin, 3, &session, &ContextMenuTarget::Channel(DEV));
    wait_log(&plugin, "右键菜单 3：频道 ").await;
    wait_until("状态栏提示", || {
        host.status_text()
            .is_some_and(|s| s.text.starts_with("回声：频道 "))
    })
    .await;
    assert_eq!(host.status_text().unwrap().plugin_name, "回声示例");
    host.toggle(&plugin, false).await;
    assert_eq!(host.status_text(), None);
    host.toggle(&plugin, true).await;

    // 工具栏按钮：替换输入框内容。
    let buttons = host.composer_buttons();
    assert_eq!(buttons[0].icon, ComposerIcon::Builtin("edit".to_owned()));
    assert_eq!(buttons[0].tooltip, "回声：替换输入框");
    host.invoke_composer_button(&plugin, 1, &session, &dm_key(&session), "原文");
    let (text, replace) = next_fill(&mut events).await;
    assert_eq!(text, "回声：原文");
    assert!(replace);

    // 面板的新字段类型。
    let target = PluginTarget::User {
        account_id: session.account_id(),
        user_id: XIAOYU,
        name: "小雨".to_owned(),
    };
    let reply = host
        .panel(&plugin, PanelRequest::Get(target.clone()))
        .await
        .unwrap()
        .unwrap();
    let fields = parse_fields(&reply);
    assert!(matches!(fields[1].value, PanelFieldValue::Text { .. }));
    assert_eq!(fields[2].value, PanelFieldValue::Button);
    let reply = host
        .panel(
            &plugin,
            PanelRequest::Click {
                target,
                key: "ping".to_owned(),
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        parse_fields(&reply)[3].value,
        PanelFieldValue::Info("点击了 ping".to_owned())
    );

    // 超时与正常完成（remove 不回复）可以区分。
    let slow = host
        .panel(
            &plugin,
            PanelRequest::Click {
                target: target_of(&session),
                key: "echo:slow".to_owned(),
            },
        )
        .await;
    assert_eq!(slow.unwrap_err(), PanelError::Timeout);
    tokio::time::sleep(Duration::from_millis(600)).await;
    let removed = host
        .panel(&plugin, PanelRequest::Remove(target_of(&session)))
        .await;
    assert_eq!(removed, Ok(None));
    host.toggle(&plugin, false).await;
    let inactive = host
        .panel(&plugin, PanelRequest::Get(target_of(&session)))
        .await;
    assert_eq!(inactive, Err(PanelError::Inactive));
}

fn target_of(session: &Session) -> PluginTarget {
    PluginTarget::User {
        account_id: session.account_id(),
        user_id: XIAOYU,
        name: "小雨".to_owned(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn composer_button_svg_icon_is_inside_plugin_directory() {
    let mut value = manifest();
    value["composer_buttons"] = json!([{"id": 1, "icon": "icons/a.svg", "tooltip": "图标"}]);
    let fixture = Fixture::new(&value).await;
    let plugin = fixture.enable().await;
    let ComposerIcon::Svg(path) = &fixture.host.composer_buttons()[0].icon.clone() else {
        panic!("应为 SVG 图标");
    };
    assert_eq!(Path::new(path), plugin.directory().join("icons/a.svg"));
}

#[tokio::test(flavor = "multi_thread")]
async fn session_send_goes_through_filter_outgoing() {
    let server = TestServer::start(0).await.unwrap();
    let fixture = Fixture::new(&manifest()).await;
    let plugin = fixture.enable().await;
    let session = fixture.login(&server).await;
    let key = dm_key(&session);

    // 提前过滤过的内容只过滤一次，而且只免一次。
    let filtered = fixture
        .host
        .filter_outgoing(&session, &key, "echo:rewrite echo:rewrite 二")
        .await
        .unwrap();
    assert_eq!(filtered, "echo:rewrite 二");
    let sent = session.send_message(&key, &filtered).await.unwrap();
    assert_eq!(server.content_of(sent).as_deref(), Some("echo:rewrite 二"));
    let sent = session.send_message(&key, &filtered).await.unwrap();
    assert_eq!(server.content_of(sent).as_deref(), Some("二"));

    // 文字：拦截的原因可以直接显示；不产生回显。
    let error = session.send_message(&key, "echo:block").await.unwrap_err();
    assert!(matches!(error, SessionError::Invalid(_)));
    assert_eq!(error.to_string(), "消息已被插件拦截");
    assert!(
        !session
            .timeline(&key)
            .messages
            .iter()
            .any(|m| m.content_html.contains("echo:block"))
    );

    // 改写。
    let sent = session
        .send_message(&key, "echo:rewrite 新内容")
        .await
        .unwrap();
    assert_eq!(server.content_of(sent).as_deref(), Some("新内容"));

    // 文件。
    let sent = session
        .send_as(&key, "[a.txt](/user_uploads/a.txt)", OutgoingKind::File)
        .await
        .unwrap();
    assert!(sent > 0);
    wait_log(&plugin, "发送种类 File").await;
    let blocked = session
        .send_as(
            &key,
            "echo:block [a.txt](/user_uploads/a.txt)",
            OutgoingKind::File,
        )
        .await;
    assert_eq!(blocked.unwrap_err().to_string(), "消息已被插件拦截");

    // 转发：逐条和合并。
    let original = server.send_as(XIAOYU, Target::Dm(vec![ME_ID]), "转发我");
    session.load_newest(&key).await.unwrap();
    assert!(session.message(original).is_some());
    let target = ConversationKey::dm([AJIE], session.self_id());
    let latest = server.latest_message_id();
    session
        .forward_each(&[original], std::slice::from_ref(&target))
        .await
        .unwrap();
    wait_log(&plugin, "发送种类 Forward").await;
    assert!(server.latest_message_id() > latest);
    let blocked = server.send_as(XIAOYU, Target::Dm(vec![ME_ID]), "正文 echo:block");
    let result = session
        .forward_each(&[blocked], std::slice::from_ref(&target))
        .await;
    assert_eq!(result.unwrap_err().to_string(), "消息已被插件拦截");
    let result = session
        .forward_merged(&[original], std::slice::from_ref(&target))
        .await;
    assert!(result.is_ok());
}

async fn next_event(
    events: &mut tokio::sync::broadcast::Receiver<PluginEvent>,
    what: &str,
    matches: impl Fn(&PluginEvent) -> bool,
) -> PluginEvent {
    let start = Instant::now();
    loop {
        match events.try_recv() {
            Ok(event) if matches(&event) => return event,
            Ok(_) => {}
            Err(_) => {
                assert!(start.elapsed() < Duration::from_secs(5), "没有收到：{what}");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

const DEPENDENT: &str = "com.yaz.echo2";

fn install_dependent(dir: &TempDir) {
    let mut value = manifest();
    value["id"] = json!(DEPENDENT);
    value["name"] = json!("依赖回声");
    value["dependencies"] = json!([ID]);
    let target = dir.path().join("plugins").join(DEPENDENT);
    std::fs::create_dir_all(&target).unwrap();
    std::fs::copy(built_echo(), target.join("plugin_echo.dll")).unwrap();
    std::fs::write(target.join("plugin.json"), value.to_string()).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn enabling_after_login_announces_accounts() {
    let server = TestServer::start(0).await.unwrap();
    let fixture = Fixture::new(&manifest()).await;
    let session = fixture.login(&server).await;
    let plugin = fixture.enable().await;
    let expected = format!("账号 {} 已登录", session.account_id());
    wait_log(&plugin, "已开启").await;
    wait_log(&plugin, &expected).await;

    // 再次开启时重新通知；开启一次只通知一次。
    fixture.host.toggle(&plugin, false).await;
    wait_log(&plugin, "已停用").await;
    plugin.clear_logs();
    fixture.host.toggle(&plugin, true).await;
    wait_log(&plugin, &expected).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        plugin.logs().iter().filter(|l| l.text == expected).count(),
        1
    );

    // 补发过的账号退出时有对应的 logout。
    plugin.clear_logs();
    fixture.hub.logout(session.account_id(), false).await;
    wait_log(&plugin, &format!("账号 {} 已退出", session.account_id())).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dependents_follow_the_dependency() {
    let dir = temp_dir();
    install(&dir, &manifest());
    install_dependent(&dir);
    let paths = AppPaths::new(dir.path().join("data"));
    std::fs::create_dir_all(paths.root()).unwrap();
    let db = Db::open(&paths.database()).unwrap();
    let (host, _hub) = start_host(&dir, &db, &paths).await;
    let base = host.plugin(ID).unwrap();
    let dependent = host.plugin(DEPENDENT).unwrap();

    // 依赖未开启时不能开启。
    assert!(matches!(
        host.toggle(&dependent, true).await,
        ToggleOutcome::MissingDependencies(_)
    ));
    assert!(matches!(
        host.toggle(&base, true).await,
        ToggleOutcome::NeedsApproval(_)
    ));
    assert!(host.approve(&base).await);
    assert!(matches!(
        host.toggle(&dependent, true).await,
        ToggleOutcome::NeedsApproval(_)
    ));
    assert!(host.approve(&dependent).await);
    assert!(dependent.is_enabled());

    // 停用依赖：联动停用并写日志，重新开启不恢复。
    assert!(matches!(
        host.toggle(&base, false).await,
        ToggleOutcome::Done
    ));
    assert!(!dependent.is_enabled());
    assert!(has_log(&dependent, "依赖的插件“回声示例”已停用"));
    assert!(matches!(
        host.toggle(&base, true).await,
        ToggleOutcome::Done
    ));
    assert!(base.is_enabled());
    assert!(!dependent.is_enabled());
    assert!(matches!(
        host.toggle(&dependent, true).await,
        ToggleOutcome::Done
    ));
    assert!(dependent.is_enabled());

    // 启动时依赖未开启（文件变化，需要重新确认）：不激活，写日志。
    let mut changed = manifest();
    changed["description"] = json!("改过");
    std::fs::write(
        dir.path().join("plugins").join(ID).join("plugin.json"),
        changed.to_string(),
    )
    .unwrap();
    let (again, _hub) = start_host(&dir, &db, &paths).await;
    assert!(!again.plugin(ID).unwrap().is_enabled());
    let waiting = again.plugin(DEPENDENT).unwrap();
    assert!(!waiting.is_enabled());
    assert!(!waiting.is_loaded());
    assert!(has_log(&waiting, "依赖的插件未开启：回声示例，本次不激活"));
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_enable_rechecks_the_fingerprint() {
    let fixture = Fixture::new(&manifest()).await;
    let plugin = fixture.enable().await;
    let host = &fixture.host;

    // 文件没有变化：不需要确认。
    host.toggle(&plugin, false).await;
    assert!(matches!(
        host.toggle(&plugin, true).await,
        ToggleOutcome::Done
    ));

    // 文件变化：走确认，确认框显示磁盘上的清单。
    host.toggle(&plugin, false).await;
    let mut changed = manifest();
    changed["permissions"] = json!(["read_message", "zulip_api"]);
    std::fs::write(
        plugin_dir(&fixture.dir).join("plugin.json"),
        changed.to_string(),
    )
    .unwrap();
    let ToggleOutcome::NeedsApproval(info) = host.toggle(&plugin, true).await else {
        panic!("文件变化后应要求确认");
    };
    assert!(info.notice.is_some_and(|n| n.contains("重启程序")));
    assert!(info.acts.iter().any(|a| a.starts_with("【高危】")));
    assert!(!plugin.is_enabled());
    assert!(plugin.needs_reapproval());
    assert!(has_log(&plugin, "需要重新开启"));

    assert!(host.approve(&plugin).await);
    assert!(plugin.is_enabled());
    assert!(!plugin.needs_reapproval());
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_plugin_calls_are_refused() {
    let server = TestServer::start(0).await.unwrap();
    let fixture = Fixture::new(&with_all_permissions()).await;
    let plugin = fixture.enable().await;
    let session = fixture.login(&server).await;

    plugin_call(&fixture, &plugin, &session, "after_disable", json!({})).await;
    fixture.host.toggle(&plugin, false).await;
    // on_disable 回调内仍可调用。
    wait_log(&plugin, "停用回调中：Ok(())").await;
    // 之后拒绝，只有 add_log、get_data_dir、get_settings 例外。
    wait_log(&plugin, "停用后调用：Err(Error(-10)) Ok(()) true").await;
    assert!(has_log(&plugin, "show_toast：插件已停用"));
}

#[tokio::test(flavor = "multi_thread")]
async fn abi_mismatches_are_fixed() {
    let server = TestServer::start(0).await.unwrap();
    let fixture = Fixture::new(&with_all_permissions()).await;
    let plugin = fixture.enable().await;
    let session = fixture.login(&server).await;

    // count 为 0：与自己私聊。
    let sent = plugin_call(
        &fixture,
        &plugin,
        &session,
        "send_self",
        json!({"text": "给自己"}),
    )
    .await;
    let id: i64 = sent.trim_start_matches("调用 send_self：").parse().unwrap();
    assert_eq!(server.content_of(id).as_deref(), Some("给自己"));

    // 部分消息超出移动时限：ZULIP_PLUGIN_ERR_SERVER。
    let first = server.send_as(AJIE, Target::Stream(DEV, "限时".to_owned()), "旧");
    let latest = server.send_as(AJIE, Target::Stream(DEV, "限时".to_owned()), "新");
    assert!(latest > first);
    server.set_move_limit(Some(latest));
    let moved = plugin_call(
        &fixture,
        &plugin,
        &session,
        "move_topic",
        json!({"stream": DEV, "topic": "限时", "new_topic": "别处"}),
    )
    .await;
    assert!(moved.contains("（-6）"), "{moved}");
    let last = plugin_call(&fixture, &plugin, &session, "last_error", json!({})).await;
    assert!(last.contains("move_topic："), "{last}");
    assert!(last.contains("条可以移动"), "{last}");
}

#[tokio::test(flavor = "multi_thread")]
async fn thread_account_priority() {
    let server = TestServer::start(0).await.unwrap();
    let other = TestServer::start(0).await.unwrap();
    let fixture = Fixture::new(&with_all_permissions()).await;
    let plugin = fixture.enable().await;
    let first = fixture.login(&server).await;
    let second = fixture.login(&other).await;
    let (a, b) = (first.account_id(), second.account_id());
    assert_ne!(a, b);

    // 工作线程：绑定优先于最先登录的账号，解除后恢复。
    let result = plugin_call(
        &fixture,
        &plugin,
        &first,
        "thread_account",
        json!({"account": b}),
    )
    .await;
    assert_eq!(
        result,
        format!("调用 thread_account：Ok({a}) Ok(()) Ok({b}) Ok(()) Ok({a})")
    );
    // 账号没有登录。
    let result = plugin_call(
        &fixture,
        &plugin,
        &first,
        "thread_account",
        json!({"account": 987654}),
    )
    .await;
    assert!(result.contains("Err(Error(-7))"), "{result}");

    // 回调账号优先于绑定。
    let result = plugin_call(
        &fixture,
        &plugin,
        &first,
        "bind_here",
        json!({"account": b}),
    )
    .await;
    assert_eq!(result, format!("调用 bind_here：Ok(()) Ok({a})"));

    // 绑定的账号退出后不改用其他账号。
    plugin_call(
        &fixture,
        &plugin,
        &first,
        "bind_wait",
        json!({"account": b}),
    )
    .await;
    fixture.hub.logout(b, false).await;
    wait_log(&plugin, "绑定后账号：Ok(()) Err(Error(-4))").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn messages_and_queues_through_the_plugin() {
    let server = TestServer::start(0).await.unwrap();
    let fixture = Fixture::new(&with_all_permissions()).await;
    let plugin = fixture.enable().await;
    let session = fixture.login(&server).await;
    let key = dm_key(&session);
    let conv = json!({"type": "direct", "users": [XIAOYU]});

    // 上次失败的详情。
    let before = plugin_call(&fixture, &plugin, &session, "last_error", json!({})).await;
    assert_eq!(before, "调用 last_error：");
    let failed = plugin_call(&fixture, &plugin, &session, "source", json!({"id": 987654})).await;
    assert!(failed.contains("失败"), "{failed}");
    let last = plugin_call(&fixture, &plugin, &session, "last_error", json!({})).await;
    assert!(last.contains("get_message_source："), "{last}");

    // 原文、已读、可用操作。
    let id = session.send_message(&key, "**原文**").await.unwrap();
    let source = plugin_call(&fixture, &plugin, &session, "source", json!({"id": id})).await;
    assert_eq!(source, "调用 source：**原文**");
    let receipts = plugin_call(&fixture, &plugin, &session, "receipts", json!({"id": id})).await;
    assert!(receipts.starts_with("调用 receipts：["), "{receipts}");
    session.load_newest(&key).await.unwrap();
    let actions = plugin_call(&fixture, &plugin, &session, "actions", json!({"id": id})).await;
    assert!(actions.contains("\"can_edit\":true"), "{actions}");
    let missing = plugin_call(
        &fixture,
        &plugin,
        &session,
        "actions",
        json!({"id": 987654}),
    )
    .await;
    assert!(missing.contains("不存在"), "{missing}");

    // 定时消息。
    let at = chrono::Utc::now().timestamp() + 3600;
    let scheduled = plugin_call(
        &fixture,
        &plugin,
        &session,
        "schedule",
        json!({"conv": conv, "text": "以后再发", "at": at}),
    )
    .await;
    wait_log(&plugin, "定时消息变化").await;
    let scheduled_id: i64 = scheduled
        .trim_start_matches("调用 schedule：")
        .parse()
        .unwrap();
    let list = plugin_call(&fixture, &plugin, &session, "scheduled", json!({})).await;
    assert!(list.contains("以后再发"), "{list}");
    assert!(list.contains(&format!("[{XIAOYU}]")), "{list}");
    plugin_call(
        &fixture,
        &plugin,
        &session,
        "cancel_scheduled",
        json!({"id": scheduled_id}),
    )
    .await;
    wait_until("定时消息已取消", || {
        session
            .scheduled_messages()
            .iter()
            .all(|m| m.id != scheduled_id)
    })
    .await;

    // 提醒。
    let reminder = plugin_call(
        &fixture,
        &plugin,
        &session,
        "remind",
        json!({"id": id, "at": at}),
    )
    .await;
    wait_log(&plugin, "提醒变化").await;
    let reminder_id: i64 = reminder
        .trim_start_matches("调用 remind：")
        .parse()
        .unwrap();
    let list = plugin_call(&fixture, &plugin, &session, "reminders", json!({})).await;
    assert!(list.contains(&format!("\"id\":{reminder_id}")), "{list}");
    assert!(list.contains(&format!("\"message_id\":{id}")), "{list}");
    plugin_call(
        &fixture,
        &plugin,
        &session,
        "cancel_reminder",
        json!({"id": reminder_id}),
    )
    .await;
    wait_until("提醒已取消", || session.pending_reminders().is_empty()).await;

    // 草稿。
    plugin_call(
        &fixture,
        &plugin,
        &session,
        "save_draft",
        json!({"conv": conv, "text": "写了一半"}),
    )
    .await;
    assert_eq!(session.draft(&key).unwrap().content, "写了一半");
    let list = plugin_call(&fixture, &plugin, &session, "drafts", json!({})).await;
    assert!(list.contains("写了一半"), "{list}");
    plugin.clear_logs();
    server.push_event(json!({
        "type": "drafts", "op": "add",
        "drafts": [{"id": 9001, "type": "private", "to": [AJIE], "topic": "", "content": "别处写的", "timestamp": 1}],
    }));
    wait_log(&plugin, "草稿变化").await;

    // 上传、下载的进度。
    let file = plugin_dir(&fixture.dir).join("plugin.json");
    let size = std::fs::metadata(&file).unwrap().len();
    let uploaded = plugin_call(
        &fixture,
        &plugin,
        &session,
        "upload_ex",
        json!({"path": file}),
    )
    .await;
    assert!(uploaded.contains("/user_uploads/"), "{uploaded}");
    assert!(
        uploaded.ends_with(&format!("进度 {size} {size}")),
        "{uploaded}"
    );
    let url = uploaded
        .trim_start_matches("调用 upload_ex：")
        .split(' ')
        .next()
        .unwrap();
    let downloaded = plugin_call(
        &fixture,
        &plugin,
        &session,
        "download_ex",
        json!({"path": url}),
    )
    .await;
    assert!(downloaded.ends_with("进度 true"), "{downloaded}");
}

#[tokio::test(flavor = "multi_thread")]
async fn subscriptions_and_preferences_through_the_plugin() {
    let server = TestServer::start(0).await.unwrap();
    let fixture = Fixture::new(&with_all_permissions()).await;
    let plugin = fixture.enable().await;
    let session = fixture.login(&server).await;
    let key = dm_key(&session);
    let conv = json!({"type": "direct", "users": [XIAOYU]});

    // 话题关注。
    plugin_call(
        &fixture,
        &plugin,
        &session,
        "set_visibility",
        json!({"stream": DEV, "topic": "发布计划", "v": 3}),
    )
    .await;
    wait_log(&plugin, "话题关注 3").await;
    let visibility = plugin_call(
        &fixture,
        &plugin,
        &session,
        "visibility",
        json!({"stream": DEV, "topic": "发布计划"}),
    )
    .await;
    assert_eq!(visibility, "调用 visibility：3");
    let bad = plugin_call(
        &fixture,
        &plugin,
        &session,
        "set_visibility",
        json!({"stream": DEV, "topic": "发布计划", "v": 9}),
    )
    .await;
    assert!(bad.contains("参数错误"), "{bad}");

    // 订阅者、订阅、频道个人设置、创建频道。
    let members = plugin_call(
        &fixture,
        &plugin,
        &session,
        "subscribers",
        json!({"stream": DEV}),
    )
    .await;
    assert!(members.contains(&ME_ID.to_string()), "{members}");
    let result = plugin_call(
        &fixture,
        &plugin,
        &session,
        "subscribed",
        json!({"stream": DEV, "on": false}),
    )
    .await;
    assert_eq!(result, "调用 subscribed：ok");
    wait_until("已退订", || {
        !session.channel(DEV).is_some_and(|c| c.is_subscribed())
    })
    .await;
    let result = plugin_call(
        &fixture,
        &plugin,
        &session,
        "subscribed",
        json!({"stream": DEV, "on": true}),
    )
    .await;
    assert_eq!(result, "调用 subscribed：ok");
    wait_until("已订阅", || {
        session.channel(DEV).is_some_and(|c| c.is_subscribed())
    })
    .await;
    let missing = plugin_call(
        &fixture,
        &plugin,
        &session,
        "subscribed",
        json!({"stream": 987654, "on": true}),
    )
    .await;
    assert!(missing.contains("不存在"), "{missing}");
    let result = plugin_call(
        &fixture,
        &plugin,
        &session,
        "subscription_property",
        json!({"stream": DEV, "name": "is_muted", "value": true}),
    )
    .await;
    assert_eq!(result, "调用 subscription_property：ok");
    let created = plugin_call(
        &fixture,
        &plugin,
        &session,
        "create_channel",
        json!({"name": "插件频道", "description": "说明", "subscribers": [ME_ID]}),
    )
    .await;
    let stream: i64 = created
        .trim_start_matches("调用 create_channel：")
        .parse()
        .unwrap_or_else(|_| panic!("{created}"));
    assert!(stream > 0);
    let empty = plugin_call(&fixture, &plugin, &session, "create_channel", json!({})).await;
    assert!(empty.contains("参数错误"), "{empty}");

    // 免打扰、置顶。
    let level = plugin_call(&fixture, &plugin, &session, "level", json!({"conv": conv})).await;
    assert_eq!(level, "调用 level：0");
    plugin_call(
        &fixture,
        &plugin,
        &session,
        "set_level",
        json!({"conv": conv, "level": 1}),
    )
    .await;
    let level = plugin_call(&fixture, &plugin, &session, "level", json!({"conv": conv})).await;
    assert_eq!(level, "调用 level：1");
    let bad = plugin_call(
        &fixture,
        &plugin,
        &session,
        "set_level",
        json!({"conv": conv, "level": 9}),
    )
    .await;
    assert!(bad.contains("参数错误"), "{bad}");
    plugin_call(
        &fixture,
        &plugin,
        &session,
        "pin",
        json!({"conv": conv, "on": true}),
    )
    .await;
    assert!(session.is_pinned(
        zulip_core::model::PinScope::Messages,
        &zulip_core::model::PinItem::Conversation(key.clone())
    ));

    // 静音、提醒词、签名。
    plugin_call(
        &fixture,
        &plugin,
        &session,
        "mute",
        json!({"id": XIAOYU, "on": true}),
    )
    .await;
    wait_log(&plugin, &format!("静音 [{XIAOYU}]")).await;
    let muted = plugin_call(&fixture, &plugin, &session, "muted", json!({})).await;
    assert_eq!(muted, format!("调用 muted：[{XIAOYU}]"));
    plugin_call(
        &fixture,
        &plugin,
        &session,
        "alert_word",
        json!({"word": "插件词", "on": true}),
    )
    .await;
    wait_until("提醒词", || {
        session.alert_words().iter().any(|w| w == "插件词")
    })
    .await;
    let words = plugin_call(&fixture, &plugin, &session, "alert_words", json!({})).await;
    assert!(words.contains("插件词"), "{words}");
    plugin_call(
        &fixture,
        &plugin,
        &session,
        "alert_word",
        json!({"word": "插件词", "on": false}),
    )
    .await;
    wait_until("提醒词已删除", || {
        !session.alert_words().iter().any(|w| w == "插件词")
    })
    .await;
    let status = plugin_call(
        &fixture,
        &plugin,
        &session,
        "status",
        json!({"text": "开会"}),
    )
    .await;
    assert_eq!(status, "调用 status：ok");
    wait_until("签名", || {
        session.status_text(ME_ID).as_deref() == Some("开会")
    })
    .await;

    // 组织信息和通知。
    let realm = plugin_call(&fixture, &plugin, &session, "realm", json!({})).await;
    assert!(realm.contains("\"max_message_length\""), "{realm}");
    server.push_event(
        json!({"type": "realm", "op": "update", "property": "name", "value": "新名字"}),
    );
    wait_log(&plugin, "组织 [\"name\"]").await;
    let realm = plugin_call(&fixture, &plugin, &session, "realm", json!({})).await;
    assert!(realm.contains("新名字"), "{realm}");

    // 输入状态（XIAOYU 已静音，用 AJIE）。
    let recipients = json!([{"user_id": AJIE}, {"user_id": ME_ID}]);
    server.push_event(json!({
        "type": "typing", "op": "start", "sender": {"user_id": AJIE}, "recipients": recipients,
    }));
    wait_log(&plugin, &format!("输入 {AJIE} true")).await;
    server.push_event(json!({
        "type": "typing", "op": "stop", "sender": {"user_id": AJIE}, "recipients": recipients,
    }));
    wait_log(&plugin, &format!("输入 {AJIE} false")).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dialog_settings_and_fill_through_the_plugin() {
    let server = TestServer::start(0).await.unwrap();
    let mut value = with_all_permissions();
    value["settings"] = json!([
        {"key": "reply", "label": "回复内容", "default": "pong"},
        {"key": "secret", "label": "口令", "type": "password"},
    ]);
    let fixture = Fixture::new(&value).await;
    let plugin = fixture.enable().await;
    let session = fixture.login(&server).await;
    let host = &fixture.host;
    let mut events = host.subscribe();

    // 对话框：插件线程上不能调用；工作线程上阻塞到回复。
    let refused = plugin_call(
        &fixture,
        &plugin,
        &session,
        "dialog_here",
        json!({"kind": "confirm"}),
    )
    .await;
    assert!(refused.contains("不能在这里调用"), "{refused}");
    plugin_call(
        &fixture,
        &plugin,
        &session,
        "dialog",
        json!({"kind": "input", "title": "标题", "text": "说明"}),
    )
    .await;
    let event = next_event(&mut events, "对话框", |e| {
        matches!(e, PluginEvent::ShowDialog { .. })
    })
    .await;
    let PluginEvent::ShowDialog {
        request_id,
        plugin_id,
        account_id,
        spec,
    } = event
    else {
        unreachable!();
    };
    assert_eq!(plugin_id, ID);
    assert_eq!(account_id, session.account_id());
    assert_eq!(spec.kind, DialogKind::Input);
    assert_eq!(spec.title, "标题");
    let pending = host.pending_dialogs();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request_id, request_id);
    assert_eq!(pending[0].plugin_id, ID);
    let result = DialogResult {
        ok: true,
        values: Map::from_iter([("value".to_owned(), json!("答案"))]),
    };
    assert!(host.reply_dialog(request_id, result.clone()));
    assert!(host.pending_dialogs().is_empty());
    wait_log(&plugin, "答案").await;
    assert!(!host.reply_dialog(request_id, result));
    plugin_call(&fixture, &plugin, &session, "dialog", json!({"kind": "x"})).await;
    wait_log(&plugin, "对话框结果：Err(Error(-2))").await;

    // set_settings：合并键、加密、发事件、不回调。
    let result = plugin_call(
        &fixture,
        &plugin,
        &session,
        "set_settings",
        json!({"values": {"secret": "口令值"}}),
    )
    .await;
    assert_eq!(result, "调用 set_settings：ok");
    let event = next_event(&mut events, "设置变化", |e| {
        matches!(e, PluginEvent::SettingsChanged(_))
    })
    .await;
    assert!(matches!(event, PluginEvent::SettingsChanged(id) if id == ID));
    let values = host.load_settings(&plugin);
    assert_eq!(values["reply"], "pong");
    assert_eq!(values["secret"], "口令值");
    let key = format!("plugin_settings:{ID}");
    let stored = SettingRepo::app(&fixture.db).get(&key).unwrap();
    assert!(!stored.contains("口令值"), "{stored}");
    assert!(stored.contains("dpapi:"), "{stored}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!has_log(&plugin, "设置已变化"));
    let bad = plugin_call(
        &fixture,
        &plugin,
        &session,
        "set_settings",
        json!({"values": [1]}),
    )
    .await;
    assert!(bad.contains("参数错误"), "{bad}");

    // 用户保存时回调。
    host.save_settings(&plugin, &values).unwrap();
    wait_log(&plugin, "设置已变化").await;

    // 无法解密的密文：读取时为空并写日志，保存空值时保留。
    SettingRepo::app(&fixture.db).set(&key, r#"{"reply": "x", "secret": "dpapi:AAAA"}"#);
    let (again, _hub) = start_host(&fixture.dir, &fixture.db, &fixture.paths).await;
    let reloaded = again.plugin(ID).unwrap();
    assert!(has_log(&reloaded, "无法解密设置项 secret"));
    let values = again.load_settings(&reloaded);
    assert_eq!(values["secret"], "");
    again.save_settings(&reloaded, &values).unwrap();
    let stored = SettingRepo::app(&fixture.db).get(&key).unwrap();
    assert!(stored.contains("dpapi:AAAA"), "{stored}");

    // fill_composer_to：事件带会话；fill_composer 不带。
    plugin_call(
        &fixture,
        &plugin,
        &session,
        "fill_to",
        json!({"conv": {"type": "direct", "users": [XIAOYU]}, "text": "填入", "on": false}),
    )
    .await;
    let event = next_event(&mut events, "填入", |e| {
        matches!(e, PluginEvent::FillComposer { .. })
    })
    .await;
    let PluginEvent::FillComposer {
        conv,
        text,
        replace,
        ..
    } = event
    else {
        unreachable!();
    };
    assert_eq!(conv, Some(dm_key(&session)));
    assert_eq!(text, "填入");
    assert!(!replace);
    host.invoke_composer_button(&plugin, 1, &session, &dm_key(&session), "原文");
    let event = next_event(&mut events, "填入", |e| {
        matches!(e, PluginEvent::FillComposer { .. })
    })
    .await;
    assert!(matches!(
        event,
        PluginEvent::FillComposer { conv: None, .. }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn message_menu_conditions_come_from_the_manifest() {
    let fixture = Fixture::new(&manifest()).await;
    let plugin = fixture.enable().await;
    let menus = plugin.active_message_menus();
    assert_eq!(menus.len(), 2);
    let own_direct = MenuContext {
        conv: MenuConv::Direct,
        own: true,
        ..MenuContext::default()
    };
    assert!(menus[0].visible_for(&MenuContext::default()));
    assert!(menus[1].visible_for(&own_direct));
    assert!(!menus[1].visible_for(&MenuContext::default()));
}

#[tokio::test(flavor = "multi_thread")]
async fn dialog_timeout_closes_through_the_plugin() {
    let server = TestServer::start(0).await.unwrap();
    let value = with_all_permissions();
    let fixture = Fixture::with_dialog_timeout(&value, Duration::from_millis(300)).await;
    let plugin = fixture.enable().await;
    let session = fixture.login(&server).await;
    let host = &fixture.host;
    let mut events = host.subscribe();

    plugin_call(
        &fixture,
        &plugin,
        &session,
        "dialog",
        json!({"kind": "confirm", "title": "没人回复"}),
    )
    .await;
    let shown = next_event(&mut events, "对话框", |e| {
        matches!(e, PluginEvent::ShowDialog { .. })
    })
    .await;
    let PluginEvent::ShowDialog { request_id, .. } = shown else {
        unreachable!();
    };

    // 没有人回复：界面收到关闭事件，待显示表清空，插件的调用以错误返回。
    let closed = next_event(&mut events, "对话框关闭", |e| {
        matches!(e, PluginEvent::DialogClosed { .. })
    })
    .await;
    assert!(
        matches!(closed, PluginEvent::DialogClosed { request_id: id } if id == request_id),
        "{closed:?}"
    );
    assert!(host.pending_dialogs().is_empty());
    wait_log(&plugin, "对话框结果：Err(Error(-7))").await;
    // 超时之后的回复没有去处。
    assert!(!host.reply_dialog(request_id, DialogResult::default()));
}
