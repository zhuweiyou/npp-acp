# AGENTS.md — 开发者说明

本文面向维护者，说明 npp-acp 的结构、构建、调试与发版流程。用户使用说明见 [README.md](README.md)。

## 项目简介

npp-acp 是 Notepad++ 的 ACP（Agent Client Protocol）聊天插件：一个可停靠的右侧面板，内嵌 WebView2 加载 React UI，Rust 侧作为 ACP client 启动 agent 子进程并驱动 `initialize → session/new → session/prompt`，把 agent 的流式输出转发到面板。

当前阶段：插件形态已可安装运行（`cdylib` + 停靠面板 + WebView2 + 真实 ACP agent），已支持权限弹窗、文件读写桥、agent 多配置、主题跟随。

## 目录结构

```
crates/npp-acp         Rust 插件（cdylib）与 dev harness
  src/lib.rs           插件 ABI + 停靠面板 + WebView2 + IPC + Scintilla 文件桥
  src/acp_client.rs    ACP client 服务：进程启动、会话、权限、fs 请求、事件
  src/main.rs          独立窗口 dev harness（bin: npp-acp-dev）
crates/mock-agent      测试用 ACP agent（流式返回）
crates/npp-acp-ui      前端：Vite+ / React / TS / antd / @ant-design/x
  src/App.tsx          聊天面板 UI（Bubble.List + Sender、权限弹窗、配置弹窗）
  src/ipc.ts           window.ipc.postMessage / window.__nppacp.emit 桥
scripts/build.ps1      一键构建：Rust release + UI build + 组装 dist
scripts/install.ps1    安装 / 卸载到 Notepad++
scripts/install.cmd    双击安装入口
.github/workflows/     CI 与 Release
dist/                  构建产物（被 .gitignore 忽略）
```

## 技术栈

- **Rust**：`wry`（WebView2）+ `tao`（窗口，仅 dev harness）+ `windows` crate（Win32 / Notepad++ ABI）+ `agent-client-protocol`（ACP SDK，v1）。
- **前端**：**Vite+**（`vite-plus` 包，`vp` CLI，集成 Vite + Rolldown + Vitest + Oxlint + Oxfmt），React 19 + TypeScript + antd 6 + `@ant-design/x`。

## 架构与数据流

1. `setInfo` 保存 Notepad++ / Scintilla 句柄，初始化 WebView2 用户数据目录与 STA COM。
2. 菜单触发 `cmd_open` → `create_panel`：创建子窗口 + `NPPM_DMMREGASDCKDLG` 注册为右侧停靠面板，`CreateWindowExW` + `wry::WebViewBuilder::build_as_child` 内嵌 WebView2。
3. UI 通过自定义协议 `http://nppacp.localhost/` 从 `ui\` 目录加载（`with_custom_protocol` 读盘返回）。
4. JS → Rust：`window.ipc.postMessage` → `handle_ipc`（`prompt` / `new_session` / `cancel` / `permission_response` / `list_agents` / `get_config` / `save_config` 等）。
5. Rust → JS：`evaluate_script("window.__nppacp.emit({...})")`，事件类型见 `acp_client::UiEvent`（`status` / `workspace` / `session` / `session_config` / `update` / `permission` / `prompt_done` / `error` / `theme`）。
6. 跨线程：ACP 在独立线程 + 自带 tokio runtime；UI 事件经 `UI_QUEUE` 队列 + `PostMessageW(WM_APP_UI_EVENT)` 回到 UI 线程再 `evaluate_script`。
7. 文件桥：`ScintillaBridge` 把「当前编辑缓冲区」映射到 Scintilla（`SCI_GETTEXT` / `SCI_SETTEXT`，能读写未保存内容），其它路径走磁盘。
8. 工作目录：`current_file_path` 取当前 buffer 路径（`NPPM_GETFULLCURRENTPATH`，回退 buffer id），取其所在目录作为 ACP `cwd`；**不再要求 git 仓库**。无文件时回退进程当前目录；文件打开/切换时若 ACP 未启动会重试。

## 本地构建

前置：Rust（`x86_64-pc-windows-msvc`）、Node 24+、pnpm 11、WebView2 Runtime。

```powershell
# 1) 安装前端依赖
pnpm -C crates/npp-acp-ui install

# 2) 一键构建（Rust release lib + UI build + 组装 dist）
powershell -ExecutionPolicy Bypass -File scripts\build.ps1
```

产物：

```
dist\npp-acp\npp-acp.dll      # 插件 DLL（crate-type cdylib）
dist\npp-acp\ui\...           # 前端静态资源
dist\npp-acp.zip              # 可直接分发的压缩包
```

`build.ps1` 内部：`cargo build -p npp-acp --lib --release` → `pnpm -C crates\npp-acp-ui exec vpr build`（输出到 `crates/npp-acp-ui/dist`）→ 复制 DLL 与 UI 到 `dist\npp-acp` → `Compress-Archive` 打 zip。

## 开发调试（dev harness）

`npp-acp-dev` 是独立窗口版本，从 Vite+ dev server（`http://localhost:5173`）加载 UI，默认启动同目录的 `mock-agent.exe`。

