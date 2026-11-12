//! Notepad++ plugin (cdylib): a dockable right-side panel hosting a WebView2 UI
//! that talks ACP to an agent. This is the installable plugin target; the
//! `npp-acp-dev` bin is the standalone dev harness.

pub mod acp_client;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, PostMessageW, RegisterClassW, SendMessageW,
    SetWindowPos, ShowWindow, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SW_SHOW, WINDOW_EX_STYLE,
    WNDCLASSW, WS_CHILD, WS_VISIBLE,
};

// ---------------------------------------------------------------------------
// Notepad++ ABI types
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NppData {
    pub npp: isize,
    pub scintilla_main: isize,
    pub scintilla_second: isize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ShortcutKey {
    pub is_ctrl: bool,
    pub is_alt: bool,
    pub is_shift: bool,
    pub key: u8,
}

#[repr(C)]
pub struct FuncItem {
    pub name: [u16; 64],
    pub pfunc: Option<extern "system" fn()>,
    pub cmd_id: i32,
    pub init2check: bool,
    pub shkey: *mut ShortcutKey,
}

impl Default for FuncItem {
    fn default() -> Self {
        Self {
            name: [0; 64],
            pfunc: None,
            cmd_id: 0,
            init2check: false,
            shkey: std::ptr::null_mut(),
        }
    }
}

#[repr(C)]
pub struct DockedWidgetData {
    pub hclient: isize,
    pub psz_name: *const u16,
    pub dlg_id: i32,
    pub u_mask: u32,
    pub hicon_tab: *mut core::ffi::c_void,
    pub psz_add_info: *const u16,
    pub rc_float: Rect,
    pub i_prev_cont: i32,
    pub psz_module_name: *const u16,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
pub struct Nmhdr {
    pub hwnd_from: isize,
    pub id_from: usize,
    pub code: u32,
}

#[repr(C)]
pub struct SCNotification {
    pub nmhdr: Nmhdr,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const WM_USER: u32 = 0x0400;
const NPPMSG: u32 = WM_USER + 1000;
const NPPM_MODELESSDIALOG: u32 = NPPMSG + 12;
const NPPM_DMMSHOW: u32 = NPPMSG + 30;
const NPPM_DMMREGASDCKDLG: u32 = NPPMSG + 33;
const NPPM_GETFULLPATHFROMBUFFERID: u32 = NPPMSG + 58;
const NPPM_GETCURRENTBUFFERID: u32 = NPPMSG + 60;
const NPPM_ISDARKMODEENABLED: u32 = NPPMSG + 107;

const NPPN_READY: u32 = 1001;
const NPPN_DARKMODECHANGED: u32 = 1027;
const NPPN_SHUTDOWN: u32 = 1009;

const WM_SIZE: u32 = 0x0005;
const WM_DESTROY: u32 = 0x0002;
const WM_WINDOWPOSCHANGED: u32 = 0x0047;
const WM_APP_UI_EVENT: u32 = 0x8000 + 1;
const WM_APP_SET_WIDTH: u32 = 0x8000 + 3;
/// Preferred default width of the dock panel (plugin side), in pixels.
const PANEL_WIDTH: i32 = 414;

const DWS_DF_CONT_RIGHT: u32 = 1 << 28;

const MODELESSDIALOGADD: usize = 0;

// ---------------------------------------------------------------------------
// Global state
// ---------------------------------------------------------------------------

static NPP_HANDLE: AtomicIsize = AtomicIsize::new(0);
static SCINTILLA_MAIN: AtomicIsize = AtomicIsize::new(0);
static PANEL_HWND: AtomicIsize = AtomicIsize::new(0);
static ACP_STARTED: AtomicIsize = AtomicIsize::new(0);

static FUNCS: OnceLock<usize> = OnceLock::new();
static CMD_TX: OnceLock<Mutex<Option<tokio::sync::mpsc::UnboundedSender<acp_client::Command>>>> =
    OnceLock::new();
static UI_QUEUE: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
static UI_STATE: OnceLock<Mutex<UiState>> = OnceLock::new();
static THEME_DARK: AtomicIsize = AtomicIsize::new(0);

thread_local! {
    static WEBVIEW: RefCell<Option<wry::WebView>> = RefCell::new(None);
}

#[derive(Default)]
struct UiState {
    status: String,
    detail: String,
    session_id: Option<String>,
    workspace: Option<String>,
}

fn ui_state() -> &'static Mutex<UiState> {
    UI_STATE.get_or_init(|| Mutex::new(UiState::default()))
}

fn cmd_tx() -> &'static Mutex<Option<tokio::sync::mpsc::UnboundedSender<acp_client::Command>>> {
    CMD_TX.get_or_init(|| Mutex::new(None))
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Wide string allocated for the process lifetime (pointers handed to Notepad++
/// must stay valid after `create_panel` returns).
fn leak_wide(s: &str) -> *const u16 {
    Box::leak(to_wide(s).into_boxed_slice()).as_ptr()
}

fn log(msg: &str) {
    use std::io::Write;
    // Resize events are extremely chatty; only record them in debug mode.
    if msg.starts_with("resize") && std::env::var("NPP_ACP_DEBUG").is_err() {
        return;
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let dir = std::path::Path::new(&local).join("npp-acp");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("plugin.log");
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() > 2_000_000 {
                let _ = std::fs::remove_file(&path);
            }
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let _ = writeln!(f, "[{ts}] {msg}");
        }
    }
}

const SCI_GETLENGTH: u32 = 2006;
const SCI_GETTEXT: u32 = 2182;
const SCI_SETTEXT: u32 = 2181;

fn scintilla_get_text(sci: isize) -> Option<String> {
    if sci == 0 {
        return None;
    }
    let hwnd = HWND(sci as *mut core::ffi::c_void);
    let len = unsafe { SendMessageW(hwnd, SCI_GETLENGTH, Some(WPARAM(0)), Some(LPARAM(0))) }.0 as usize;
    let mut buf = vec![0u8; len + 1];
    let n = unsafe {
        SendMessageW(
            hwnd,
            SCI_GETTEXT,
            Some(WPARAM(len + 1)),
            Some(LPARAM(buf.as_mut_ptr() as isize)),
        )
    }
    .0 as usize;
    buf.truncate(n.min(len));
    String::from_utf8(buf).ok()
}

fn scintilla_set_text(sci: isize, text: &str) {
    if sci == 0 {
        return;
    }
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    let hwnd = HWND(sci as *mut core::ffi::c_void);
    unsafe {
        SendMessageW(
            hwnd,
            SCI_SETTEXT,
            Some(WPARAM(0)),
            Some(LPARAM(bytes.as_ptr() as isize)),
        );
    }
}

/// Document bridge that maps the active editor buffer to Scintilla and other
/// paths to disk. This is what lets the agent see unsaved editor content.
struct ScintillaBridge {
    sci: isize,
}

impl acp_client::DocumentBridge for ScintillaBridge {
    fn read(&self, path: &str, line: Option<u32>, limit: Option<u32>) -> Result<String, String> {
        let current = current_file_path();
        let is_current = current
            .as_ref()
            .map(|p| p.to_string_lossy().eq_ignore_ascii_case(path))
            .unwrap_or(false);
        let content = if is_current {
            scintilla_get_text(self.sci).ok_or_else(|| "read editor buffer failed".to_string())?
        } else {
            std::fs::read_to_string(path).map_err(|e| e.to_string())?
        };
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
        let current = current_file_path();
        let is_current = current
            .as_ref()
            .map(|p| p.to_string_lossy().eq_ignore_ascii_case(path))
            .unwrap_or(false);
        if is_current {
            scintilla_set_text(self.sci, content);
            Ok(())
        } else {
            std::fs::write(path, content).map_err(|e| e.to_string())
        }
    }
}

fn agent_log_path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| "C:\\".to_string());
    PathBuf::from(base).join("npp-acp").join("agent.log")
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct AgentServer {
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
pub struct AgentsConfig {
    #[serde(default, alias = "agent_servers", alias = "agentServers")]
    pub agent_servers: Vec<AgentServer>,
    #[serde(default, alias = "default_agent", alias = "defaultAgent")]
    pub default_agent: Option<String>,
}

fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("NPP_ACP_CONFIG") {
        return PathBuf::from(p);
    }
    let base = std::env::var("APPDATA").unwrap_or_else(|_| "C:\\".to_string());
    PathBuf::from(base)
        .join("Notepad++")
        .join("plugins")
        .join("config")
        .join("npp-acp")
        .join("agents.json")
}

fn default_config() -> AgentsConfig {
    AgentsConfig {
        agent_servers: vec![AgentServer {
            id: "claude".to_string(),
            name: "Claude Agent".to_string(),
            command: "cmd".to_string(),
            args: vec!["/c".to_string(), "claude-agent-acp.cmd".to_string()],
            env: Default::default(),
        }],
        default_agent: Some("claude".to_string()),
    }
}

fn load_config() -> AgentsConfig {
    let path = config_path();
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(cfg) = serde_json::from_str::<AgentsConfig>(&text) {
            if !cfg.agent_servers.is_empty() {
                return cfg;
            }
        }
    }
    let cfg = default_config();
    let _ = save_config(&cfg);
    cfg
}

