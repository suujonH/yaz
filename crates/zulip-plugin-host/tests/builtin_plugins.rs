//! Loads the built-in plugin DLLs for real: translation and the reply assistant access the local
//! model API through the model hub (resource requests).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use tempfile::TempDir;
use test_server::{ME_EMAIL, ME_ID, TestServer, XIAOYU};
use tokio::runtime::Handle;
use zulip_plugin_host::{
    NativePlugin, PanelFieldValue, PanelRequest, PluginEvent, PluginHost, PluginHostConfig,
    PluginTarget, ResourceSelection, parse_fields,
};
use zulip_session::{Hub, PasswordLogin, Session};
use zulip_store::{AppPaths, Db};

const MODEL_CENTER: &str = "com.yaz.model-center";
const TRANSLATOR: &str = "com.yaz.translator";
const REPLY_ASSISTANT: &str = "com.yaz.reply-assistant";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

// A separate directory, so it does not contend for the build lock with the outer cargo test.
fn built_dir() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("plugin-build");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
        let status = Command::new(cargo)
            .args([
                "build",
                "-p",
                "plugin-model-center",
                "-p",
                "plugin-translator",
                "-p",
                "plugin-reply-assistant",
            ])
            .current_dir(workspace_root())
            .env("CARGO_TARGET_DIR", &target)
            .status()
            .unwrap();
        assert!(status.success());
        target.join("debug")
    })
}

fn install(plugins_dir: &Path, folder: &str) {
    let source = workspace_root().join("plugins").join(folder);
    let text = std::fs::read_to_string(source.join("plugin.json")).unwrap();
    let manifest: Value = serde_json::from_str(&text).unwrap();
    let target = plugins_dir.join(manifest["id"].as_str().unwrap());
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("plugin.json"), text).unwrap();
    let library = manifest["library"].as_str().unwrap();
    std::fs::copy(built_dir().join(library), target.join(library)).unwrap();
}

// A model API that replies by request path.
fn start_model_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || answer(stream));
        }
    });
    url
}

fn answer(mut stream: TcpStream) {
    let mut received = Vec::new();
    let mut buffer = [0u8; 8192];
    let request = loop {
        let Ok(n) = stream.read(&mut buffer) else {
            return;
        };
        received.extend_from_slice(&buffer[..n]);
        let text = String::from_utf8_lossy(&received).into_owned();
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if body.len() >= length {
                break text;
            }
        }
        if n == 0 {
            return;
        }
    };
    let body = if request.starts_with("POST /decide") {
        // Conflict question level 1; behavior questions have no answer.
        json!({"answers": {"friction.other": {"score": 1}}, "model": "判断"})
    } else if request.contains("分析模型") || request.contains("需要分析的消息") {
        let content = json!({"intent": "打招呼", "replies": ["你好", "在的"]}).to_string();
        json!({"choices": [{"message": {"content": content}}]})
    } else {
        json!({"choices": [{"message": {"content": "译文：hello"}}]})
    }
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "等待超时：{what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

struct Fixture {
    _dir: TempDir,
    host: Arc<PluginHost>,
    hub: Arc<Hub>,
}

impl Fixture {
    async fn new() -> Self {
        // The DLL is never unloaded, so directories that cannot be deleted stay under target.
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let plugins_dir = dir.path().join("plugins");
        for folder in ["model-center", "translator", "reply-assistant"] {
            install(&plugins_dir, folder);
        }
        let paths = AppPaths::new(dir.path().join("data"));
        std::fs::create_dir_all(paths.root()).unwrap();
        let db = Db::open(&paths.database()).unwrap();
        let host = PluginHost::new(PluginHostConfig {
            plugins_dir,
            db: db.clone(),
            paths: paths.clone(),
            runtime: Handle::current(),
            dialog_timeout: zulip_plugin_host::DIALOG_TIMEOUT,
        });
        let hub = Hub::new(db, paths, None);
        host.attach(&hub);
        host.load_all().await.unwrap();
        Self {
            _dir: dir,
            host,
            hub,
        }
    }

