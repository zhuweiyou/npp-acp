import { useEffect, useRef, useState } from "react"
import { Bubble, Sender } from "@ant-design/x"
import XMarkdown from "@ant-design/x-markdown"
import { Badge, Button, ConfigProvider, Flex, Input, Modal, Select, Typography, theme as antdTheme } from "antd"
import { onEvent, send, type IpcMessage } from "./ipc"

type ChatRole = "user" | "assistant" | "system"
type ThemeMode = "light" | "dark"

interface ChatMessage {
  key: string
  role: ChatRole
  content: string
  streaming?: boolean
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

function initialTheme(): ThemeMode {
  if (typeof window !== "undefined" && window.matchMedia) {
    return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light"
  }
  return "light"
}

const baseName = (p?: string) => {
  if (!p) return ""
  const parts = p.split(/[\\/]/).filter(Boolean)
  return parts[parts.length - 1] ?? p
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
  const toolKey = useRef<string | null>(null)
  const toolCount = useRef(0)
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
      const content = update.content as { text?: string } | undefined
      queueText(content?.text ?? "")
    } else if (kind === "tool_call" || kind === "tool_call_update") {
      toolCount.current += 1
      const title = String(update.title ?? update.kind ?? "tool")
      setMessages((prev) => {
        const key = toolKey.current
        const content = `🔧 工具调用 ${toolCount.current}：${title}`
        if (key) {
          return prev.map((m) => (m.key === key ? { ...m, content } : m))
        }
        const k = nextKey()
        toolKey.current = k
        return [...prev, { key: k, role: "system", content }]
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
    toolKey.current = null
    toolCount.current = 0
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
        toolKey.current = null
        toolCount.current = 0
      } else if (type === "error") {
        setBusy(false)
        setStatus("error")
        pushSystem(`⚠️ ${event.message}`)
      } else if (type === "theme") {
        setMode(event.mode === "dark" ? "dark" : "light")
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

  const items = messages.map((m) => ({
    key: m.key,
    role: m.role,
    content: m.content || (m.streaming ? "…" : ""),
  }))

  const statusBadge =
    status === "connected" ? (
      <Badge status="success" text="已连接" />
    ) : status === "no_workspace" ? (
      <Badge status="default" text="无 Git 工作区" />
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
        <Flex align="center" justify="space-between" wrap="wrap" gap={6} className="app-header">
          <Flex align="center" gap={6} className="app-header-left">
            <Select
              size="small"
              style={{ width: 108 }}
              value={agentId}
              placeholder="选择 agent"
              options={agents.map((a) => ({ label: a.name, value: a.id }))}
              onChange={setAgentId}
            />
            <Button size="small" onClick={handleNewSession} disabled={status === "no_workspace"}>
              新会话
            </Button>
            <Button size="small" onClick={() => send({ type: "get_config" })}>
              设置
            </Button>
          </Flex>
          <Flex align="center" gap={6} className="app-header-right">
            {statusBadge}
            <Typography.Text type="secondary" className="app-meta" title={statusDetail}>
              {workspace ? `📁 ${baseName(workspace)}` : "无 git 仓库"}
              {sessionId ? ` · ${sessionId.slice(0, 8)}` : ""}
            </Typography.Text>
          </Flex>
        </Flex>

        {status === "no_workspace" && (
          <div className="app-notice">当前打开的文件不在任何 git 仓库中。请打开 git 仓库里的文件后再使用 agent。</div>
        )}

        <div className="app-body" ref={bodyRef}>
          <Bubble.List
            items={items}
            role={{
              user: {
                placement: "end",
                variant: "borderless",
                contentRender: (content) => <div className="msg msg-user">{String(content)}</div>,
              },
              assistant: {
                placement: "start",
                variant: "borderless",
                contentRender: (content) => (
                  <div className="msg msg-assistant">
                    <XMarkdown content={String(content)} />
                  </div>
                ),
              },
              system: { placement: "start", variant: "borderless" },
            }}
            styles={{ root: { width: "100%", maxWidth: "100%" } }}
          />
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
            onCancel={() => send({ type: "cancel" })}
            disabled={status === "no_workspace"}
            placeholder={status === "no_workspace" ? "需要 git 工作区才能使用 agent" : "输入内容并发送…"}
            onSubmit={(value) => {
              handleSend(value)
              setInput("")
            }}
          />
          {(modes.length > 0 ||
            configOptions.some(
              (o) => o && o.type === "select" && String(o.category ?? "").toLowerCase() !== "mode",
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
                  // The mode selector is rendered separately from SessionModeState.
                  return category !== "mode" && id !== "mode" && name !== "mode" && name !== "模式"
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
                        onChange={(value) => send({ type: "set_config", configId: id, value })}
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