fn save_config(cfg: &AgentsConfig) -> Result<(), String> {
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| e.to_string())
}

fn active_server(cfg: &AgentsConfig) -> Option<AgentServer> {
    let id = cfg
        .default_agent
        .clone()
        .or_else(|| cfg.agent_servers.first().map(|s| s.id.clone()));
    id.and_then(|id| {
        cfg.agent_servers
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .or_else(|| cfg.agent_servers.first().cloned())
    })
}

fn npp_hwnd() -> HWND {
    HWND(NPP_HANDLE.load(Ordering::Relaxed) as *mut core::ffi::c_void)
}

fn panel_hwnd() -> HWND {
    HWND(PANEL_HWND.load(Ordering::Relaxed) as *mut core::ffi::c_void)
}

// ---------------------------------------------------------------------------
// WebView helpers
// ---------------------------------------------------------------------------

fn reply_json(json: &str) {
    let js = format!("window.__nppacp && window.__nppacp.emit({json});");
    WEBVIEW.with(|cell| {
        if let Some(webview) = cell.borrow().as_ref() {
            let _ = webview.evaluate_script(&js);
        }
    });
}

fn update_state(event: &acp_client::UiEvent) {
    let mut state = ui_state().lock().unwrap();
    match event {
        acp_client::UiEvent::Status { state: s, detail } => {
            log(&format!("status {s} {detail}"));
            state.status = s.clone();
            state.detail = detail.clone();
        }
        acp_client::UiEvent::Session { session_id } => {
            log(&format!("session {session_id}"));
            state.session_id = Some(session_id.clone());
        }
        acp_client::UiEvent::Workspace { root } => {
            log(&format!("workspace {root}"));
            state.workspace = Some(root.clone());
        }
        acp_client::UiEvent::SessionConfig { modes, config } => {
            let mcount = modes
                .get("availableModes")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            let ccount = config.as_array().map(|a| a.len()).unwrap_or(0);
            log(&format!("session_config modes={mcount} config={ccount}"));
        }
        _ => {}
    }
}

