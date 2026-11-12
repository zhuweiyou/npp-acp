//! ACP client service: spawn an agent, drive initialize -> session/new ->
//! session/prompt, forward `session/update` notifications, handle permission
//! requests and optional `fs/read_text_file` / `fs/write_text_file` requests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::{
    CancelNotification, ClientCapabilities, ContentBlock, FileSystemCapabilities, InitializeRequest,
    NewSessionRequest, NewSessionResponse, PermissionOptionId, PromptRequest, ReadTextFileRequest,
    ReadTextFileResponse, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionConfigId,
    SessionConfigOptionValue, SessionId, SessionModeId, SessionNotification,
    SetSessionConfigOptionRequest, SetSessionModeRequest, TextContent, WriteTextFileRequest,
    WriteTextFileResponse,
};
use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::{AcpAgent, Agent, Client};

/// Abstraction over "the document store" the agent reads/writes.
/// Phase 2 uses the filesystem; the Notepad++ plugin will use Scintilla buffers.
pub trait DocumentBridge: Send + Sync {
    fn read(&self, path: &str, line: Option<u32>, limit: Option<u32>) -> Result<String, String>;
    fn write(&self, path: &str, content: &str) -> Result<(), String>;
}

/// Filesystem-backed bridge for the dev harness. It is a plain passthrough:
/// which files the agent may touch is an agent-side policy (Claude's readonly /
/// plan / accept-edits / bypass modes), not something this client enforces.
#[derive(Default)]
pub struct FsBridge;

impl DocumentBridge for FsBridge {
    fn read(&self, path: &str, line: Option<u32>, limit: Option<u32>) -> Result<String, String> {
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        if line.is_none() && limit.is_none() {
            return Ok(content);
        }
        let start = line.unwrap_or(1).max(1) as usize - 1;
        let iter = content.lines().skip(start);
        let selected: Vec<&str> = match limit {
            Some(n) => iter.take(n as usize).collect(),
            None => iter.collect(),
        };
        Ok(selected.join("\n"))
    }

    fn write(&self, path: &str, content: &str) -> Result<(), String> {
        std::fs::write(path, content).map_err(|e| e.to_string())
    }
}

/// Walk up from a file/dir to find the enclosing git repository root.
pub fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut current = if start.is_dir() {
        start.to_path_buf()
    } else {
        start.parent()?.to_path_buf()
    };
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionChoice {
    #[serde(rename = "optionId")]
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiEvent {
    Agents { agents: Vec<AgentInfo> },
    Workspace { root: String },
    Status { state: String, detail: String },
    Session { session_id: String },
    SessionConfig { modes: serde_json::Value, config: serde_json::Value },
    Update { session_id: String, update: serde_json::Value },
    PromptDone { session_id: String, stop_reason: String },
    Permission {
        id: String,
        session_id: String,
        tool: String,
        options: Vec<PermissionChoice>,
    },
    Error { message: String },
}

#[derive(Debug)]
pub enum Command {
    NewSession,
    Prompt(String),
    Cancel,
    PermissionResponse { id: String, option_id: Option<String> },
    SetMode { mode_id: String },
    SetConfig { config_id: String, value: String },
}

type PermissionCallback = Box<dyn FnOnce(Option<String>) + Send>;

static DEBUG_LOG: OnceLock<PathBuf> = OnceLock::new();

/// Point agent stdout/stderr tracing at a log file (called by the plugin).
pub fn set_debug_log(path: PathBuf) {
    let _ = DEBUG_LOG.set(path);
}

fn debug_log(line: &str) {
    use std::io::Write;
    if let Some(path) = DEBUG_LOG.get() {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn next_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn session_id_to_string(id: &SessionId) -> String {
    serde_json::to_value(id)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default()
}

fn emit_session_config(tx: &UnboundedSender<UiEvent>, session: &NewSessionResponse) {
    let modes = serde_json::to_value(&session.modes).unwrap_or(serde_json::Value::Null);
    let config = serde_json::to_value(&session.config_options).unwrap_or(serde_json::Value::Null);
    let _ = tx.send(UiEvent::SessionConfig { modes, config });
}

/// Start the ACP service on a dedicated thread with its own tokio runtime.
pub fn start(
    ui_tx: UnboundedSender<UiEvent>,
    agent_cmd: Vec<String>,
    workspace_root: Option<PathBuf>,
    bridge: Arc<dyn DocumentBridge>,
) -> UnboundedSender<Command> {
    let (cmd_tx, cmd_rx) = unbounded_channel::<Command>();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(err) => {
                let _ = ui_tx.send(UiEvent::Error {
                    message: format!("tokio runtime init failed: {err}"),
                });
                return;
            }
        };
        rt.block_on(run(ui_tx, agent_cmd, workspace_root, cmd_rx, bridge));
    });
    cmd_tx
}

