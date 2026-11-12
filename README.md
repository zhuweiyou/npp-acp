# npp-acp

Notepad++ 的 ACP（Agent Client Protocol）聊天插件。计划分阶段推进：

- **Phase 1（当前）**：Rust ↔ WebUI 最小通讯（独立窗口 + WebView2 + ping/pong）
- Phase 2：接入 ACP（mock agent，流式）
- Phase 3：封装为 Notepad++ 插件（右侧 dock 面板）
- Phase 4：真实 agent + 配置 + 权限 + 文件桥

## 目录

```
crates/npp-acp       Rust：Phase 1 是独立窗口 dev harness（后续改 cdylib）
crates/npp-acp-ui    前端：Vite + React + TS + antd + @ant-design/x
```

## Phase 1 运行

前置：Node 20+、pnpm、Rust（x86_64-pc-windows-msvc）、WebView2 Runtime（Win11 自带）。

前端使用 **Vite+**（`vite-plus` 包，`vp` CLI：集成 Vite + Rolldown + Vitest + Oxlint + Oxfmt），不是裸 Vite。

```powershell
# 1) 安装前端依赖
pnpm -C crates/npp-acp-ui install

# 2) 启动 Vite+ dev server（保持运行）
pnpm -C crates/npp-acp-ui exec vp dev

# 3) 另开一个终端，启动 Rust 窗口（会加载 http://localhost:5173）
cargo run -p npp-acp --bin npp-acp-dev
```

> 也可以直接 `pnpm -C crates/npp-acp-ui dev`（脚本内部就是 `vp dev`）。

在窗口里输入文字发送，气泡会出现 `pong: <内容>`。浏览器直接打开 http://localhost:5173 也能看 UI（会走浏览器回显，不走 Rust）。

## Phase 1 验收（已跑通）

- Rust（tao + wry/WebView2）独立窗口成功加载 Vite+ dev server；
- 前端用 `@ant-design/x` 的 `Bubble.List` + `Sender` 渲染聊天界面；
- 前端 `window.ipc.postMessage({type:"ping"})` → Rust 控制台打印 `[ipc] ping <- "..."` → Rust 回 `window.__nppacp.emit({type:"pong"})` → 气泡显示 `pong: ...`；
- 顶部状态显示 `Rust 已连通 · transport: WebView2 IPC`。

## 主题（深/浅）说明

Notepad++ **8.0+ 自带深色模式**，并提供插件 API：`NPPM_ISDARKMODEENABLED`（查询）、`NPPN_DARKMODECHANGED`（变更通知）、`NPPM_GETDARKMODECOLORS`（取色）、`NPPM_DARKMODESUBCLASSANDTHEME`。因此插件做**自适应**：

- 阶段 1（独立窗口）：前端用 `prefers-color-scheme` 跟随系统；
- 阶段 3（插件内）：Rust 用 `NPPM_ISDARKMODEENABLED` 初始化、监听 `NPPN_DARKMODECHANGED`，通过 `{type:"theme",mode}` 事件推送，前端切换 antd `defaultAlgorithm` / `darkAlgorithm` 并更新 CSS 变量。

## Phase 2 验收（已跑通，真实 ACP 流式）

- Rust 侧用官方 `agent-client-protocol` SDK 启动 `mock-agent` 子进程，跑通 `initialize → session/new → session/prompt`；
- `mock-agent`（`crates/mock-agent`）按 ACP 协议流式发 `agent_message_chunk`，最后 `end_turn`；
- 通知经 `session/update` 转发到前端，`Bubble.List` 实时追加；`prompt_done` 后停止 loading；
- 日志可见：`[acp] update: {"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"..."}}`。

联调用 `NPP_ACP_SELFTEST=1` 可自动在页面加载后发一条 prompt（走真实 UI 发送路径）：

```powershell
$env:NPP_ACP_SELFTEST='1'; cargo run -p npp-acp --bin npp-acp-dev
```

### 接本机真实 agent（已验证 claude-agent-acp）