fn replay_state() {
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
        let ev = acp_client::UiEvent::Workspace { root };
        if let Ok(json) = serde_json::to_string(&ev) {
            reply_json(&json);
        }
    }
    if !status.is_empty() {
        let ev = acp_client::UiEvent::Status { state: status, detail };
        if let Ok(json) = serde_json::to_string(&ev) {
            reply_json(&json);
        }
    }
    if let Some(session_id) = session_id {
        let ev = acp_client::UiEvent::Session { session_id };
        if let Ok(json) = serde_json::to_string(&ev) {
            reply_json(&json);
        }
    }
}

fn agent_args() -> Vec<String> {
    let cfg = load_config();
    if let Some(server) = active_server(&cfg) {
        let mut args: Vec<String> = server
            .env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        args.push(server.command.clone());
        args.extend(server.args.iter().cloned());
        args
    } else {
        default_config()
            .agent_servers
            .first()
            .map(|s| {
                let mut v = vec![s.command.clone()];
                v.extend(s.args.iter().cloned());
                v
            })
            .unwrap_or_else(|| vec!["cmd".into(), "/c".into(), "claude-agent-acp.cmd".into()])
    }
}

#[allow(dead_code)]
fn agent_label() -> String {
    active_server(&load_config())
        .map(|s| s.name)
        .unwrap_or_else(|| "Agent".to_string())
}
fn current_file_path() -> Option<PathBuf> {
    let npp = npp_hwnd();
    let id = unsafe { SendMessageW(npp, NPPM_GETCURRENTBUFFERID, Some(WPARAM(0)), Some(LPARAM(0))) }.0 as usize;
    if id == 0 {
        return None;
    }
    let mut buf = [0u16; 2048];
    let n = unsafe {
        SendMessageW(
            npp,
            NPPM_GETFULLPATHFROMBUFFERID,
            Some(WPARAM(id)),
            Some(LPARAM(buf.as_mut_ptr() as isize)),
        )
    }
    .0;
    if n <= 0 {
        return None;
    }
    let s = String::from_utf16_lossy(&buf[..n as usize]);
    if s.is_empty() {
        None
    } else {
        Some(PathBuf::from(s))
    }
}

