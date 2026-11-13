import { useEffect, useMemo, useRef, useState } from "react"
import { Sender } from "@ant-design/x"
import MarkdownIt from "markdown-it"
import hljs from "highlight.js"
import githubLightCss from "github-markdown-css/github-markdown-light.css?url"
import githubDarkCss from "github-markdown-css/github-markdown-dark.css?url"
import hljsLightCss from "highlight.js/styles/github.css?url"
import hljsDarkCss from "highlight.js/styles/github-dark.css?url"
import { Badge, Button, ConfigProvider, Flex, Input, Modal, Select, Typography, theme as antdTheme } from "antd"
import { onEvent, send, type IpcMessage } from "./ipc"

type ChatRole = "user" | "assistant" | "system"
type ThemeMode = "light" | "dark"

interface ToolInfo {
  name: string
  title: string
  status: string
  detail: string
}

interface ChatMessage {
  key: string
  role: ChatRole
  content: string
  streaming?: boolean
  tool?: ToolInfo & { seq: number }
}

interface AgentInfo {
  id: string
  name: string
}

interface PermissionOptionView {
  optionId: string
  name: string
  kind: string
}

interface PermissionRequest {
  id: string
  tool: string
  options: PermissionOptionView[]
}

let seq = 0
const nextKey = () => `m-${Date.now()}-${seq++}`

const md = new MarkdownIt({
  html: false,
  linkify: true,
  breaks: true,
  highlight(str: string, lang: string) {
    if (lang && hljs.getLanguage(lang)) {
      try {
        return `<pre class="hljs"><code>${hljs.highlight(str, { language: lang, ignoreIllegals: true }).value}</code></pre>`
      } catch {
        // fall through
      }
    }
    return `<pre class="hljs"><code>${md.utils.escapeHtml(str)}</code></pre>`
  },
})

function MarkdownView({ content }: { content: string }) {
  const html = useMemo(() => md.render(content), [content])
  return <div className="markdown-body" dangerouslySetInnerHTML={{ __html: html }} />
}

function toolInfo(update: Record<string, unknown>): ToolInfo {
  const u = update as Record<string, any>
  const rawInput = u.rawInput ?? u.raw_input
  const raw = rawInput && typeof rawInput === "object" ? (rawInput as Record<string, any>) : undefined
  const kindLabels: Record<string, string> = {
    read: "Read",
    edit: "Edit",
    delete: "Delete",
    move: "Move",
    search: "Search",
    execute: "Execute",
    think: "Think",
    fetch: "Fetch",
  }
  const title = String(u.title ?? "").trim()
  const kindLabel = kindLabels[String(u.kind ?? "").toLowerCase()] ?? ""

  let name =
    [u.name, u.toolName, u.tool, raw?.tool, raw?.toolName, raw?.name]
      .map((v) => String(v ?? "").trim())
      .find(Boolean) ?? ""
  if (!name && title && title !== "tool") name = title
  if (!name && kindLabel) name = kindLabel
  if (!name && raw) {
    const cmd = raw.command ?? raw.cmd
    if (typeof cmd === "string") name = cmd.trim().split(/\s+/)[0]
    else if (Array.isArray(cmd) && cmd.length) name = String(cmd[0])
    else if (typeof raw.path === "string") name = raw.path.split(/[\\/]/).filter(Boolean).pop() || "Read"
    else if (typeof raw.pattern === "string") name = "Search"
    else if (typeof raw.query === "string") name = "Search"
  }
  if (!name && Array.isArray(u.locations) && u.locations.length) {
    const p = String(u.locations[0]?.path ?? "")
    name = p.split(/[\\/]/).filter(Boolean).pop() || ""
  }
  if (!name) name = "Tool"

  const status = String(u.status ?? "")
  const stripFence = (t: string) =>
    t
      .replace(/^\s*```[a-zA-Z0-9_-]*\s*\n?/, "")
      .replace(/\n?\s*```\s*$/, "")
      .trim()
  const fmt = (v: any) => (typeof v === "string" ? stripFence(v) : JSON.stringify(v, null, 2))
  const parts: string[] = []
  if (rawInput !== undefined) parts.push(fmt(rawInput))
  const content = u.content
  if (Array.isArray(content)) {
    const texts = content
      .map((c: any) => (typeof c?.content?.text === "string" ? c.content.text : typeof c?.text === "string" ? c.text : ""))
      .filter(Boolean)
      .map(stripFence)
    if (texts.length) parts.push(texts.join("\n"))
  } else if (typeof content === "string") {
    parts.push(stripFence(content))
  }
  const out = u.rawOutput ?? u.raw_output
  if (out !== undefined) parts.push(fmt(out))
  return { name, title: title && title !== name ? title : "", status, detail: parts.filter(Boolean).join("\n\n") }
}