```powershell
# 终端 1：启动 dev server
pnpm -C crates/npp-acp-ui exec vp dev

# 终端 2：启动 Rust 窗口
cargo run -p npp-acp --bin npp-acp-dev
```

接真实 agent（例如 `claude-agent-acp`）：

```powershell
# 先全局安装默认 agent（需要 Node.js 24+）
npm i -g @anthropic-ai/claude-code @agentclientprotocol/claude-agent-acp

$env:NPP_ACP_AGENT='["cmd","/c","claude-agent-acp.cmd"]'
cargo run -p npp-acp --bin npp-acp-dev
```

## 环境变量

| 变量 | 作用 | 范围 |
| --- | --- | --- |
| `NPP_ACP_CONFIG` | 覆盖 `agents.json` 配置路径 | 插件 / dev |
| `NPP_ACP_DEBUG` | 记录面板 resize 等调试日志 | 插件 / dev |
| `NPP_ACP_UI_DIR` | 覆盖 UI 静态资源目录 | 插件 |
| `NPP_ACP_AUTOOPEN` | `NPPN_READY` 时自动打开面板 | 插件 |
| `NPP_ACP_AGENT` | agent 命令（JSON 数组或纯字符串），覆盖默认 | dev harness |
| `NPP_ACP_WORKSPACE` | 指定工作目录（不再要求 `.git`） | dev harness |
| `NPP_ACP_SELFTEST` | 页面加载后自动发一条 prompt，走真实发送路径 | dev harness |

## 前端工具链（Vite+）

- 格式化：`vp fmt`（Oxfmt，配置在 `crates/npp-acp-ui/vite.config.ts` 的 `fmt` 块：`semi: false`、`singleQuote: false`、`tabWidth: 2`、`printWidth: 120`）。
- 校验：`vp lint`（Oxlint）；`vp check` = 格式化 + lint + 类型检查。

```powershell
pnpm -C crates/npp-acp-ui fmt     # vp fmt . --write
pnpm -C crates/npp-acp-ui lint    # vp lint
pnpm -C crates/npp-acp-ui check   # vp check
```

## 配置与日志路径

- agent 配置：`%APPDATA%\Notepad++\plugins\config\npp-acp\agents.json`（结构见 README；`default_agent` + `agent_servers[]`，字段 `id` / `name` / `command` / `args` / `env`）。
- 插件日志：`%LOCALAPPDATA%\npp-acp\plugin.log`（超过 2MB 会删除重建）。
- agent 输出日志：`%LOCALAPPDATA%\npp-acp\agent.log`。
- WebView2 用户数据：`%LOCALAPPDATA%\npp-acp\webview2`（不写在 notepad++.exe 旁边）。

## CI / 发版

### CI（`.github/workflows/ci.yml`）

- 触发：push 到 `main`、对 `main` 的 PR、手动 `workflow_dispatch`。
- 运行环境：`windows-latest`（插件是 Windows x64 cdylib，必须在 Windows 构建）。
- 步骤：`cargo test --workspace --locked` → `scripts/build.ps1` 构建插件 → 上传 `dist/npp-acp.zip` 作为 artifact（保留 7 天）。

### Release（`.github/workflows/release.yml`）

- 触发：仅手动 `workflow_dispatch`，填表选择版本升级类型 `patch` / `minor` / `major`（参考 `../oc2api` 的自动累加方式）。
- 版本推算：取最近一个 `v*` tag 作为基线；无 tag 时取 `Cargo.toml` 的 `workspace.package.version` 作为基线。按选择的类型累加出下一个 `vX.Y.Z`，若 tag 已存在则报错退出。
- 流程：推算版本 → 生成 release notes（按 commit 前缀分组）→ `scripts/build.ps1` 构建插件 → 打 annotated tag 并 push → `gh release create` 创建 Release，并上传 `dist/npp-acp.zip`，资产名为 `npp-acp-vX.Y.Z.zip`。
- 版本号不写回源码；`Cargo.toml` 的版本仅作首个 tag 的基线。

> 注意：`scripts/build.ps1` 依赖已安装的前端依赖；CI/Release 在构建前会先 `pnpm install --frozen-lockfile`。

## 后续事项

- `agent-client-protocol` 版本升级（当前 5.2.0）后需回归权限 / fs / session config 相关 schema。
- Phase 4（真实 agent + 配置 + 权限 + 文件桥）主体已完成，后续为打磨项。