    async fn enable(&self, id: &str) -> Arc<NativePlugin> {
        let plugin = self.host.plugin(id).unwrap();
        self.host.toggle(&plugin, true).await;
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

    fn configure(&self, plugin: &Arc<NativePlugin>, values: &[(&str, Value)]) {
        let mut settings: Map<String, Value> = self.host.load_settings(plugin);
        for (key, value) in values {
            settings.insert((*key).to_owned(), value.clone());
        }
        self.host.save_settings(plugin, &settings).unwrap();
    }

    // Enable the model hub and add the chat and judge providers.
    async fn model_center(&self, url: &str) -> Arc<NativePlugin> {
        let center = self.enable(MODEL_CENTER).await;
        self.configure(
            &center,
            &[(
                "providers",
                json!([
                    {"name": "本地", "type": "chat", "endpoint": url, "api_key": "k", "models": ["m"]},
                    {"name": "判断", "type": "decisions", "endpoint": format!("{url}/decide"), "models": ["d"]},
                    {"name": "没有模型", "type": "chat", "endpoint": url},
                ]),
            )],
        );
        let host = &self.host;
        wait_until("发布资源", || {
            !host
                .available_resources(MODEL_CENTER, "decision_model")
                .is_empty()
        })
        .await;
        center
    }
}

fn chat_selection() -> Value {
    json!({"type": "model", "plugin": MODEL_CENTER, "resource": "本地", "item": "m"})
}

async fn message_of(session: &Session, id: i64) -> zulip_core::model::ChatMessage {
    let dto = session.client().get_message(id, true).await.unwrap();
    zulip_session::convert::message_from_dto(&dto, session.self_id())
}

#[tokio::test(flavor = "multi_thread")]
async fn model_center_publishes_only_providers_with_models() {
    let fixture = Fixture::new().await;
    let center = fixture.model_center(&start_model_server()).await;
    let chat = fixture.host.available_resources(MODEL_CENTER, "model");
    assert_eq!(chat.len(), 1);
    assert_eq!(
        (chat[0].id.as_str(), chat[0].items[0].as_str()),
        ("本地", "m")
    );
    assert!(
        center
            .logs()
            .iter()
            .any(|l| l.text.contains("已提供 1 个 provider 的 model"))
    );
    let selection = ResourceSelection::from_json(&chat_selection()).unwrap();
    let translator = fixture.host.plugin(TRANSLATOR).unwrap();
    let field = translator
        .manifest()
        .unwrap()
        .settings
        .iter()
        .find(|f| f.key == "model")
        .unwrap()
        .clone();
    let options = fixture.host.resource_options(&field, Some(&selection));
    assert!(
        options
            .iter()
            .any(|o| o.selection.as_ref() == Some(&selection) && o.available)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn translator_replaces_bubble_through_model_center() {
    let url = start_model_server();
    let server = TestServer::start(0).await.unwrap();
    let fixture = Fixture::new().await;
    fixture.model_center(&url).await;
    let translator = fixture.enable(TRANSLATOR).await;
    let session = fixture.login(&server).await;
    let host = &fixture.host;
    let account = session.account_id();
    let id = server.send_as(XIAOYU, test_server::Target::Dm(vec![ME_ID]), "你好");
    let message = message_of(&session, id).await;

    // No model selected.
    host.invoke_message_menu(&translator, 1, &message, &session);
    wait_until("提示选择模型", || {
        host.message_annotations(account, id)
            .cards
            .first()
            .is_some_and(|c| c.text.contains("请先在"))
    })
    .await;

    fixture.configure(&translator, &[("model", chat_selection())]);
    host.invoke_message_menu(&translator, 1, &message, &session);
    wait_until("显示译文", || {
        host.message_annotations(account, id)
            .rendered
            .is_some_and(|r| r.html.contains("译文：hello"))
    })
    .await;

    // The translation is saved in the plugin's database and re-attached when the message is shown
    // again.
    let mut events = host.subscribe();
    host.notify_messages_shown(&session, &[id]);
    let reattached = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let PluginEvent::AnnotationChanged { message_id, .. } = events.recv().await.unwrap()
                && message_id == id
            {
                break;
            }
        }
    })
    .await;
    assert!(reattached.is_ok(), "没有重新附加");
}

#[tokio::test(flavor = "multi_thread")]
async fn reply_assistant_shows_analysis_and_candidates() {
    let url = start_model_server();
    let server = TestServer::start(0).await.unwrap();
    let fixture = Fixture::new().await;
    fixture.model_center(&url).await;
    let assistant = fixture.enable(REPLY_ASSISTANT).await;
    let session = fixture.login(&server).await;
    let host = &fixture.host;
    let account = session.account_id();

    // The user profile card sets an identity.
    let target = PluginTarget::User {
        account_id: account,
        user_id: XIAOYU,
        name: "小雨".to_owned(),
    };
    let reply = host
        .panel(
            &assistant,
            PanelRequest::Set {
                target: target.clone(),
                key: "identity".to_owned(),
                value: json!("work/peer"),
            },
        )
        .await
        .unwrap()
        .unwrap();
    let PanelFieldValue::Select { selected, .. } = &parse_fields(&reply)[0].value else {
        panic!("应为下拉框");
    };
    assert_eq!(selected, "work/peer");

    fixture.configure(
        &assistant,
        &[
            ("model", chat_selection()),
            (
                "analysis_model",
                json!({"type": "decision_model", "plugin": MODEL_CENTER, "resource": "判断", "item": "d"}),
            ),
        ],
    );
    let id = server.send_as(
        XIAOYU,
        test_server::Target::Dm(vec![ME_ID]),
        "周五前能给我吗",
    );
    let message = message_of(&session, id).await;
    host.invoke_message_menu(&assistant, 1, &message, &session);
    wait_until("分析结果", || {
        host.message_annotations(account, id)
            .cards
            .first()
            .is_some_and(|c| !c.replies.is_empty())
    })
    .await;
    let annotations = host.message_annotations(account, id);
    let card = &annotations.cards[0];
    assert_eq!(card.title, "对方意图：打招呼");
    // jev's conflict level 1, no risk from the reply model: low danger; the scene is on hover.
    assert_eq!(card.tags[0].text, "危险 低");
    assert!(
        card.tags[0].tip.contains("分析场景：工作 / 同事"),
        "{}",
        card.tags[0].tip
    );
    // jev identified nothing and the model gave no confidence or preparation.
    assert!(card.fields.is_empty(), "{:?}", card.fields);
    assert_eq!(card.replies.len(), 2);
    assert_eq!(card.replies[1].action.fill, "在的");
    assert_eq!(card.tools[0].icon, "refresh");
    assert!(card.close.is_some());
}