fn is_dark() -> bool {
    THEME_DARK.load(Ordering::Relaxed) != 0
}

fn send_theme() {
    let mode = if is_dark() { "dark" } else { "light" };
    reply_json(&format!("{{\"type\":\"theme\",\"mode\":\"{mode}\"}}"));
}

// ---------------------------------------------------------------------------
// ACP service lifecycle
// ---------------------------------------------------------------------------

fn start_acp() {
    if ACP_STARTED.load(Ordering::SeqCst) != 0 {
        return;
    }
    let workspace = current_file_path().and_then(|p| acp_client::find_git_root(&p));
    if workspace.is_none() {
        {
            let mut state = ui_state().lock().unwrap();
            state.workspace = Some(String::new());
            state.status = "no_workspace".into();
            state.detail = "当前文件不在 git 仓库中".into();
        }
        reply_json("{\"type\":\"workspace\",\"root\":\"\"}");
        reply_json("{\"type\":\"status\",\"state\":\"no_workspace\",\"detail\":\"当前文件不在 git 仓库中\"}");
        return;
    }
    if ACP_STARTED.swap(1, Ordering::SeqCst) != 0 {
        return;
    }
    let panel = panel_hwnd();
    let panel_addr = panel.0 as isize;
    let (ui_tx, mut ui_rx) = tokio::sync::mpsc::unbounded_channel::<acp_client::UiEvent>();

    std::thread::spawn(move || {
        let panel = HWND(panel_addr as *mut core::ffi::c_void);
        while let Some(event) = ui_rx.blocking_recv() {
            update_state(&event);
            if let Ok(json) = serde_json::to_string(&event) {
                UI_QUEUE
                    .get_or_init(|| Mutex::new(VecDeque::new()))
                    .lock()
                    .unwrap()
                    .push_back(json);
                unsafe {
                    let _ = PostMessageW(Some(panel), WM_APP_UI_EVENT, WPARAM(0), LPARAM(0));
                }
            }
        }
    });

    let bridge = Arc::new(ScintillaBridge {
        sci: SCINTILLA_MAIN.load(Ordering::Relaxed),
    });
    acp_client::set_debug_log(agent_log_path());
    let tx = acp_client::start(ui_tx, agent_args(), workspace, bridge);
    *cmd_tx().lock().unwrap() = Some(tx);
}

// ---------------------------------------------------------------------------
// IPC from the webview
// ---------------------------------------------------------------------------