async fn run(
    ui_tx: UnboundedSender<UiEvent>,
    agent_cmd: Vec<String>,
    workspace_root: Option<PathBuf>,
    cmd_rx: UnboundedReceiver<Command>,
    bridge: Arc<dyn DocumentBridge>,
) {
    let workspace = match workspace_root {
        Some(root) => {
            let _ = ui_tx.send(UiEvent::Workspace {
                root: root.to_string_lossy().to_string(),
            });
            root
        }
        None => {
            let _ = ui_tx.send(UiEvent::Workspace { root: String::new() });
            let _ = ui_tx.send(UiEvent::Status {
                state: "no_workspace".into(),
                detail: "当前文件不在 git 仓库中，无法启用 agent".into(),
            });
            return;
        }
    };

    let _ = ui_tx.send(UiEvent::Status {
        state: "connecting".into(),
        detail: "launching agent".into(),
    });

    let agent = match AcpAgent::from_args(agent_cmd) {
        Ok(agent) => {
            if DEBUG_LOG.get().is_some() {
                agent.with_debug(|line, _dir| {
                    debug_log(line);
                })
            } else {
                agent
            }
        }
        Err(err) => {
            let _ = ui_tx.send(UiEvent::Error {
                message: format!("spawn agent failed: {err}"),
            });
            return;
        }
    };

    let pending: Arc<Mutex<HashMap<String, PermissionCallback>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let notif_tx = ui_tx.clone();
    let perm_pending = pending.clone();
    let perm_tx = ui_tx.clone();
    let fs_bridge = bridge.clone();
    let fs_bridge_write = bridge.clone();

    let result = Client
        .v1()
        .on_receive_notification(
            move |notification: SessionNotification, _cx| {
                let tx = notif_tx.clone();
                async move {
                    let session_id = session_id_to_string(&notification.session_id);
                    let update = serde_json::to_value(&notification.update)
                        .unwrap_or(serde_json::Value::Null);
                    let kind = update
                        .get("sessionUpdate")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if kind != "available_commands_update" {
                        eprintln!("[acp] update: {kind}");
                    }
                    let _ = tx.send(UiEvent::Update { session_id, update });
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest,
                  responder: agent_client_protocol::Responder<
                RequestPermissionResponse,
                agent_client_protocol::V1,
            >,
                  _cx| {
                let id = format!("perm-{}", next_id());
                let session_id = session_id_to_string(&request.session_id);
                let tool_json = serde_json::to_value(&request.tool_call)
                    .unwrap_or(serde_json::Value::Null);
                let tool = tool_json
                    .get("title")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        tool_json
                            .get("fields")
                            .and_then(|f| f.get("title"))
                            .and_then(|v| v.as_str())
                    })
                    .unwrap_or("tool")
                    .to_string();
                let options: Vec<PermissionChoice> = request
                    .options
                    .iter()
                    .map(|option| PermissionChoice {
                        option_id: option.option_id.0.to_string(),
                        name: option.name.clone(),
                        kind: format!("{:?}", option.kind),
                    })
                    .collect();
                let _ = perm_tx.send(UiEvent::Permission {
                    id: id.clone(),
                    session_id,
                    tool,
                    options: options.clone(),
                });
                let callback: PermissionCallback = Box::new(move |choice: Option<String>| {
                    let outcome = match choice {
                        Some(option_id) => RequestPermissionOutcome::Selected(
                            SelectedPermissionOutcome::new(PermissionOptionId::new(option_id)),
                        ),
                        None => RequestPermissionOutcome::Cancelled,
                    };
                    let _ = responder.respond(RequestPermissionResponse::new(outcome));
                });
                perm_pending.lock().unwrap().insert(id, callback);
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: ReadTextFileRequest, responder, _cx| {
                let path = request.path.to_string_lossy().to_string();
                match fs_bridge.read(&path, request.line, request.limit) {
                    Ok(content) => responder.respond(ReadTextFileResponse::new(content)),
                    Err(err) => responder.respond_with_internal_error(err),
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: WriteTextFileRequest, responder, _cx| {
                let path = request.path.to_string_lossy().to_string();
                match fs_bridge_write.write(&path, &request.content) {
                    Ok(()) => responder.respond(WriteTextFileResponse::new()),
                    Err(err) => responder.respond_with_internal_error(err),
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(agent, |connection: ConnectionTo<Agent>| async move {
            let conn_for_loop = connection.clone();
            let workspace_for_loop = workspace.clone();
            tokio::spawn(async move {
                command_loop(conn_for_loop, workspace_for_loop, ui_tx, cmd_rx, pending).await;
            });
            // Keep the connection (and its dispatch loop) alive.
            connection.terminated().await;
            Ok(())
        })
        .await;

    if let Err(err) = result {
        // `ui_tx` was moved into the closure; a dedicated clone is used for the
        // final error through the same channel in `command_loop` results.
        eprintln!("[acp] connection ended: {err}");
    }
}

async fn command_loop(
    connection: ConnectionTo<Agent>,
    workspace: PathBuf,
    ui_tx: UnboundedSender<UiEvent>,
    mut cmd_rx: UnboundedReceiver<Command>,
    pending: Arc<Mutex<HashMap<String, PermissionCallback>>>,
) {
    let init_request = InitializeRequest::new(ProtocolVersion::V1).client_capabilities(
        ClientCapabilities::new().fs(
            FileSystemCapabilities::new()
                .read_text_file(true)
                .write_text_file(true),
        ),
    );
    match connection.send_request(init_request).block_task().await {
        Ok(init) => eprintln!("[acp] initialized: {:?}", init.agent_info),
        Err(err) => {
            let _ = ui_tx.send(UiEvent::Error {
                message: err.to_string(),
            });
            return;
        }
    }
    let _ = ui_tx.send(UiEvent::Status {
        state: "connected".into(),
        detail: "agent initialized".into(),
    });

    let session = match connection
        .send_request(NewSessionRequest::new(workspace.clone()))
        .block_task()
        .await
    {
        Ok(session) => session,
        Err(err) => {
            let _ = ui_tx.send(UiEvent::Error {
                message: err.to_string(),
            });
            return;
        }
    };
    let mut session_id = session.session_id.clone();
    emit_session_config(&ui_tx, &session);
    let _ = ui_tx.send(UiEvent::Session {
        session_id: session_id_to_string(&session_id),
    });

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            Command::NewSession => {
                match connection
                    .send_request(NewSessionRequest::new(workspace.clone()))
                    .block_task()
                    .await
                {
                    Ok(session) => {
                        emit_session_config(&ui_tx, &session);
                        session_id = session.session_id;
                        let _ = ui_tx.send(UiEvent::Session {
                            session_id: session_id_to_string(&session_id),
                        });
                    }
                    Err(err) => {
                        let _ = ui_tx.send(UiEvent::Error {
                            message: err.to_string(),
                        });
                    }
                }
            }
            Command::Prompt(text) => {
                let conn = connection.clone();
                let sid = session_id.clone();
                let tx = ui_tx.clone();
                tokio::spawn(async move {
                    let request = PromptRequest::new(
                        sid.clone(),
                        vec![ContentBlock::Text(TextContent::new(text))],
                    );
                    match conn.send_request(request).block_task().await {
                        Ok(response) => {
                            let _ = tx.send(UiEvent::PromptDone {
                                session_id: session_id_to_string(&sid),
                                stop_reason: format!("{:?}", response.stop_reason),
                            });
                        }
                        Err(err) => {
                            let _ = tx.send(UiEvent::Error {
                                message: err.to_string(),
                            });
                        }
                    }
                });
            }
            Command::Cancel => {
                let _ = connection.send_notification(CancelNotification::new(session_id.clone()));
            }
            Command::PermissionResponse { id, option_id } => {
                if let Some(callback) = pending.lock().unwrap().remove(&id) {
                    callback(option_id);
                }
            }
            Command::SetMode { mode_id } => {
                let request = SetSessionModeRequest::new(
                    session_id.clone(),
                    SessionModeId::new(mode_id),
                );
                if let Err(err) = connection.send_request(request).block_task().await {
                    let _ = ui_tx.send(UiEvent::Error {
                        message: err.to_string(),
                    });
                }
            }
            Command::SetConfig { config_id, value } => {
                let request = SetSessionConfigOptionRequest::new(
                    session_id.clone(),
                    SessionConfigId::new(config_id),
                    SessionConfigOptionValue::value_id(value),
                );
                if let Err(err) = connection.send_request(request).block_task().await {
                    let _ = ui_tx.send(UiEvent::Error {
                        message: err.to_string(),
                    });
                }
            }
        }
    }
}
