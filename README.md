# YaZ

YaZ（Yet another Zulip）是一个即时通讯风格的 [Zulip](https://zulip.com/) 桌面客户端（Windows），基于 Rust 和 Dioxus 0.7 desktop（WebView2）。

- 界面以会话列表为中心：Zulip 的话题（Topic）显示为“群”，频道（Channel）显示为“群分组”。
- 会话置顶、免打扰、好友分组、备注、下载记录等保存在本地 SQLite。
- 支持原生 DLL 插件（`sdk/zulip_plugin.h`），内置模型中心、翻译、回复助手三个插件。

## 许可证

[MIT](LICENSE)。使用的第三方图标、字体和题库及其许可证见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)，发布包中也附带这两个文件。

## 构建

需要 Rust stable（Windows x64）和 WebView2 运行时。

```powershell
cargo build --workspace
cargo run -p yaz
```

提交前和 CI 一致的检查：

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

不连真实服务器时用假服务器（账号 `me@zulip.test`，任意密码），用法见 [`app/README.md`](app/README.md)：

```powershell
cargo run -p test-server -- 9991
```

编译内置插件（release）并复制到程序的 `plugins` 目录。`plugins/echo` 是示例，不随程序发布：

```powershell
./tools/build-plugins.ps1 -OutDir target/debug/plugins
```

## 发布

- 推送到 `main` 或提交 PR 时，CI（`.github/workflows/ci.yml`）在 Windows 上运行格式检查、Clippy 和测试；推送到 `main` 时还上传构建产物 `YaZ-win-x64`（`yaz.exe`、`yaz-updater.exe`、内置插件）。
- 推送 `v*` 形式的 tag（例如 `v0.2.0`，必须是语义版本）后，`.github/workflows/release.yml` 把工作区版本改成 tag 的版本，运行测试，用 `tools/pack-release.ps1` 打包，创建 Release，附带 `YaZ-v<版本>-win-x64.zip` 和自动更新清单 `YaZ-update.json`。tag 含 `-` 的是预发布版，不会推送给已安装的程序。
- 手动运行 release 工作流只生成构建产物，不创建 Release。

## 自动更新

从 Release 安装的程序启动后在后台检查最新的正式版，有新版本时静默下载并校验，在用户空闲一分钟后（或下次启动时）退出、替换、启动新版本。设置的“关于”页面有“确认版本”按钮可以手动检查。

- 替换由独立的 `yaz-updater.exe` 完成，只动 `yaz.files.sha256` 中列出的文件，程序目录中的其它文件不受影响。
- 新版本启动并确认正常后才删除旧文件；否则恢复旧文件并启动旧版本。
- 新版本启动失败后不再自动尝试该版本；下载的包校验失败、替换失败累计 3 次后同样不再尝试。手动“确认版本”会重试，发布更高的版本后自动恢复。
- 运行状态在 `%LocalAppData%\YaZ.Update`，数据目录是 `%LocalAppData%\YaZ`，更新日志在数据目录的 `logs\updater.log`。

## 数据目录

可以在设置中把数据搬到其它目录，位置记在 `%LocalAppData%\YaZ\location.json`。

- 配置的目录不可用（例如移动硬盘未连接）时，启动时先提示，不打开数据库：可以重试、本次使用默认目录或退出。使用默认目录不改配置，这次运行不能更改数据目录，下次启动再次检查。
- `location.json` 无法解析时改名为 `location.json.bad`（覆盖旧的），提示一次后使用默认目录。

## 开发者模式

开发者模式（设置来源标签、显示不可用的功能、请求记录）只在调试版中编译。Release 忽略保存的开发者选项；`tools/check-release-no-devmode.ps1` 检查 Release 的 exe 中没有相关文字。

## 目录

| 目录 | 内容 |
|---|---|
| `app/` | 主程序（bin `zulip`），界面开发规则见 [`app/README.md`](app/README.md) |
| `crates/zulip-core/` | 领域模型与纯逻辑（消息 HTML、Markdown、权限判断等），不做任何 IO |
| `crates/zulip-api/` | Zulip REST API 客户端 |
| `crates/zulip-store/` | 本地存储：SQLite 数据库、设置、数据目录、日志、API key 加密 |
| `crates/zulip-session/` | 账号会话：事件队列、状态、操作 |
| `crates/zulip-plugin-host/` | 插件宿主：加载 DLL、权限、事件分发、函数表 |
| `crates/zulip-plugin-sdk/` | 用 Rust 写插件的 SDK（ABI 定义和封装） |
| `crates/zulip-update/` | 自动更新的核心逻辑（版本比较、校验、替换与回滚） |
| `crates/yaz-updater/` | 独立更新器（bin `yaz-updater`） |
| `crates/test-server/` | 开发用的假 Zulip 服务器 |
| `plugins/` | 内置插件 `model-center`、`translator`、`reply-assistant`，示例 `echo` |
| `sdk/` | 插件 C 头文件 `zulip_plugin.h`，C 示例 `examples/echo.c` |
| `tools/` | `build-plugins.ps1` 编译插件、`pack-release.ps1` 打包发布、`check-release-no-devmode.ps1` 确认 Release 不含开发者模式、`gen_icons.ps1` 生成 `app/src/ui/icon_data.rs`、`seed-zulip.ps1` 向测试用 Zulip 服务器写种子数据 |

## 插件

插件是放在 `<程序目录>\plugins\<id>\` 中的原生 DLL 和 `plugin.json`。接口定义和完整约定（ABI 规则、`plugin.json` 字段、事件、函数表）在 [`sdk/zulip_plugin.h`](sdk/zulip_plugin.h)，用 Rust 写插件时用 [`crates/zulip-plugin-sdk`](crates/zulip-plugin-sdk)，示例见 `plugins/echo` 和 `sdk/examples/echo.c`。

插件只能调用 `plugin.json` 的 `permissions` 中声明过的权限对应的函数，开启时用户确认。每个权限对应哪些函数见头文件开头。

| 权限 | 含义 |
|---|---|
| `read_message` | 读取消息 |
| `read_contacts` | 读取联系人和频道 |
| `read_attachment` | 下载消息中的附件 |
| `send_message` | 发送消息 |
| `modify_message` | 改写发送的消息、回应和标记消息 |
| `upload_file` | 上传文件 |
| `annotate_message` | 在消息下方显示内容 |
| `render_message` | 改写气泡信息 |
| `fill_composer` | 填入输入框 |
| `edit_message` | 编辑消息 |
| `delete_message` | 删除消息 |
| `manage_channel` | 修改频道 |
| `manage_user` | 修改用户 |
| `launch` | 打开链接和程序 |
| `manage_topic` | 移动、改名和删除话题 |
| `open_conversation` | 切换到其他会话 |
| `notify` | 显示通知和状态栏提示 |
| `manage_subscription` | 订阅和退订频道、创建频道、修改话题关注和频道个人设置 |
| `manage_preferences` | 修改免打扰、置顶、静音用户、提醒词和个人状态 |
| `ui_dialog` | 弹出对话框 |
| `zulip_api` | 【高危】以你的账号直接调用 Zulip API：可以读取、修改和删除你有权访问的任何内容 |
| `network` | 访问网络 |