fn handle_ipc(body: &str) {
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return,
    };
    let msg_type = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if msg_type == "jserror" {
        log(&format!(
            "JSERROR {}",
            parsed.get("message").and_then(|v| v.as_str()).unwrap_or("")
        ));
        return;
    }
    log(&format!("ipc {msg_type}"));
    match msg_type {
        "list_agents" => {
            let cfg = load_config();
            let agents: Vec<acp_client::AgentInfo> = cfg
                .agent_servers
                .iter()
                .map(|s| acp_client::AgentInfo {
                    id: s.id.clone(),
                    name: s.name.clone(),
                })
                .collect();
            let ev = acp_client::UiEvent::Agents { agents };
            if let Ok(json) = serde_json::to_string(&ev) {
                reply_json(&json);
            }
            replay_state();
        }
        "get_config" => {
            let cfg = load_config();
            let text = serde_json::to_string_pretty(&cfg).unwrap_or_default();
            let payload = serde_json::json!({
                "type": "config",
                "path": config_path().to_string_lossy(),
                "json": text,
            });
            reply_json(&payload.to_string());
        }
        "save_config" => {
            let text = parsed.get("json").and_then(|v| v.as_str()).unwrap_or("");
            match serde_json::from_str::<AgentsConfig>(text) {
                Ok(cfg) => match save_config(&cfg) {
                    Ok(()) => reply_json("{\"type\":\"config_saved\"}"),
                    Err(err) => reply_json(
                        &serde_json::json!({ "type": "error", "message": err }).to_string(),
                    ),
                },
                Err(err) => reply_json(
                    &serde_json::json!({ "type": "error", "message": format!("JSON 无效: {err}") })
                        .to_string(),
                ),
            }
        }
        "prompt" => {
            let text = parsed.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if let Some(tx) = cmd_tx().lock().unwrap().as_ref() {
                let _ = tx.send(acp_client::Command::Prompt(text));
            }
        }
        "new_session" => {
            if let Some(tx) = cmd_tx().lock().unwrap().as_ref() {
                let _ = tx.send(acp_client::Command::NewSession);
            }
        }
        "cancel" => {
            if let Some(tx) = cmd_tx().lock().unwrap().as_ref() {
                let _ = tx.send(acp_client::Command::Cancel);
            }
        }
        "permission_response" => {
            let id = parsed.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let option_id = parsed.get("optionId").and_then(|v| v.as_str()).map(|s| s.to_string());
            if let Some(tx) = cmd_tx().lock().unwrap().as_ref() {
                let _ = tx.send(acp_client::Command::PermissionResponse { id, option_id });
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Window proc
// ---------------------------------------------------------------------------

fn resize_webview(hwnd: HWND) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    let w = (r.right - r.left).max(1) as u32;
    let h = (r.bottom - r.top).max(1) as u32;
    log(&format!("resize {w}x{h}"));
    let bounds = wry::Rect {
        position: wry::dpi::Position::Physical(wry::dpi::PhysicalPosition::new(0, 0)),
        size: wry::dpi::Size::Physical(wry::dpi::PhysicalSize::new(w, h)),
    };
    WEBVIEW.with(|cell| {
        if let Some(webview) = cell.borrow().as_ref() {
            let _ = webview.set_bounds(bounds);
        }
    });
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_APP_UI_EVENT => {
            let mut pending = Vec::new();
            if let Some(queue) = UI_QUEUE.get() {
                let mut q = queue.lock().unwrap();
                while let Some(json) = q.pop_front() {
                    pending.push(json);
                }
            }
            for json in pending {
                reply_json(&json);
            }
            LRESULT(0)
        }
        WM_SIZE => {
            resize_webview(hwnd);
            LRESULT(0)
        }
        WM_WINDOWPOSCHANGED => {
            resize_webview(hwnd);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_APP_SET_WIDTH => {
            let mut r = RECT::default();
            unsafe {
                let _ = GetClientRect(hwnd, &mut r);
            }
            let h = (r.bottom - r.top).max(100);
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    0,
                    0,
                    PANEL_WIDTH,
                    h,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            let mut r2 = RECT::default();
            unsafe {
                let _ = GetClientRect(hwnd, &mut r2);
            }
            log(&format!("panel width now {}x{}", r2.right - r2.left, r2.bottom - r2.top));
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// ---------------------------------------------------------------------------
// Panel creation
// ---------------------------------------------------------------------------

struct PanelWindow(isize);

impl HasWindowHandle for PanelWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let hwnd = std::num::NonZeroIsize::new(self.0).ok_or(HandleError::Unavailable)?;
        let raw = RawWindowHandle::Win32(Win32WindowHandle::new(hwnd));
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

fn mime_for(path: &str) -> &'static str {
    if path.ends_with(".js") {
        "text/javascript; charset=utf-8"
    } else if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if path.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".json") {
        "application/json"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".png") {
        "image/png"
    } else {
        "application/octet-stream"
    }
}

fn serve_ui(
    dir: &std::path::Path,
    request: wry::http::Request<Vec<u8>>,
) -> wry::http::Response<std::borrow::Cow<'static, [u8]>> {
    let raw = request.uri().path().to_string();
    let rel = raw.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let full = dir.join(rel);
    match std::fs::read(&full) {
        Ok(bytes) => wry::http::Response::builder()
            .header("Content-Type", mime_for(rel))
            .body(std::borrow::Cow::Owned(bytes))
            .unwrap(),
        Err(_) => wry::http::Response::builder()
            .status(404)
            .body(std::borrow::Cow::Owned(Vec::new()))
            .unwrap(),
    }
}

fn ui_index_path() -> PathBuf {
    if let Ok(dir) = std::env::var("NPP_ACP_UI_DIR") {
        return PathBuf::from(dir).join("index.html");
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("plugins").join("npp-acp").join("ui").join("index.html")))
        .unwrap_or_else(|| PathBuf::from("ui\\index.html"))
}

fn create_panel() {
    log("create_panel called");
    let existing = panel_hwnd();
    if PANEL_HWND.load(Ordering::Relaxed) != 0 {
        unsafe {
            let _ = SendMessageW(npp_hwnd(), NPPM_DMMSHOW, Some(WPARAM(0)), Some(LPARAM(existing.0 as isize)));
        }
        start_acp();
        return;
    }

    let class_name = to_wide("npp_acp_panel");
    let title = to_wide("Agent ACP");
    let module = match unsafe { GetModuleHandleW(None) } {
        Ok(m) => m,
        Err(_) => return,
    };
    let hinstance = windows::Win32::Foundation::HINSTANCE(module.0);

    let wc = WNDCLASSW {
        style: windows::Win32::UI::WindowsAndMessaging::WNDCLASS_STYLES(0),
        lpfnWndProc: Some(wndproc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinstance,
        hIcon: windows::Win32::UI::WindowsAndMessaging::HICON::default(),
        hCursor: windows::Win32::UI::WindowsAndMessaging::HCURSOR::default(),
        hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH::default(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
    };
    unsafe {
        RegisterClassW(&wc);
    }

    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_CHILD | WS_VISIBLE,
            0,
            0,
            414,
            600,
            Some(npp_hwnd()),
            Some(windows::Win32::UI::WindowsAndMessaging::HMENU::default()),
            Some(hinstance),
            None,
        )
    };
    let hwnd = match hwnd {
        Ok(h) => h,
        Err(err) => {
            eprintln!("[npp-acp] CreateWindowExW failed: {err}");
            return;
        }
    };
    PANEL_HWND.store(hwnd.0 as isize, Ordering::Relaxed);

    // Register as a dockable right-side panel.
    let name_ptr = leak_wide("Agent ACP");
    let module_ptr = leak_wide("npp-acp.dll");
    let addinfo_ptr = leak_wide("");
    let mut data = DockedWidgetData {
        hclient: hwnd.0 as isize,
        psz_name: name_ptr,
        dlg_id: 0,
        u_mask: DWS_DF_CONT_RIGHT,
        hicon_tab: std::ptr::null_mut(),
        psz_add_info: addinfo_ptr,
        rc_float: Rect::default(),
        i_prev_cont: 0,
        psz_module_name: module_ptr,
    };
    unsafe {
        SendMessageW(
            npp_hwnd(),
            NPPM_DMMREGASDCKDLG,
            Some(WPARAM(0)),
            Some(LPARAM(&mut data as *mut DockedWidgetData as isize)),
        );
        SendMessageW(
            npp_hwnd(),
            NPPM_MODELESSDIALOG,
            Some(WPARAM(MODELESSDIALOGADD)),
            Some(LPARAM(hwnd.0 as isize)),
        );
        let _ = ShowWindow(hwnd, SW_SHOW);
    }

    // Docking manager lays out the panel asynchronously; force the preferred
    // width shortly after creation (plugin-side width, not the web viewport).
    let hwnd_addr = hwnd.0 as isize;
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(600));
        unsafe {
            let _ = PostMessageW(
                Some(HWND(hwnd_addr as *mut core::ffi::c_void)),
                WM_APP_SET_WIDTH,
                WPARAM(0),
                LPARAM(0),
            );
        }
    });

    // Create the embedded WebView2.
    let mut client_rect = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client_rect);
    }
    let width = (client_rect.right - client_rect.left).max(600) as u32;
    let height = (client_rect.bottom - client_rect.top).max(400) as u32;
    let bounds = wry::Rect {
        position: wry::dpi::Position::Physical(wry::dpi::PhysicalPosition::new(0, 0)),
        size: wry::dpi::Size::Physical(wry::dpi::PhysicalSize::new(width, height)),
    };

    let ui_path = ui_index_path();
    let ui_dir = ui_path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let url = "http://nppacp.localhost/index.html".to_string();
    let window = PanelWindow(hwnd.0 as isize);

    let serve_dir = ui_dir.clone();
    let builder = wry::WebViewBuilder::new()
        .with_url(&url)
        .with_devtools(true)
        .with_initialization_script(
            "window.addEventListener('error',function(e){try{window.ipc&&window.ipc.postMessage(JSON.stringify({type:'jserror',message:String(e.error&&e.error.stack||e.message)}))}catch(_){}});",
        )
        .with_custom_protocol("nppacp".into(), move |_id, request| {
            serve_ui(&serve_dir, request)
        })
        .with_on_page_load_handler(|_event, url| {
            log(&format!("page load {url}"));
        })
        .with_bounds(bounds)
        .with_ipc_handler(|request| {
            handle_ipc(request.body());
        });

    match builder.build_as_child(&window) {
        Ok(webview) => {
            WEBVIEW.with(|cell| *cell.borrow_mut() = Some(webview));
            log("webview created ok");
            send_theme();
            ACP_STARTED.store(0, Ordering::SeqCst);
            start_acp();
        }
        Err(err) => log(&format!("webview build failed: {err}")),
    }
}

