# Superba

[English](README.md) · **简体中文**

Superba 是一个小巧的桌面启动器（附带无界面 CLI），用来把
[OpenAI Codex](https://github.com/openai/codex) —— ChatGPT / Codex 桌面 app、
`codex` 命令行，以及 app 连接的 SSH 远程主机 —— 指向
[Krill](https://www.krill-code.com) 中转站、任意兼容 OpenAI Responses API 的站点，
或者还原回你自己的 ChatGPT 官方登录。切换线路和认证方式时**不用关闭 app**，
会话列表保持可见，随时可以把一切原样还原。

支持 macOS、Linux 和 Windows，用 Rust 编写（egui 界面，Vercel Geist 风格）。

![概览](docs/screenshots/overview-dark-zh.png)

| 热切换前的倒计时 | 供应商：线路、认证方式、Key |
|---|---|
| ![倒计时](docs/screenshots/countdown-light-zh.png) | ![供应商](docs/screenshots/providers-light-zh.png) |
| **用量与花费（Krill）** | **命令面板（Cmd/Ctrl+K）** |
| ![用量](docs/screenshots/usage-dark-zh.png) | ![命令面板](docs/screenshots/palette-light-zh.png) |

> 截图由真实程序在隔离的主目录和本地模拟中转站上运行生成
> （`crates/superba-gui/tools/screenshots/shoot.py`），其中的账户、Key 和用量均为虚构数据。

## 功能

- **热切换。** app 运行中直接换线路（Krill：国内极速、海外线路、三网优化 CDN）
  或换认证方式：app 内部的 Codex 核心约 1 秒重启，窗口、正在看的对话和最近
  列表都保留，下一轮直接走新线路并带上完整历史。
- **自动选择认证方式。** Krill 的 Key 能用作个人访问令牌（PAT）时就用 PAT，否则
  用普通 API Key：线路边缘拦截了 PAT 端点、线路是明文 http、设置了自定义 base URL、
  通过本地路由，或者已安装的 Codex（app 自带的或 CLI）版本太旧不支持 PAT 时，都会
  改用 API Key，并始终说明原因。在“高级”里仍可固定为 PAT 或 API Key。
- **配置 = 连接 + Codex 设置。** 一个配置包括站点、按优先级排列的线路（第一条
  不可用时用下一条）、认证方式和 Key，以及 Superba 为它管理的 `config.toml` 设置。
  设置项来自按每个 Codex 版本自己的 schema 和功能列表生成的目录（英文说明取自官方，
  中文说明由人工撰写），会按读取该配置的各个 Codex 版本（app、CLI、每台远程主机）
  检查；每次切换都会预览每个改动的键及原因。
- **任意兼容 OpenAI 的站点。** 用 base URL 添加 Responses API 站点，把 Codex 的模型名
  映射到站点上的名称，切换前先检测（延迟、Key、模型列表、一次 Responses 请求）。
- **模型与速度档。** 模型页（以及 `spb models`）列出供应商的模型及其上下文窗口和
  自动压缩上限，可按模型覆盖（区分默认值与自定义值，带校验，可重置）；标准 / Fast /
  Ultrafast 速度档控件会说明每个档位对终端和桌面 app 各自的效果。
- **Codex 与 app 版本。** 更新页（以及 `spb codex`、`spb app`）显示 app、CLI 和每台
  远程主机运行的 Codex，安装经校验的 Codex 版本，可把桌面 app 固定在某个 Codex 版本，
  并检查 ChatGPT 桌面 app 是否有新版本（只显示安装命令，不会替你执行）。
- **账户与用量。** 任何用 Key 的站点都通过标准计费接口显示余额、套餐和到期时间；
  Krill 还会显示账户邮箱、每日费用和按模型的用量（按需获取：Krill 每小时只允许
  10 次用量查询）。
- **本地路由（可选，默认关闭）。** 一个只监听 `127.0.0.1` 的本用户后台进程，把本机
  Codex 的请求转发到 Krill 各线路和你的其他站点，按请求故障切换并保持会话粘性。开启
  后你的流量会经过这个本地进程。ChatGPT 官方登录从不经过路由，远程主机始终直连；
  Superba（或 `spb router watch`）运行期间路由进程挂掉时，会把 Codex 切回直连线路。
- **用量。** **用量**页面和 `spb usage` 显示最近请求（类似 Surge 的请求列表：会话名、
  供应商、token、缓存、费用）和统计，不开路由也可用：Superba 只读 Codex 写在会话记录里
  的用量（只有数字）。开启路由后，请求还会带首字、TPS、应答的线路和传输方式，并且只列一次。
- **默认安全。** 每次切换先出计划（看计划时不写任何文件），5 秒倒计时内按 Esc
  即可取消；执行过程写日志（journal），完成后校验（新核心进程、文件哈希、认证
  与线路检查、窗口状态），校验失败自动回滚。
- **等你的回合跑完。** 核心重启会让进行中的回合丢失回答，Superba 默认等回合
  结束（或询问你）再重启。
- **还原。** 回到*原始配置*（其他内容没变时逐字节一致）、回到*ChatGPT 官方登录*
  （Superba 第一次替换它时已加密暂存），或回到任意一次之前的切换。
- **会话 provider 迁移。** app 只显示 provider 与当前一致的会话。用过旧版 Krill
  教程（`model_provider = "krill"`）的用户可以把这些会话迁移到 `openai`（也可以
  迁回去），带备份、迁移账本和精确回滚。
- **远程主机。** app 已连接的 SSH 主机可以和本机一起切换（使用系统自带 `ssh`，
  Key 只通过 stdin 传输）。
- **通用模式。** 任何兼容 OpenAI Responses API 的站点都可以用 base URL 添加。
- **诊断。** 发现过期的 Codex 进程、外来的环境变量、无法解析的配置、钥匙串问题等。
- 中英文界面。

## 安装

发布页提供以下预编译包：

| 平台 | 文件 |
|---|---|
| macOS 11+（Apple 芯片和 Intel） | `superba-<版本>-macos-universal.dmg` / `.zip` —— 已签名并公证的 `Superba.app` 以及 `spb` |
| Linux x86_64 / aarch64（glibc） | `superba-<版本>-linux-<架构>.tar.gz`（`superba`、`spb`、`superba-router`） |
| Linux x86_64，仅 CLI（musl 静态） | `spb-<版本>-linux-x86_64-musl.tar.gz`（`spb`、`superba-router`） |
| Windows x86_64 | `superba-<版本>-windows-x86_64.zip`（`superba.exe`、`spb.exe`、`superba-router.exe`） |

`superba-router` 是可选的本地路由程序，请和 `superba`、`spb` 放在一起（macOS 上
它在 `Superba.app` 内部，开启路由前请先把 app 移到“应用程序”文件夹）。每个版本附带
`SHA256SUMS.txt`。从源码构建见[构建](#构建)。

## 快速开始（Krill）

1. 在 <https://www.krill-code.com> 获取 Key。
2. 打开 **Superba**。首次启动向导会检测桌面 app 和 CLI，展示当前配置（以及旧的
   `krill` 会话），填写 Key 并测速线路；认证方式自动选择（线路支持 PAT 时用 PAT，
   否则用 API Key，并说明原因）。
3. 确认后点 **切换**。如果桌面 app 正在运行但不是 Superba 启动的，Superba 会
   提出完整重启一次，之后就可以热切换了（见下文）。
4. 之后在概览页点另一条线路（或按 Cmd/Ctrl+K 输入线路名），倒计时结束后原地切换。

用 CLI 完成同样的事：

```sh
# Key 存进系统钥匙串（从 stdin 读取，不会出现在命令行参数里）
# 认证方式自动选择：线路支持时用 PAT，否则用 API Key（并说明原因）
spb profile add krill --line overseas --line cdn --key-stdin
spb lines --probe                 # 各线路延迟以及 PAT / API Key 是否可用
spb switch --profile krill        # 计划、倒计时、切换、校验
spb launch                        # 通过 Superba（重新）打开桌面 app
spb switch --line cdn             # 之后：热切换到另一条线路
spb account --usage               # Key 的余额、套餐、到期时间和近期用量
spb settings list                 # 配置管理的 Codex 设置（来自 Codex 自己的 schema）
```

其他兼容 OpenAI 的站点：

```sh
spb site add myapi --name "My API" --base-url https://api.example.com/v1 \
  --map gpt-5.5=vendor/gpt-5.5    # 可选：Codex 模型在该站点上的名称
spb profile add myapi --site myapi --key-stdin
spb site probe myapi --profile myapi --responses   # 延迟、Key、模型列表、一次 Responses 请求
spb switch --profile myapi
```

回到原来的样子：

```sh
spb restore original              # Superba 接管前的配置（最近一次接管）
spb restore official              # 你的 ChatGPT 登录（加密暂存）
spb restore generation --generation 3
spb uninstall                     # 还原并移除 Superba 写入的 shell 配置块
```

## 热切换的原理

Codex 只在 app-server 进程启动时读取 `config.toml`、`auth.json` 和环境变量，
所以任何切换都需要重启这个进程。桌面 app 内部有一条“重启 Codex 核心”的消息，
外部能发送它的唯一途径是 Chromium 调试协议（DevTools）。因此：

1. Superba 自己启动桌面 app，并只在本机回环上开调试端口（Linux 和 macOS 用
   `--remote-debugging-port=0`，再从 `DevToolsActivePort` 读出实际端口；Windows
   预留一个空闲端口），`--remote-allow-origins` 限定为这个地址，记录端口。
2. 切换时，Superba 原子地写入 `config.toml` 和 `auth.json` 中它负责的键（有
   journal），确认端口确实属于 app 进程后，请 app 的渲染进程重启 Codex 核心。
3. Superba 校验：新核心进程已启动、文件就是它写的内容、Key 在该线路可用、
   窗口没有掉回登录页。否则回滚并再次重启核心。

限制：

- 只有 Superba 启动的 app 才能热切换。否则第一次切换是**完整重启**（退出再打开），
  之后即可热切换。
- 有些变化必须完整重启：在 ChatGPT 登录和 Key 之间切换；PAT 模式下线路的认证地址
  与 app 启动时不同（app 只在启动时读取一次 `CODEX_AUTHAPI_BASE_URL`）；会话迁移
  （必须关闭所有 Codex 进程）。
- 核心重启时正在运行的回合会丢失回答。Superba 默认等待回合结束；`--force` 会打断。
- PAT 需要 `https` 线路，Codex 不接受明文 http 的 ChatGPT 后端。边缘节点拦截了
  PAT 端点的线路（目前是国内极速）会改用 API Key 模式，并说明原因。
- 终端里的 `codex` 会话和 IDE 插件在重启前继续使用旧配置，Superba 会把它们标为过期。
- 热切换已在 Linux（桌面 app 26.10）上验证。macOS 和 Windows 已实现、仍在验证中；
  无法确认重启通道时 Superba 会退回完整重启。
- 调试端口开启期间，以你的身份运行的其他程序，以及多用户机器上的其他本地用户，
  都可以连上它并控制 app。在设置中打开**安全模式**可以不带调试端口启动（不能热切换）。
  以 root 身份运行时从不开调试端口。详见 [SECURITY.md](SECURITY.md)。

## 本地路由（可选）

路由模式默认关闭，需要你主动开启（**路由**页面，或 `spb router enable`）。至少需要
一个成员：一个带自己 Key 的 Krill 或自定义站点配置。

- Superba 把 `superba-router` 安装为本用户服务（Linux：systemd 用户服务，不可用时用
  XDG 自启动；macOS：LaunchAgent；Windows：`HKCU\...\Run` 启动项），只监听
  `127.0.0.1`，并让本机 Codex 以 API Key 模式、用一个随机的本地令牌访问
  `http://127.0.0.1:<端口>/backend-api/codex`。路由校验这个令牌，再用各成员自己的
  Key 把请求发给成员（Krill 的 Key 绝不会发往其他站点）。
- 每个请求先走该会话当前的线路；连接失败、尚未返回任何内容就断开、502/503/504 或
  被边缘节点拦截时，换到该成员的下一条线路，再换下一个成员。401、403、429 及其他
  错误原样返回，从不切换。流式响应（SSE、WebSocket）和压缩请求体原样透传。
- 开启、关闭路由走正常的切换流程并带预览。已安装的路由在增删、调整成员后会重新加载；
  在“安装并启动”（或开启路由）之前，成员列表只会被保存。Codex 经过路由期间，只要
  Superba 窗口或 `spb router watch` 在运行，就会检查路由；连续 20 秒无响应时，Superba
  把 Codex 切回直连线路并关闭路由模式（切换失败会再次尝试）。
- 删除成员配置或为其更换 Key 时，会同步更新路由的 Key 文件。`spb router disable` 回到
  直连；`spb router uninstall`（或 `spb uninstall`）移除服务及其 Key 文件。

路由能看到什么，见 [SECURITY.md](SECURITY.md#the-local-router)。

## Superba 会改动什么

| 对象 | 何时 | 如何还原 |
|---|---|---|
| `~/.codex/config.toml`（或 `$CODEX_HOME`） | 每次切换 | 只改 Superba 负责的键（`model_provider`、`openai_base_url`、`chatgpt_base_url`、配置的模型以及它管理的 Codex 设置）；配置不再管理的键会恢复原值；注释、MCP 服务、插件和其他内容逐字节保留 |
| `~/.codex/auth.json` | 每次切换 | 以 0600 权限写入；替换前会加密暂存你的 ChatGPT 登录 |
| shell 启动文件 | 仅 PAT 模式 | 一个带标记的块，导出 `CODEX_AUTHAPI_BASE_URL`（Linux：`~/.profile`、`~/.bashrc`、`~/.zshenv`、fish `conf.d`、`~/.config/environment.d`；macOS：`~/.zshenv` 加 LaunchAgent；Windows：`HKCU\Environment`）。旧 Krill 教程写的行会被注释掉，之后可还原 |
| `state_5.sqlite`、`thread_history_*.sqlite`、`sessions/**/rollout-*.jsonl` | 仅在迁移会话时 | 备份加迁移账本；`spb sessions rollback <id>` |
| 远程主机的 `~/.codex` 和 shell 文件 | 仅限你选中的主机 | 在主机上按次备份；`spb remote rollback <目标>` |
| 路由服务（`superba-router.service`、`com.krill.superba.router` LaunchAgent、`SuperbaRouter` 启动项） | 仅在安装了本地路由时 | `spb router uninstall` / `spb uninstall` |
| Superba 数据目录 | 始终 | `~/.local/share/superba`（Linux）、`~/Library/Application Support/com.krill-code.Superba`（macOS）、`%APPDATA%\krill-code\Superba\data`（Windows）；可用 `SUPERBA_DATA_DIR` 覆盖 |

你交给 Superba 的 Key 存在系统钥匙串（Keychain、凭据管理器、Secret Service）中；
没有钥匙串时存在数据目录下权限为 0600 的文件里。登录和文件的备份用
ChaCha20-Poly1305 加密。

切换中途被打断（崩溃、断电）时，Superba 下次启动会把它完成或回滚
（也可以手动运行 `spb recover`）。

## CLI 参考

`spb` 是无界面 CLI；`superba <命令>` 的行为完全相同。所有命令都支持 `--json`
（stdout 恰好输出一个 JSON 文档）和 `--locale en|zh-CN`。

| 命令 | 用途 |
|---|---|
| `spb status` | 当前配置、桌面 app（安装、CDP、认证地址）、CLI 进程、远程主机 |
| `spb lines [--site ID] [--probe]` | 线路及其延迟 / PAT / API Key / 边缘拦截状态 |
| `spb key set --profile P (--stdin \| --key-env VAR)` / `spb key check` | 保存或检查 Key |
| `spb profile list \| add \| edit \| remove \| use` | 命名配置（站点、线路顺序、认证：自动 / PAT / API Key、Key 引用、模型） |
| `spb site list \| add \| edit \| remove \| probe` | 内置站点和兼容 OpenAI 的站点、模型映射、线路和 Key 检测 |
| `spb switch [--profile P] [--line L] [--auth pat\|apikey]` | 计划并执行切换（预览会列出每个改动的键及原因） |
| `spb settings list \| get \| set \| reset \| forget \| migrate \| diff` | 配置管理的 Codex 设置，按已安装的 Codex 版本检查 |
| `spb router status \| install \| enable \| disable \| uninstall \| watch` | 可选的本地路由：在线路和站点之间故障切换，只作用于本机 |
| `spb usage [log] [--provider P]` / `spb usage stats [--by model\|provider\|day\|session]` | 这台电脑上的 Codex 请求（路由的请求记录合并 Codex 会话记录，不开路由也可用）；按供应商（Krill、每个自定义站点）汇总，并列出其各条线路 |
| `spb account [--usage] [--days N] [--refresh]` | 余额、套餐和到期时间；每日费用和按模型的用量（Krill） |
| `spb restore [original\|official\|generation]` | 还原 |
| `spb launch [--restart]` | 通过 Superba 打开（或完整重启）桌面 app |
| `spb sessions [scan \| migrate --from krill \| rollback ID]` | 会话 provider |
| `spb remote list \| apply \| rollback` | SSH 远程主机 |
| `spb doctor`、`spb recover`、`spb uninstall` | 诊断、崩溃恢复、彻底移除 |

执行计划的命令常用参数：`--dry-run`（只显示计划）、`--countdown N`（0–30 秒，
Ctrl-C 取消）、`--yes`（确认完整重启）、`--wait-turn` / `--force`（处理进行中的
回合）、`--write-only`（只写文件，不重启任何东西）、`--remote 目标` / `--no-local`。

退出码：`0` 完成，`1` 出错，`2` 用法错误，`3` 被阻止，`4` 需要回答或确认，
`5` 已取消。

环境变量：`SUPERBA_DATA_DIR`（数据目录）、`SUPERBA_LOCALE`、`SUPERBA_APP_PATH`
（指定桌面 app 安装位置）、`SUPERBA_APP_ARGS`（额外的 app 启动参数）、
`SUPERBA_SECRET_BACKEND=keyring|file|auto`、`CODEX_HOME`。

## Krill

[Krill](https://www.krill-code.com) 是 Superba 的主要站点：它的线路、PAT / API Key
两种模式、线路探测以及 whoami / 账户检查都已内置。账户、Key、套餐和计费请访问
<https://www.krill-code.com>。Superba 同样支持任何兼容 OpenAI Responses API 的站点
和 ChatGPT 官方登录。

## 构建

需要 Rust 1.98 或更新版本和 C 编译器（SQLite、zstd 和 TLS 库从源码编译）。
Linux 上的 GUI 还需要常见的 X11 / Wayland 开发包：

```sh
sudo apt-get install -y libxkbcommon-dev libwayland-dev libxcb-render0-dev \
  libxcb-shape0-dev libxcb-xfixes0-dev libgl1-mesa-dev pkg-config
```

```sh
cargo build --release              # 生成 target/release/superba、spb 和 superba-router
cargo build --release -p superba-cli -p superba-router   # 无界面部分，不需要 GUI 库
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
python3 -I crates/superba-gui/tools/screenshots/shoot.py --bin-dir target/release
                                   # 重新生成 docs/screenshots（Xvfb + 模拟中转站）
```

crate 结构和切换流程见 [ARCHITECTURE.md](ARCHITECTURE.md)，提交修改前请阅读
[CONTRIBUTING.md](CONTRIBUTING.md)。发布打包和 macOS 签名见
[scripts/release/README.md](scripts/release/README.md)。

## 许可证

可任选 [Apache License 2.0](LICENSE-APACHE) 或 [MIT 许可证](LICENSE-MIT)。
内置的 Geist 字体使用 SIL Open Font License 1.1
（`crates/superba-gui/assets/fonts/LICENSE-Geist-OFL.txt`）。

Superba 是独立项目，与 OpenAI 无关联，也未获其认可。“ChatGPT”“Codex”“OpenAI”
是 OpenAI 的商标。
