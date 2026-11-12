//! Phase 2 dev harness: standalone tao window hosting a wry WebView2 that loads
//! the Vite+ dev server. It bridges the React UI to a real ACP agent (mock-agent)
//! over `agent-client-protocol`, streaming `session/update` notifications back
//! into the chat UI.

mod acp_client_reexport {}
pub use npp_acp::acp_client;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

use npp_acp::acp_client::{AgentInfo, Command, UiEvent};
use serde_json::{json, Value};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use tokio::sync::mpsc::UnboundedSender;
use wry::{WebView, WebViewBuilder};

const DEV_URL: &str = "http://localhost:5173";

static CMD_TX: OnceLock<UnboundedSender<Command>> = OnceLock::new();
static AGENT_LABEL: OnceLock<String> = OnceLock::new();

/// Latest agent/session state, replayed to the UI after it finishes loading
/// (the ACP service may connect before the webview script registers its listener).
#[derive(Default)]
struct UiState {
    status: String,
    detail: String,
    session_id: Option<String>,
    workspace: Option<String>,
}

static UI_STATE: OnceLock<Mutex<UiState>> = OnceLock::new();

fn ui_state() -> &'static Mutex<UiState> {
    UI_STATE.get_or_init(|| Mutex::new(UiState::default()))
}

fn replay_state(holder: &Rc<RefCell<Option<WebView>>>) {
    let (status, detail, session_id, workspace) = {
        let state = ui_state().lock().unwrap();
        (
            state.status.clone(),
            state.detail.clone(),
            state.session_id.clone(),
            state.workspace.clone(),
        )
    };

    if let Some(root) = workspace {
        reply(
            holder,
            serde_json::to_value(UiEvent::Workspace { root }).unwrap_or(Value::Null),
        );
    }
    if !status.is_empty() {
        reply(
            holder,
            serde_json::to_value(UiEvent::Status {
                state: status,
                detail,
            })
            .unwrap_or(Value::Null),
        );
    }
    if let Some(session_id) = session_id {
        reply(
            holder,
            serde_json::to_value(UiEvent::Session { session_id }).unwrap_or(Value::Null),
        );
    }
}

fn reply(holder: &Rc<RefCell<Option<WebView>>>, payload: Value) {
    let js = format!("window.__nppacp && window.__nppacp.emit({payload});");
    if let Some(webview) = holder.borrow().as_ref() {
        if let Err(err) = webview.evaluate_script(&js) {
            eprintln!("[ipc] evaluate_script failed: {err}");
        }
    }
}

fn handle_ipc(holder: &Rc<RefCell<Option<WebView>>>, body: &str) {
    let parsed: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("[ipc] invalid json: {err}: {body}");
            return;
        }
    };

    match parsed.get("type").and_then(Value::as_str) {
        Some("ping") => {
            let text = parsed.get("text").and_then(Value::as_str).unwrap_or("");
            reply(
                holder,
                json!({ "type": "pong", "text": format!("pong: {text}") }),
            );
        }
        Some("list_agents") => {
            let label = AGENT_LABEL
                .get()
                .cloned()
                .unwrap_or_else(|| "Mock Agent".to_string());
            reply(
                holder,
                serde_json::to_value(UiEvent::Agents {
                    agents: vec![AgentInfo {
                        id: "default".into(),
                        name: label,
                    }],
                })
                .unwrap_or(Value::Null),
            );
            // The agent may have connected before the page listener existed.
            replay_state(holder);
        }
        Some("prompt") => {
            let text = parsed
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if let Some(tx) = CMD_TX.get() {
                let _ = tx.send(Command::Prompt(text));
            }
        }
        Some("new_session") => {
            if let Some(tx) = CMD_TX.get() {
                let _ = tx.send(Command::NewSession);
            }
        }
        Some("cancel") => {
            if let Some(tx) = CMD_TX.get() {
                let _ = tx.send(Command::Cancel);
            }
        }
        Some("permission_response") => {
            let id = parsed
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let option_id = parsed
                .get("optionId")
                .and_then(Value::as_str)
                .map(|s| s.to_string());
            if let Some(tx) = CMD_TX.get() {
                let _ = tx.send(Command::PermissionResponse { id, option_id });
            }
        }
        Some("set_mode") => {
            let mode_id = parsed
                .get("modeId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if let Some(tx) = CMD_TX.get() {
                let _ = tx.send(Command::SetMode { mode_id });
            }
        }
        Some("set_config") => {
            let config_id = parsed
                .get("configId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let value = parsed
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if let Some(tx) = CMD_TX.get() {
                let _ = tx.send(Command::SetConfig { config_id, value });
            }
        }
        other => {
            eprintln!("[ipc] unhandled message type: {other:?}");
            reply(
                holder,
                json!({ "type": "error", "message": format!("unhandled type: {other:?}") }),
            );
        }
    }
}