// ---------------------------------------------------------------------------
// Plugin commands
// ---------------------------------------------------------------------------

fn funcs_ptr() -> *mut FuncItem {
    let addr = *FUNCS.get_or_init(|| {
        let mut item = FuncItem::default();
        let name = to_wide("Agent ACP");
        let len = name.len().min(64);
        item.name[..len].copy_from_slice(&name[..len]);
        item.pfunc = Some(cmd_open);
        item.cmd_id = 0;
        let boxed = Box::new([item]);
        Box::into_raw(boxed) as usize
    });
    addr as *mut FuncItem
}

extern "system" fn cmd_open() {
    create_panel();
}

// ---------------------------------------------------------------------------
// Exported plugin ABI
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "system" fn setInfo(npp_data: NppData) {
    NPP_HANDLE.store(npp_data.npp, Ordering::SeqCst);
    SCINTILLA_MAIN.store(npp_data.scintilla_main, Ordering::SeqCst);
    log(&format!(
        "setInfo npp={:#x} scintilla={:#x}",
        npp_data.npp, npp_data.scintilla_main
    ));

    // WebView2 needs a writable user-data folder; never write next to notepad++.exe.
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let dir = std::path::Path::new(&local).join("npp-acp").join("webview2");
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", dir);
    }
    // WebView2 requires an STA apartment on this (UI) thread.
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        );
    }
}

