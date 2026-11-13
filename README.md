# npp-acp

Notepad++ 的 **ACP（Agent Client Protocol）聊天插件**：在 Notepad++ 里打开一个右侧停靠面板，直接和 ACP agent 对话，让 agent 读写你正在编辑的代码。


<img width="1427" height="1002" alt="ScreenShot_2026-11-13_155958_336" src="https://github.com/user-attachments/assets/da075d24-1d9c-48bc-b8b2-6e9e54d2cf9b" />


## 环境要求

- Windows 10 / 11（x64）
- Notepad++ 8.0 及以上（64 位）
- WebView2 Runtime（Windows 11 自带；Windows 10 若缺失，请安装 Microsoft Edge WebView2 Runtime）
- 一个可用的 ACP agent 命令（默认 `claude-agent-acp`）

## 安装

1. 打开 **Releases** 页面，下载最新的 `npp-acp-vX.Y.Z.zip`。
2. 解压得到 `npp-acp` 文件夹（包含 `npp-acp.dll` 和 `ui\`）。
3. 把整个 `npp-acp` 文件夹放到 Notepad++ 的插件目录，最终结构为：

   ```
   C:\Program Files\Notepad++\plugins\npp-acp\npp-acp.dll
   C:\Program Files\Notepad++\plugins\npp-acp\ui\index.html
   ```

4. 重启 Notepad++。

> 如果拿到的是源码仓库，可直接双击 `scripts\install.cmd`（自动请求管理员权限并复制到 Notepad++ 插件目录）。
> 卸载：`powershell -ExecutionPolicy Bypass -File scripts\install.ps1 -Uninstall`。

## 使用

1. 先在 Notepad++ 打开一个文件（该文件所在目录会作为 agent 的工作目录，不要求 git 仓库）。
2. 菜单 **插件 → Agent ACP**，打开右侧面板。
3. 面板顶部选择 agent，输入内容回车发送，回复会流式显示。
4. 顶部按钮：
   - **新会话**：重新开一个 ACP 会话；
   - **设置**：编辑 agent 配置（见下）。
5. 输入框输入 `/` 会弹出 agent 提供的可用命令列表。
6. 面板底部会根据 agent 的能力显示 **模式 / 模型 / 思考强度** 等下拉（agent 支持时才有）。
7. 当 agent 请求权限（执行命令、修改文件等）时，会弹窗让你选择「允许」或「拒绝」。

面板主题会跟随 Notepad++ 的深色 / 浅色模式自动切换。

## 配置 agent

默认使用 `claude-agent-acp`。首次使用前需要全局安装（需要 Node.js 24+）：

```bash
npm i -g @anthropic-ai/claude-code @agentclientprotocol/claude-agent-acp
```

安装后 `claude-agent-acp.cmd` 会进入 PATH，插件默认通过 `cmd /c claude-agent-acp.cmd` 启动它。

可以点面板上方的 **设置** 直接编辑配置，也可以手动修改配置文件：

```
%APPDATA%\Notepad++\plugins\config\npp-acp\agents.json
```

```json
{
  "default_agent": "claude",
  "agent_servers": [
    {
      "id": "claude",
      "name": "Claude Agent",
      "command": "cmd",
      "args": ["/c", "claude-agent-acp.cmd"],
      "env": {}
    },
    {
      "id": "my-agent",
      "name": "我的 Agent",
      "command": "my-agent.exe",
      "args": ["--acp"],
      "env": { "API_KEY": "xxx" }
    }
  ]
}
```

- `default_agent`：默认选中的 agent id；`agent_servers` 可配多个，面板顶部下拉切换。
- `command` + `args`：启动 agent 的命令。Windows 的 `.cmd` / `.bat` 需要用 `cmd /c` 包一层。
- `env`：传给 agent 进程的环境变量。
- 保存后 **重启 Notepad++** 生效。
- 也可用环境变量 `NPP_ACP_CONFIG` 指向其它配置文件。

## 工作目录

- ACP 的工作目录（`cwd`）= 当前打开文件所在的目录，**不再要求 git 仓库**。
- 没有打开文件时提示「无工作目录」，打开文件后会自动重试。
- 面板顶部会显示当前 ACP 的工作目录（完整路径）。
- 文件能读 / 写到哪里，由 agent 自身的权限模式（如 readonly / plan / accept-edits / bypass）决定，插件只负责透传。

## 日志与排查

- 插件日志：`%LOCALAPPDATA%\npp-acp\plugin.log`
- agent 输出日志：`%LOCALAPPDATA%\npp-acp\agent.log`
- 调试细节：设置环境变量 `NPP_ACP_DEBUG=1` 后重启，可额外记录面板 resize 等信息。
- 面板白屏：确认已安装 WebView2 Runtime。