fn main() -> anyhow::Result<()> {
    let event_loop = EventLoopBuilder::<UiEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    // UI events -> event loop (so WebView calls happen on the UI thread).
    let (ui_tx, mut ui_rx) = tokio::sync::mpsc::unbounded_channel::<UiEvent>();
    std::thread::spawn(move || {
        while let Some(event) = ui_rx.blocking_recv() {
            if proxy.send_event(event).is_err() {
                break;
            }
        }
    });

    // Human-readable label from the configured agent command.
    let agent_label = match std::env::var("NPP_ACP_AGENT") {
        Ok(spec) => {
            let trimmed = spec.trim().to_string();
            let base = if trimmed.starts_with('[') {
                serde_json::from_str::<Vec<String>>(&trimmed)
                    .ok()
                    .and_then(|v| v.last().cloned())
                    .unwrap_or(trimmed)
            } else {
                trimmed
            };
            std::path::Path::new(&base)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or(base)
        }
        Err(_) => "Mock Agent".to_string(),
    };
    let _ = AGENT_LABEL.set(agent_label.clone());
    let _ = ui_tx.send(UiEvent::Agents {
        agents: vec![AgentInfo {
            id: "default".into(),
            name: agent_label,
        }],
    });

    // Resolve the agent command: NPP_ACP_AGENT accepts either a JSON array
    // (e.g. ["cmd","/c","claude-agent-acp.cmd"]) or a plain command string.
    // Default: the sibling mock-agent built in the same target dir.
    let default_agent = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("mock-agent.exe")))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "mock-agent.exe".to_string());
    let agent_args: Vec<String> = match std::env::var("NPP_ACP_AGENT") {
        Ok(spec) => {
            let trimmed = spec.trim().to_string();
            if trimmed.starts_with('[') {
                serde_json::from_str::<Vec<String>>(&trimmed).unwrap_or_else(|_| vec![trimmed])
            } else {
                vec![trimmed]
            }
        }
        Err(_) => vec![default_agent],
    };
    eprintln!("[npp-acp] agent command: {:?}", agent_args);

    // Workspace rule: the agent's root is the git repo that contains the file
    // being edited. Without a git repo, the agent is disabled.
    let start_hint = std::env::var("NPP_ACP_WORKSPACE")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok());
    let workspace_root = start_hint
        .as_deref()
        .and_then(acp_client::find_git_root);
    eprintln!("[npp-acp] workspace root: {:?}", workspace_root);
    let bridge = Arc::new(acp_client::FsBridge);

    let cmd_tx = acp_client::start(ui_tx.clone(), agent_args, workspace_root, bridge);
    let _ = CMD_TX.set(cmd_tx);

    // Dev-only: when NPP_ACP_SELFTEST=1, append ?selftest=1 so the UI auto-sends
    // a prompt after load (exercises the real user->assistant path).
    let url = if std::env::var("NPP_ACP_SELFTEST").is_ok() {
        format!("{DEV_URL}/?selftest=1")
    } else {
        DEV_URL.to_string()
    };

    let window = WindowBuilder::new()
        .with_title("npp-acp — Phase 2 (ACP)")
        .with_inner_size(LogicalSize::new(1180.0, 800.0))
        .with_min_inner_size(LogicalSize::new(720.0, 480.0))
        .build(&event_loop)?;

    let webview_holder: Rc<RefCell<Option<WebView>>> = Rc::new(RefCell::new(None));
    let handler_holder = webview_holder.clone();

    let webview = WebViewBuilder::new()
        .with_url(&url)
        .with_devtools(true)
        .with_ipc_handler(move |request| {
            let body = request.body().clone();
            handle_ipc(&handler_holder, &body);
        })
        .build(&window)?;

    *webview_holder.borrow_mut() = Some(webview);
    println!("[npp-acp] window up, loading {url}");

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(ui_event) => {
                {
                    let mut state = ui_state().lock().unwrap();
                    match &ui_event {
                        UiEvent::Status { state: s, detail } => {
                            state.status = s.clone();
                            state.detail = detail.clone();
                        }
                        UiEvent::Session { session_id } => {
                            state.session_id = Some(session_id.clone());
                        }
                        UiEvent::Workspace { root } => {
                            state.workspace = Some(root.clone());
                        }
                        _ => {}
                    }
                }
                let payload = serde_json::to_value(&ui_event).unwrap_or(Value::Null);
                reply(&webview_holder, payload);
            }
            _ => {}
        }
        let _ = &webview_holder;
    });
}