#[no_mangle]
pub extern "system" fn getName() -> *const u16 {
    static NAME: OnceLock<Vec<u16>> = OnceLock::new();
    NAME.get_or_init(|| to_wide("Agent ACP")).as_ptr()
}

#[no_mangle]
pub extern "system" fn getFuncsArray(nb_f: *mut i32) -> *mut FuncItem {
    if !nb_f.is_null() {
        unsafe {
            *nb_f = 1;
        }
    }
    funcs_ptr()
}

#[no_mangle]
pub extern "system" fn beNotified(notification: *mut SCNotification) {
    if notification.is_null() {
        return;
    }
    let code = unsafe { (*notification).nmhdr.code };
    match code {
        NPPN_READY => {
            let dark = unsafe {
                SendMessageW(npp_hwnd(), NPPM_ISDARKMODEENABLED, Some(WPARAM(0)), Some(LPARAM(0)))
            }
            .0;
            THEME_DARK.store(dark, Ordering::Relaxed);
            log("NPPN_READY");
            if std::env::var("NPP_ACP_AUTOOPEN").is_ok() {
                create_panel();
            }
        }
        NPPN_DARKMODECHANGED => {
            let dark = unsafe {
                SendMessageW(npp_hwnd(), NPPM_ISDARKMODEENABLED, Some(WPARAM(0)), Some(LPARAM(0)))
            }
            .0;
            THEME_DARK.store(dark, Ordering::Relaxed);
            send_theme();
        }
        NPPN_SHUTDOWN => {}
        _ => {}
    }
}

#[no_mangle]
pub extern "system" fn messageProc(_msg: u32, _wparam: usize, _lparam: isize) -> isize {
    1
}

#[no_mangle]
pub extern "system" fn isUnicode() -> i32 {
    1
}