function initialTheme(): ThemeMode {
  if (typeof window !== "undefined" && window.matchMedia) {
    return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light"
  }
  return "light"
}

interface SelectOptionView {
  label: string
  value: string
}

// Session config options may be a flat array or grouped; normalize to a flat list.
function flattenOptions(options: unknown): SelectOptionView[] {
  if (!Array.isArray(options)) return []
  const out: SelectOptionView[] = []
  for (const item of options) {
    if (item && typeof item === "object") {
      const obj = item as Record<string, unknown>
      if (Array.isArray(obj.options)) {
        out.push(...flattenOptions(obj.options))
      } else if (obj.value !== undefined) {
        out.push({ label: String(obj.name ?? obj.value), value: String(obj.value) })
      }
    }
  }
  return out
}

export default function App() {
  const [mode, setMode] = useState<ThemeMode>(initialTheme)
  const [messages, setMessages] = useState<ChatMessage[]>([
    {
      key: nextKey(),
      role: "system",
      content: "已接入 ACP。发送内容后，回复会流式显示。",
    },
  ])
  const [agents, setAgents] = useState<AgentInfo[]>([])
  const [agentId, setAgentId] = useState<string>()
  const [status, setStatus] = useState("connecting")
  const [statusDetail, setStatusDetail] = useState("")
  const [sessionId, setSessionId] = useState<string>()
  const [workspace, setWorkspace] = useState<string>()
  const [busy, setBusy] = useState(false)
  const [input, setInput] = useState("")
  const [permission, setPermission] = useState<PermissionRequest | null>(null)
  const [commands, setCommands] = useState<{ name: string; description?: string }[]>([])
  const [modes, setModes] = useState<{ id: string; name: string; description?: string }[]>([])
  const [currentMode, setCurrentMode] = useState("")
  const [configOptions, setConfigOptions] = useState<Record<string, unknown>[]>([])
  const [configOpen, setConfigOpen] = useState(false)
  const [configText, setConfigText] = useState("")
  const [configPath, setConfigPath] = useState("")
  const streamingKey = useRef<string | null>(null)
  const pendingText = useRef("")
  const flushTimer = useRef<number | null>(null)
  // 思考块：连续 thought chunk 追加到同一条，遇到消息/工具则另起一块。
  const thoughtKey = useRef<string | null>(null)
  // 工具调用：按 toolCallId 映射到各自的消息；没有 id 时逐条追加，序号递增。
  const toolKeys = useRef<Map<string, string>>(new Map())
  const toolSeq = useRef(0)
  const bodyRef = useRef<HTMLDivElement | null>(null)

  const pushSystem = (content: string) => setMessages((prev) => [...prev, { key: nextKey(), role: "system", content }])

  const appendAssistant = (text: string) => {
    if (!text) return
    setMessages((prev) => {
      const key = streamingKey.current
      if (key) {
        return prev.map((m) => (m.key === key ? { ...m, content: m.content + text } : m))
      }
      const newKey = nextKey()
      streamingKey.current = newKey
      return [...prev, { key: newKey, role: "assistant", content: text, streaming: true }]
    })
  }

  const flushPending = () => {
    if (flushTimer.current !== null) {
      clearTimeout(flushTimer.current)
      flushTimer.current = null
    }
    const s = pendingText.current
    pendingText.current = ""
    if (s) appendAssistant(s)
  }

  // Batch token chunks so the narrow panel doesn't re-render on every token.
  const queueText = (text: string) => {
    if (!text) return
    pendingText.current += text
    if (flushTimer.current === null) {
      flushTimer.current = window.setTimeout(() => {
        flushTimer.current = null
        const s = pendingText.current
        pendingText.current = ""
        if (s) appendAssistant(s)
      }, 40)
    }
  }

  const handleUpdate = (update: Record<string, unknown>) => {
    const kind = update.sessionUpdate
    if (kind === "agent_message_chunk") {
      thoughtKey.current = null
      const content = update.content as { text?: string } | undefined
      queueText(content?.text ?? "")
    } else if (kind === "agent_thought_chunk") {
      const content = update.content as { text?: string } | undefined
      const text = content?.text ?? ""
      if (text) {
        setMessages((prev) => {
          const key = thoughtKey.current
          if (key) {
            return prev.map((m) => (m.key === key ? { ...m, content: m.content + text } : m))
          }
          const k = nextKey()
          thoughtKey.current = k
          return [...prev, { key: k, role: "system", content: `🧠 ${text}` }]
        })
      }
    } else if (kind === "tool_call" || kind === "tool_call_update") {
      thoughtKey.current = null
      const u = update as Record<string, unknown>
      const rawId = u.toolCallId ?? u.tool_call_id
      const id = rawId ? String(rawId) : ""
      const info = toolInfo(update)
      setMessages((prev) => {
        const key = id ? toolKeys.current.get(id) : undefined
        if (key) {
          return prev.map((m) =>
            m.key === key ? { ...m, tool: { ...info, seq: m.tool?.seq ?? 0 } } : m,
          )
        }
        const k = nextKey()
        const seq = ++toolSeq.current
        if (id) toolKeys.current.set(id, k)
        return [...prev, { key: k, role: "system", content: "", tool: { ...info, seq } }]
      })
    } else if (kind === "available_commands_update") {
      setCommands((update.availableCommands as { name: string; description?: string }[]) ?? [])
    } else if (kind === "current_mode_update") {
      setCurrentMode(String(update.currentModeId ?? ""))
    } else if (kind === "config_option_update") {
      setConfigOptions((update.configOptions as Record<string, unknown>[]) ?? [])
    }
  }

  const handleSend = (text: string) => {
    const value = text.trim()
    if (!value) return
    if (status === "no_workspace") return
    streamingKey.current = null
    pendingText.current = ""
    thoughtKey.current = null
    toolKeys.current.clear()
    toolSeq.current = 0
    setMessages((prev) => [...prev, { key: nextKey(), role: "user", content: value }])
    setBusy(true)
    if (!send({ type: "prompt", text: value })) {
      setBusy(false)
      appendAssistant(`（浏览器回显，未连接 Rust）收到：${value}`)
    }
  }

  const handleNewSession = () => {
    send({ type: "new_session" })
    streamingKey.current = null
    thoughtKey.current = null
    toolKeys.current.clear()
    toolSeq.current = 0
    setBusy(false)
    pushSystem("已请求新建会话")
  }

  const respondPermission = (optionId: string | null) => {
    if (!permission) return
    send({ type: "permission_response", id: permission.id, optionId })
    setPermission(null)
  }

  useEffect(() => {
    document.documentElement.dataset.theme = mode
  }, [mode])

  // GitHub Markdown / 高亮主题跟随深浅色切换（浅色用 light，深色用 dark）。
  useEffect(() => {
    const setLink = (id: string, href: string) => {
      let el = document.getElementById(id) as HTMLLinkElement | null
      if (!el) {
        el = document.createElement("link")
        el.id = id
        el.rel = "stylesheet"
        const first = document.head.querySelector('link[rel="stylesheet"], style')
        if (first && first.parentNode) first.parentNode.insertBefore(el, first)
        else document.head.appendChild(el)
      }
      if (el.getAttribute("href") !== href) el.href = href
    }
    setLink("github-markdown-theme", mode === "dark" ? githubDarkCss : githubLightCss)
    setLink("hljs-theme", mode === "dark" ? hljsDarkCss : hljsLightCss)
  }, [mode])

  useEffect(() => {
    if (!window.matchMedia) return
    const mq = window.matchMedia("(prefers-color-scheme: dark)")
    const handler = (e: MediaQueryListEvent) => setMode(e.matches ? "dark" : "light")
    mq.addEventListener("change", handler)
    return () => mq.removeEventListener("change", handler)
  }, [])

  useEffect(() => {
    const el = bodyRef.current
    if (el) el.scrollTop = el.scrollHeight
  }, [messages])

  useEffect(() => {
    const off = onEvent((event: IpcMessage) => {
      const type = event.type
      if (type === "agents") {
        const list = (event.agents as AgentInfo[]) ?? []
        setAgents(list)
        setAgentId((prev) => prev ?? list[0]?.id)
      } else if (type === "workspace") {
        setWorkspace(String(event.root ?? ""))
      } else if (type === "status") {
        setStatus(String(event.state ?? ""))
        setStatusDetail(String(event.detail ?? ""))
      } else if (type === "session") {
        setSessionId(String(event.session_id ?? ""))
      } else if (type === "session_config") {
        const modeState = event.modes as {
          currentModeId?: string
          availableModes?: { id: string; name: string }[]
        } | null
        setModes(modeState?.availableModes ?? [])
        setCurrentMode(String(modeState?.currentModeId ?? ""))
        setConfigOptions((event.config as Record<string, unknown>[]) ?? [])
      } else if (type === "config") {
        setConfigPath(String(event.path ?? ""))
        setConfigText(String(event.json ?? ""))
        setConfigOpen(true)
      } else if (type === "config_saved") {
        setConfigOpen(false)
        pushSystem("配置已保存，重启 Notepad++ 后生效")
      } else if (type === "update") {
        handleUpdate((event.update as Record<string, unknown>) ?? {})
      } else if (type === "permission") {
        setPermission({
          id: String(event.id ?? ""),
          tool: String(event.tool ?? "tool"),
          options: (event.options as PermissionOptionView[]) ?? [],
        })
      } else if (type === "prompt_done") {
        flushPending()
        setBusy(false)
        setMessages((prev) => prev.map((m) => (m.key === streamingKey.current ? { ...m, streaming: false } : m)))
        streamingKey.current = null
        thoughtKey.current = null
        toolKeys.current.clear()
        toolSeq.current = 0
      } else if (type === "error") {
        setBusy(false)
        setStatus("error")
        pushSystem(`⚠️ ${event.message}`)
      } else if (type === "theme") {
        setMode(event.mode === "dark" ? "dark" : "light")
      } else if (type === "editor_font") {
        const font = String(event.font ?? "").trim()
        const size = Number(event.size ?? 0)
        const root = document.documentElement
        if (font)
          root.style.setProperty(
            "--editor-font",
            `"${font}", "SimSun", "宋体", "Microsoft YaHei", ui-monospace, Consolas, monospace`,
          )
        if (size > 0) root.style.setProperty("--editor-font-size", `${size}px`)
      }
    })
    send({ type: "list_agents" })
    return off
  }, [])

  // Dev-only: `?selftest=1` auto-sends a prompt after load.
  const selftestFired = useRef(false)
  useEffect(() => {
    if (selftestFired.current) return
    const selftest = new URLSearchParams(window.location.search).get("selftest")
    if (!selftest) return
    selftestFired.current = true
    const timer = setTimeout(() => handleSend("用一句话介绍你自己"), 2000)
    return () => clearTimeout(timer)
  }, [])

  const statusBadge =
    status === "connected" ? (
      <Badge status="success" text="已连接" />
    ) : status === "no_workspace" ? (
      <Badge status="default" text="无工作目录" />
    ) : status === "error" ? (
      <Badge status="error" text="出错" />
    ) : (
      <Badge status="processing" text="连接中…" />
    )

  return (
    <ConfigProvider
      theme={{
        algorithm: mode === "dark" ? antdTheme.darkAlgorithm : antdTheme.defaultAlgorithm,
        token: { colorPrimary: "#4f8cff", borderRadius: 10 },
      }}
    >
      <Flex vertical className="app-root">
        <div className="app-header">
          <div className="app-header-row">
            <Select
              size="small"
              className="app-agent-select"
              value={agentId}
              placeholder="选择 agent"
              options={agents.map((a) => ({ label: a.name, value: a.id }))}
              onChange={setAgentId}
            />
            <Button size="small" className="app-btn" onClick={handleNewSession} disabled={status === "no_workspace"}>
              新会话
            </Button>
            <Button size="small" className="app-btn" onClick={() => send({ type: "get_config" })}>
              设置
            </Button>
            <span className="app-spacer" />
            {statusBadge}
          </div>
          <div className="app-workspace" title={workspace || statusDetail}>
            <span className="app-workspace-label">工作目录</span>
            <span className="app-workspace-path">{workspace || "无工作目录"}</span>
            {sessionId ? <span className="app-session">{sessionId.slice(0, 8)}</span> : null}
          </div>
        </div>

        {status === "no_workspace" && (
          <div className="app-notice">无法确定 ACP 工作目录（当前没有打开文件）。打开一个文件后会自动重试。</div>
        )}

        <div className="app-body" ref={bodyRef}>
          <div className="msg-list">
            {messages.map((m) => (
              <div key={m.key} className={`msg-row ${m.role}`}>
                {m.tool ? (
                  <details className="tool-block">
                    <summary className="tool-summary">
                      <span className="tool-seq">#{m.tool.seq}</span>
                      <span className="tool-name" title={m.tool.title || m.tool.name}>
                        {m.tool.name || m.tool.title || "tool"}
                      </span>
                      {m.tool.status ? (
                        <span className={`tool-status ${m.tool.status}`}>{m.tool.status}</span>
                      ) : null}
                    </summary>
                    {m.tool.detail ? <pre className="tool-detail">{m.tool.detail}</pre> : null}
                  </details>
                ) : m.role === "user" ? (
                  <div className="msg msg-user">{m.content}</div>
                ) : m.role === "assistant" ? (
                  <div className="assistant-plain markdown-body">
                    <MarkdownView content={m.content || (m.streaming ? "…" : "")} />
                  </div>
                ) : (
                  <div className="event-line">{m.content}</div>
                )}
              </div>
            ))}
          </div>
        </div>

        <div className="app-footer">
          {input.startsWith("/") && commands.length > 0 && (
            <div className="slash-menu">
              {commands
                .filter((c) => c.name.toLowerCase().includes(input.slice(1).split(/\s+/)[0].toLowerCase()))
                .slice(0, 40)
                .map((c) => (
                  <div key={c.name} className="slash-item" onClick={() => setInput(`/${c.name} `)}>
                    <b>/{c.name}</b>
                    {c.description ? <span>{c.description}</span> : null}
                  </div>
                ))}
            </div>
          )}
          <Sender
            value={input}
            onChange={setInput}
            loading={busy}
            autoSize={{ minRows: 2, maxRows: 8 }}
            onCancel={() => send({ type: "cancel" })}
            disabled={status === "no_workspace"}
            placeholder={status === "no_workspace" ? "无工作目录，打开文件后可用" : "输入内容并发送…"}
            onSubmit={(value) => {
              handleSend(value)
              setInput("")
            }}
          />
          {(modes.length > 0 ||
            configOptions.some(
              (o) =>
                o &&
                o.type === "select" &&
                !["mode", "model_config"].includes(String(o.category ?? "").toLowerCase()),
            )) && (
            <div className="config-bar">
              {modes.length > 0 && (
                <div className="cfg-item">
                  <span className="cfg-label">模式</span>
                  <Select
                    size="small"
                    className="cfg-select"
                    value={currentMode || undefined}
                    options={modes.map((m) => ({ label: m.name, value: m.id }))}
                    onChange={(value) => {
                      setCurrentMode(value)
                      send({ type: "set_mode", modeId: value })
                    }}
                  />
                </div>
              )}
              {configOptions
                .filter((o) => {
                  if (!o || o.type !== "select") return false
                  const category = String(o.category ?? "").toLowerCase()
                  const id = String(o.id ?? "").toLowerCase()
                  const name = String(o.name ?? "").toLowerCase()
                  // 模式单独渲染；模型参数（model_config）不展示。
                  return (
                    category !== "mode" &&
                    category !== "model_config" &&
                    id !== "mode" &&
                    name !== "mode" &&
                    name !== "模式"
                  )
                })
                .map((o) => {
                  const id = String(o.id)
                  const category = String(o.category ?? "").toLowerCase()
                  const categoryLabels: Record<string, string> = {
                    model: "模型",
                    thought_level: "思考强度",
                    model_config: "模型参数",
                  }
                  const label = categoryLabels[category] ?? String(o.name ?? id)
                  return (
                    <div className="cfg-item" key={id}>
                      <span className="cfg-label">{label}</span>
                      <Select
                        size="small"
                        className="cfg-select"
                        showSearch={id === "model" || category === "model"}
                        optionFilterProp="label"
                        value={String(o.currentValue ?? "") || undefined}
                        options={flattenOptions(o.options)}
                        onChange={(value) => {
                          // 先本地乐观更新，避免受控 Select 在服务端回包前弹回旧值。
                          setConfigOptions((prev) =>
                            prev.map((item) =>
                              String(item.id) === id ? { ...item, currentValue: value } : item,
                            ),
                          )
                          send({ type: "set_config", configId: id, value })
                        }}
                      />
                    </div>
                  )
                })}
            </div>
          )}
        </div>
      </Flex>

      <Modal
        open={!!permission}
        title="需要授权"
        onCancel={() => respondPermission(null)}
        footer={null}
        destroyOnHidden
      >
        <div style={{ marginBottom: 12 }}>
          Agent 想要执行：<b>{permission?.tool}</b>
        </div>
        <Flex vertical gap={8}>
          {permission?.options.map((option) => (
            <Button key={option.optionId} onClick={() => respondPermission(option.optionId)} block>
              {option.name}
            </Button>
          ))}
          <Button danger onClick={() => respondPermission(null)} block>
            拒绝
          </Button>
        </Flex>
      </Modal>

      <Modal
        open={configOpen}
        title="agent_servers 配置"
        width={560}
        okText="保存"
        cancelText="取消"
        onCancel={() => setConfigOpen(false)}
        onOk={() => send({ type: "save_config", json: configText })}
        destroyOnHidden
      >
        <Typography.Text type="secondary" style={{ fontSize: 11 }}>
          {configPath}
        </Typography.Text>
        <Input.TextArea
          value={configText}
          onChange={(e) => setConfigText(e.target.value)}
          autoSize={{ minRows: 12, maxRows: 24 }}
          style={{ marginTop: 8, fontFamily: "ui-monospace, Consolas, monospace", fontSize: 12 }}
        />
      </Modal>
    </ConfigProvider>
  )
}