`NPP_ACP_AGENT` 支持 JSON 数组或纯命令字符串；Windows 的 `.cmd` 需要用 `cmd /c` 包一层：

```powershell
$env:NPP_ACP_AGENT='["cmd","/c","claude-agent-acp.cmd"]'
cargo run -p npp-acp --bin npp-acp-dev
```

已实测通过：`initialize` 返回 `Claude Agent 0.101.0`，`session/new` 成功，`session/prompt` 流式收到 `agent_thought_chunk` / `agent_message_chunk` / `usage_update`，最后 `end_turn`。

> 说明：`Bubble.List` 在 `@ant-design/x` v2 里左右布局的 prop 是 **`role`（单数）**，不是 `roles`。

## 工作区与权限策略

- **启动位置 = 当前文件所属 git 仓库根**：客户端向上查找 `.git`，把该目录作为 `session/new` 的 `cwd` 传给 agent；当前文件不在任何 git 仓库时禁用 agent。
- **文件能读/写到哪里，由 agent 自己的权限模式决定**（如 Claude 的 readonly / plan / accept-edits / bypass），客户端只负责透传 `fs/*` 请求，不做路径限制或沙箱。
- **权限弹窗**：agent 通过 ACP 发 `session/request_permission` 时，客户端把它转成 UI 弹窗让用户选择；若 agent 自身设为 auto/bypass，则不会发这类请求。

## Phase A（权限 / 文件桥 / UI）验收

- 权限：`session/request_permission` 存下 responder，前端弹窗选择允许/拒绝后回传并 `respond`；
- 文件桥：`fs/read_text_file` / `fs/write_text_file` 透传到 `DocumentBridge`（dev 用 `FsBridge` 直连磁盘，插件后续可换 Scintilla）；
- 工作区：`Workspace` 事件把仓库根推给 UI 展示，无仓库时 `no_workspace` 并禁用输入；
- UI：Markdown 渲染（`@ant-design/x-markdown`）、头像、左右气泡、工作区/会话状态、授权弹窗。

## 安装到 Notepad++（插件）

插件产物：`dist\npp-acp\`（`npp-acp.dll` + `ui\`），另打包 `dist\npp-acp.zip`。

一键构建：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\build.ps1
```

安装（写入 `C:\Program Files\Notepad++\plugins\npp-acp`，需要管理员，会自动请求提权）：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\install.ps1
# 卸载： scripts\install.ps1 -Uninstall
```

手动安装：把 `dist\npp-acp` 整个目录复制到 `<Notepad++>\plugins\npp-acp\`（即 `plugins\npp-acp\npp-acp.dll` + `plugins\npp-acp\ui\...`）。

使用：

1. 重启 Notepad++，菜单 **插件 → Agent ACP** 打开右侧面板；
2. 先打开一个**位于 git 仓库内**的文件（插件会取该仓库根作为 agent 的 `cwd`）；
3. 面板里选择 agent、输入内容对话。

默认 agent 命令为 `claude-agent-acp.cmd`（经 `cmd /c` 启动）；可用环境变量 `NPP_ACP_AGENT` 覆盖（JSON 数组或纯命令）。

已实机验证（portable Notepad++）：`setInfo → NPPN_READY → create_panel → webview created ok → workspace C:\work\npp-acp → status connected → session …`，说明插件能加载、面板/WebView2 正常、ACP 能连上真实 agent。

## 前端工具链（Vite+）

- 格式化：`vp fmt`（Oxfmt，Prettier 兼容）——配置写在 `crates/npp-acp-ui/vite.config.ts` 的 `fmt` 块：`semi: false`、`singleQuote: false`、`tabWidth: 2`；
- 校验：`vp lint`（Oxlint，默认 Recommended 规则集）。

```powershell
pnpm -C crates/npp-acp-ui fmt        # vp fmt . --write
pnpm -C crates/npp-acp-ui lint       # vp lint
pnpm -C crates/npp-acp-ui check      # vp check（格式化+lint+类型检查）
```
