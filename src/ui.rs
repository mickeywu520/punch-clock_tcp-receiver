//! Desktop UI (iced) for the punch-clock intermediary.
//!
//! Shows the local IPv4 addresses (so the installer can enter them into the
//! punch clock backend), lets the operator set the punch clock IP and the GCP
//! endpoint / API key, tests the punch clock connectivity, and shows live
//! events / forwarding status fed from the tokio runtime via channels.
//!
//! Iced 0.13+ loads the OS font set at startup (cosmic-text calls
//! `fontdb::load_system_fonts`), so Chinese text renders out of the box; we
//! only pin the default font to the Traditional-Chinese face on Windows.

use std::collections::HashMap;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iced::widget::{button, column, pick_list, row, scrollable, text, text_input};
use iced::window;
use iced::{Alignment, Element, Font, Length, Size, Subscription, Task, Theme};

use crate::config::Config;
use crate::punch_writer;
use crate::tray;
use tracing::info;

// ---------------------------------------------------------------------------
// Event bus: tokio runtime (server/delivery) -> UI thread
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum UiEvent {
    DeviceConnected(String),
    DeviceDisconnected(String),
    Punch {
        time: String,
        uid: String,
        event: String,
        ip: String,
    },
    GcpStatus {
        ok: bool,
        count: usize,
        detail: String,
    },
    /// 卡鐘 RTC 輪詢狀態（由 ua worker 送出）
    ClockStatus {
        online: bool,
        rtc: Option<String>,
        note: Option<String>,
    },
    Info(String),
    Error(String),
}

/// Cloneable non-blocking sender into the UI thread.
#[derive(Clone)]
pub struct UiBus {
    tx: std::sync::mpsc::Sender<UiEvent>,
}

impl UiBus {
    pub fn new(tx: std::sync::mpsc::Sender<UiEvent>) -> Self {
        Self { tx }
    }

    pub fn send(&self, ev: UiEvent) {
        let _ = self.tx.send(ev);
    }
}

// ---------------------------------------------------------------------------
// Local IPv4 helpers
// ---------------------------------------------------------------------------

pub fn local_ipv4() -> Vec<String> {
    let Ok(addrs) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut ips: Vec<String> = addrs
        .into_iter()
        .filter(|a| !a.is_loopback() && a.ip().is_ipv4())
        .map(|a| a.ip().to_string())
        .collect();
    ips.sort();
    ips.dedup();
    ips
}

// ---------------------------------------------------------------------------
// iced Application
// ---------------------------------------------------------------------------

/// Values the UI should start with (read once by `main` inside the runtime).
#[derive(Debug, Clone, Default)]
pub struct InitialSettings {
    pub punch_clock_ip: String,
    pub command_port: String,
    pub endpoint: String,
    pub api_key: String,
    pub status_line: String,
}

pub struct Flags {
    pub ui_rx: std::sync::mpsc::Receiver<UiEvent>,
    pub config: Arc<tokio::sync::RwLock<Config>>,
    pub cfg_path: Option<PathBuf>,
    pub active_devices: Arc<Mutex<HashMap<String, u32>>>,
    pub listen_addr: String,
    pub initial: InitialSettings,
    /// 卡鐘校時 worker 的指令端（GUI「卡鐘校時」按鈕使用）
    pub clock_sync_tx: tokio::sync::mpsc::UnboundedSender<crate::ua::ClockSyncCmd>,
}

impl Default for Flags {
    fn default() -> Self {
        let (_tx, rx) = std::sync::mpsc::channel::<UiEvent>();
        let (sync_tx, _sync_rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            ui_rx: rx,
            config: Arc::new(tokio::sync::RwLock::new(Config::default())),
            cfg_path: None,
            active_devices: Arc::new(Mutex::new(HashMap::new())),
            listen_addr: String::new(),
            initial: InitialSettings::default(),
            clock_sync_tx: sync_tx,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    Ui(UiEvent),
    EditPunchClockIp(String),
    EditPort(String),
    EditEndpoint(String),
    EditApiKey(String),
    Save,
    SaveResult(Result<(), String>),
    TestConnect,
    TestResult(Result<String, String>),
    ClockSyncNow,
    // 工作匣（tray）
    WindowEvent((window::Id, window::Event)),
    // 頁籤與人員管理
    TabSelected(Tab),
    EditCard(String),
    EditName(String),
    EditAddr(String),
    AccessModeSelected(String),
    AddPerson,
    AddPersonDone(Result<Vec<punch_writer::WriteOutcome>, String>),
    ImportCsv,
    ImportFileChosen(Option<PathBuf>),
    ImportDone(Result<Vec<punch_writer::WriteOutcome>, String>),
    // 取得卡鐘已註冊人員 → 匯出 CSV
    ExportUsers,
    ExportUsersResult(Result<Vec<punch_writer::ReadUser>, String>),
    ExportPathChosen(Option<PathBuf>),
    ExportDone(Result<(PathBuf, usize), String>),
}

/// 頂部頁籤
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Monitor,
    People,
    Settings,
}

/// 通行模式下拉選項（順序與 `punch_writer::AccessMode` 對應）
const MODE_OPTIONS: [&str; 3] = ["卡片驗證", "卡片或密碼", "卡片+密碼"];

struct App {
    external: Arc<tokio::sync::RwLock<Config>>,
    cfg_path: Option<PathBuf>,
    rx: std::sync::mpsc::Receiver<UiEvent>,
    active_devices: Arc<Mutex<HashMap<String, u32>>>,
    clock_sync_tx: tokio::sync::mpsc::UnboundedSender<crate::ua::ClockSyncCmd>,
    listen_addr: String,
    clock_ip: String,

    local_ips: Vec<String>,
    edit_punch_clock_ip: String,
    edit_port: String,
    edit_endpoint: String,
    edit_api_key: String,

    status_line: String,
    test_status: Option<String>,
    last_events: Vec<(String, String, String, String)>, // time, uid, event, ip
    gcp_status: Option<(bool, String)>,
    clock_online: bool,
    clock_rtc: String,

    // 頁籤與人員管理
    tab: Tab,
    edit_card: String,
    edit_name: String,
    edit_addr: String,
    access_mode: punch_writer::AccessMode,
    people_log: Vec<String>,
    people_busy: bool,
    pending_export: Vec<punch_writer::ReadUser>,

    // 工作匣／單一視窗
    tray: Option<tray::TrayHandle>,
    window_id: Option<window::Id>,
    force_quit: bool,
}

pub fn run(flags: Flags) -> iced::Result {
    let boot = {
        let flags = Arc::new(Mutex::new(Some(flags)));
        move || {
            let flags = flags
                .lock()
                .map(|mut f| f.take())
                .ok()
                .flatten()
                .expect("boot flags consumed once");
            (App::new(flags), Task::none())
        }
    };
    iced::application(boot, App::update, App::view)
        .title("打卡機中轉程式 (Punch Clock Receiver)")
        .subscription(App::subscription)
        .theme(Theme::Dark)
        // 方案 A：不讓 X 直接關閉程式，改由 `window::Event::CloseRequested`
        // 收進工作匣（tray）；真正離開走 tray 選單「離開程式」→ `window::close`
        .exit_on_close_request(false)
        .default_font(default_font())
        .window_size(Size::new(780.0, 700.0))
        .resizable(true)
        .run()
}

/// The Traditional-Chinese font bundled with Windows. System fonts are loaded
/// automatically by iced 0.14, so this family always resolves on Windows.
/// 目前安裝的程式版本（顯示於 UI 標題，追蹤客戶端版本用）。
fn current_version() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

fn default_font() -> Font {
    #[cfg(target_os = "windows")]
    {
        Font::with_name("Microsoft JhengHei")
    }
    #[cfg(not(target_os = "windows"))]
    {
        Font::DEFAULT
    }
}

impl App {
    fn new(flags: Flags) -> Self {
        let local_ips = local_ipv4();
        let mut app = App {
            external: flags.config,
            cfg_path: flags.cfg_path,
            rx: flags.ui_rx,
            active_devices: flags.active_devices,
            clock_sync_tx: flags.clock_sync_tx,
            listen_addr: flags.listen_addr,
            clock_ip: flags.initial.punch_clock_ip.clone(),
            local_ips,
            edit_punch_clock_ip: if flags.initial.punch_clock_ip.is_empty() {
                "192.168.1.127".to_string()
            } else {
                flags.initial.punch_clock_ip
            },
            edit_port: if flags.initial.command_port.is_empty() {
                "1621".to_string()
            } else {
                flags.initial.command_port
            },
            edit_endpoint: flags.initial.endpoint,
            edit_api_key: flags.initial.api_key,
            status_line: flags.initial.status_line,
            test_status: None,
            last_events: Vec::new(),
            gcp_status: None,
            clock_online: false,
            clock_rtc: "－".to_string(),
            tab: Tab::default(),
            edit_card: String::new(),
            edit_name: String::new(),
            edit_addr: String::new(),
            access_mode: punch_writer::AccessMode::default(),
            people_log: Vec::new(),
            people_busy: false,
            pending_export: Vec::new(),
            tray: None,
            window_id: None,
            force_quit: false,
        };
        // 工作匣圖示必須在有 win32 message pump 的執行緒建立（= iced 主執行緒）。
        match tray::build_tray() {
            Ok(t) => app.tray = Some(t),
            Err(e) => {
                app.status_line = format!("警告：工作匣啟動失敗（{e}），仍可使用視窗操作。")
            }
        }
        app
    }

    fn drain_events(&mut self) -> Task<Message> {
        let events: Vec<UiEvent> = self.rx.try_iter().collect();
        let mut task = Task::none();
        for ev in events {
            task = task.chain(self.update(Message::Ui(ev)));
        }
        task
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => {
                let mut task = self.drain_events();
                if let Some(cmd) = tray::poll_tray() {
                    task = task.chain(self.handle_tray(cmd));
                }
                task
            }
            Message::Ui(ev) => {
                match ev {
                    UiEvent::DeviceConnected(ip) => {
                        self.status_line = format!("卡鐘已連線：{ip}");
                    }
                    UiEvent::DeviceDisconnected(ip) => {
                        self.status_line = format!("卡鐘離線：{ip}");
                    }
                    UiEvent::Punch { time, uid, event, ip } => {
                        self.last_events.insert(0, (time, uid, event, ip));
                        self.last_events.truncate(200);
                    }
                    UiEvent::GcpStatus { ok, count, detail } => {
                        self.gcp_status = Some((ok, format!("{count} 筆 {detail}")));
                    }
                    UiEvent::ClockStatus { online, rtc, note } => {
                        self.clock_online = online;
                        if let Some(r) = rtc {
                            self.clock_rtc = r;
                        }
                        if let Some(n) = note {
                            self.status_line = format!("卡鐘：{n}");
                        }
                    }
                    UiEvent::Info(s) => {
                        self.status_line = s;
                    }
                    UiEvent::Error(e) => {
                        self.status_line = format!("錯誤：{e}");
                    }
                }
                Task::none()
            }
            Message::ClockSyncNow => {
                let ip = self.edit_punch_clock_ip.trim().to_string();
                let port = self.edit_port.trim().parse().unwrap_or(1621);
                if ip.is_empty() {
                    self.status_line = "錯誤：請先填寫卡鐘 IP".to_string();
                } else {
                    let _ = self.clock_sync_tx.send(crate::ua::ClockSyncCmd::SyncNow {
                        ip: ip.clone(),
                        port,
                    });
                    self.status_line = format!("已送出校時指令到 {ip}:{port}，等待卡鐘回應…");
                }
                Task::none()
            }
            Message::EditPunchClockIp(s) => {
                self.edit_punch_clock_ip = s;
                Task::none()
            }
            Message::EditPort(s) => {
                self.edit_port = s;
                Task::none()
            }
            Message::EditEndpoint(s) => {
                self.edit_endpoint = s;
                Task::none()
            }
            Message::EditApiKey(s) => {
                self.edit_api_key = s;
                Task::none()
            }
            Message::Save => {
                let cfg_path = self.cfg_path.clone();
                let external = self.external.clone();
                let punch_ip = self.edit_punch_clock_ip.trim().to_string();
                let port = self.edit_port.trim().to_string();
                let endpoint = self.edit_endpoint.trim().to_string();
                let api_key = self.edit_api_key.trim().to_string();
                Task::perform(
                    async move {
                        save_config(external, cfg_path, punch_ip, port, endpoint, api_key).await
                    },
                    Message::SaveResult,
                )
            }
            Message::SaveResult(res) => {
                self.status_line = match res {
                    Ok(()) => "已儲存，立即生效。".to_string(),
                    Err(e) => format!("儲存失敗：{e}"),
                };
                Task::none()
            }
            Message::TestConnect => {
                let ip = self.edit_punch_clock_ip.trim().to_string();
                let port = self
                    .edit_port
                    .trim()
                    .parse::<u16>()
                    .unwrap_or(1621);
                Task::perform(
                    async move { test_tcp(ip, port).await },
                    Message::TestResult,
                )
            }
            Message::TestResult(res) => {
                self.test_status = Some(match res {
                    Ok(m) => m,
                    Err(e) => e,
                });
                Task::none()
            }
            // -- 工作匣 --
            Message::WindowEvent((id, ev)) => match ev {
                window::Event::Opened { .. } => {
                    self.window_id = Some(id);
                    Task::none()
                }
                window::Event::CloseRequested => {
                    self.window_id = Some(id);
                    if self.force_quit {
                        // 已是 tray「離開程式」流程，直接放行關閉
                        window::close(id)
                    } else {
                        self.status_line =
                            "已收進工作匣：程式持續接收刷卡。可由工作匣圖示叫回主視窗。"
                                .to_string();
                        window::set_mode(id, window::Mode::Hidden)
                    }
                }
                _ => Task::none(),
            },
            // -- 頁籤 --
            Message::TabSelected(tab) => {
                self.tab = tab;
                Task::none()
            }
            // -- 人員管理 --
            Message::EditCard(s) => {
                self.edit_card = s;
                Task::none()
            }
            Message::EditName(s) => {
                self.edit_name = s;
                Task::none()
            }
            Message::EditAddr(s) => {
                self.edit_addr = s;
                Task::none()
            }
            Message::AccessModeSelected(label) => {
                self.access_mode = punch_writer::AccessMode::from_label(&label);
                Task::none()
            }
            Message::AddPerson => {
                let ip = self.edit_punch_clock_ip.trim().to_string();
                if ip.is_empty() {
                    self.status_line = "錯誤：請先在「設定」填寫卡鐘 IP".to_string();
                    return Task::none();
                }
                let port = self.edit_port.trim().parse().unwrap_or(1621);
                let card = self.edit_card.trim().to_string();
                if card.is_empty() {
                    self.status_line = "錯誤：請填卡片號".to_string();
                    return Task::none();
                }
                let addr = match self.addr_from_edit() {
                    Ok(a) => a,
                    Err(msg) => {
                        self.status_line = msg;
                        return Task::none();
                    }
                };
                let entry = punch_writer::PersonEntry {
                    card_spec: card,
                    name: if self.edit_name.trim().is_empty() {
                        None
                    } else {
                        Some(self.edit_name.trim().to_string())
                    },
                    addr,
                    mode: self.access_mode,
                };
                self.people_busy = true;
                self.status_line = format!("正在經由同步引擎新增人員到 {ip}:{port} …");
                let sync_tx = self.clock_sync_tx.clone();
                Task::perform(
                    async move {
                        Self::write_via_worker(&sync_tx, &ip, port, vec![entry]).await
                    },
                    Message::AddPersonDone,
                )
            }
            Message::AddPersonDone(res) => {
                self.people_busy = false;
                self.apply_write_results(res);
                Task::none()
            }
            Message::ImportCsv => Task::perform(
                async move {
                    let file = rfd::AsyncFileDialog::new()
                        .set_title("選擇批次匯入 CSV（一行一人）")
                        .add_filter("CSV / 文字檔", &["csv", "txt"])
                        .pick_file()
                        .await;
                    file.map(|f| f.path().to_path_buf())
                },
                Message::ImportFileChosen,
            ),
            Message::ImportFileChosen(Some(path)) => {
                let ip = self.edit_punch_clock_ip.trim().to_string();
                let port = self.edit_port.trim().parse().unwrap_or(1621);
                self.people_busy = true;
                self.status_line = format!("正在經由同步引擎讀取 {path:?} 並批次新增 …");
                let sync_tx = self.clock_sync_tx.clone();
                Task::perform(
                    async move {
                        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                        let entries = punch_writer::parse_csv(&bytes)?;
                        Self::write_via_worker(&sync_tx, &ip, port, entries).await
                    },
                    Message::ImportDone,
                )
            }
            Message::ImportFileChosen(None) => {
                self.status_line = "已取消匯入".to_string();
                Task::none()
            }
            Message::ImportDone(res) => {
                self.people_busy = false;
                self.apply_write_results(res);
                Task::none()
            }
            Message::ExportUsers => {
                let ip = self.edit_punch_clock_ip.trim().to_string();
                if ip.is_empty() {
                    self.status_line = "錯誤：請先在「設定」填寫卡鐘 IP".to_string();
                    self.push_people_log("✗ 匯出失敗：尚未填寫卡鐘 IP".to_string());
                    return Task::none();
                }
                let port = self.edit_port.trim().parse().unwrap_or(1621);
                self.people_busy = true;
                self.status_line = format!("正在從卡鐘回讀已註冊人員（{ip}:{port}）…");
                let sync_tx = self.clock_sync_tx.clone();
                Task::perform(
                    async move { Self::read_via_worker(&sync_tx, &ip, port).await },
                    Message::ExportUsersResult,
                )
            }
            Message::ExportUsersResult(Ok(users)) => {
                if users.is_empty() {
                    self.people_busy = false;
                    self.status_line = "卡鐘中沒有已註冊人員".to_string();
                    self.push_people_log("✓ 已回讀：卡鐘內沒有已註冊人員".to_string());
                    return Task::none();
                }
                self.pending_export = users;
                Task::perform(
                    async move {
                        rfd::AsyncFileDialog::new()
                            .set_title("匯出已註冊人員 CSV")
                            .set_file_name("cardclock_users.csv")
                            .add_filter("CSV", &["csv"])
                            .save_file()
                            .await
                            .map(|f| f.path().to_path_buf())
                    },
                    Message::ExportPathChosen,
                )
            }
            Message::ExportUsersResult(Err(msg)) => {
                self.people_busy = false;
                let line = format!("✗ 匯出失敗：{msg}");
                self.status_line = line.clone();
                self.push_people_log(line);
                Task::none()
            }
            Message::ExportPathChosen(Some(path)) => {
                let users = std::mem::take(&mut self.pending_export);
                let n = users.len();
                Task::perform(
                    async move {
                        std::fs::write(&path, punch_writer::users_csv(&users))
                            .map(|_| (path.clone(), n))
                            .map_err(|e| format!("寫入 {path:?} 失敗：{e}"))
                    },
                    Message::ExportDone,
                )
            }
            Message::ExportPathChosen(None) => {
                self.people_busy = false;
                self.status_line = "已取消匯出".to_string();
                Task::none()
            }
            Message::ExportDone(Ok((path, n))) => {
                self.people_busy = false;
                self.status_line = format!("已匯出 {n} 人到 {path:?}");
                self.push_people_log(format!("✓ 已匯出 {n} 人到 {path:?}"));
                Task::none()
            }
            Message::ExportDone(Err(msg)) => {
                self.people_busy = false;
                let line = format!("✗ 匯出失敗：{msg}");
                self.status_line = line.clone();
                self.push_people_log(line);
                Task::none()
            }
        }
    }

    /// tray 選單指令 → 視窗動作
    fn handle_tray(&mut self, cmd: tray::TrayCmd) -> Task<Message> {
        match cmd {
            tray::TrayCmd::Show => match self.window_id {
                Some(id) => {
                    self.status_line = "已顯示主視窗。".to_string();
                    window::set_mode(id, window::Mode::Windowed)
                        .chain(window::gain_focus(id))
                }
                None => Task::none(),
            },
            tray::TrayCmd::Quit => {
                // 直接終結行程：iced 在 `exit_on_close_request(false)` 下
                // `window::close` 不會結束 run loop，整個程序會留在背景，
                // 屆時重啟會被單一實例鎖擋住。`process::exit` 會立即釋放
                // 所有資源（含 16x1/1621 連線與單一實例鎖）。
                info!("user requested quit via tray menu");
                std::process::exit(0);
            }
        }
    }

    /// 把一次寫入的結果推到紀錄清單與狀態列
    fn apply_write_results(&mut self, res: Result<Vec<punch_writer::WriteOutcome>, String>) {
        match res {
            Ok(outcomes) => {
                let mut ok = 0usize;
                let mut fail = 0usize;
                let mut lines = Vec::new();
                for o in &outcomes {
                    if o.ok {
                        ok += 1;
                    } else {
                        fail += 1;
                    }
                    let name = o.name.as_deref().unwrap_or("（無姓名）");
                    lines.push(if o.ok {
                        format!("位址 {}  UID {}  {}  ✓ 成功", o.addr, o.uid_hex, name)
                    } else {
                        format!("位址 {}  UID {}  {}  ✗ 失敗：{}", o.addr, o.uid_hex, name, o.detail)
                    });
                }
                for line in lines {
                    self.push_people_log(line);
                }
                self.status_line = format!("寫入完成：成功 {ok} 筆 / 失敗 {fail} 筆");
            }
            Err(msg) => {
                self.status_line = format!("寫入失敗：{msg}");
                self.push_people_log(format!("✗ 寫入失敗：{msg}"));
            }
        }
    }

    fn push_people_log(&mut self, line: String) {
        self.people_log.insert(0, line);
        self.people_log.truncate(300);
    }

    /// 把人員寫入委託給 clock-sync worker：worker 先釋放自己的 1621 session 再執行，
/// 避免撞上 SOYAL 控制器「僅接受單一 master 連線」的規則（否則寫入連線會被即時斷開）。
async fn write_via_worker(
    sync_tx: &tokio::sync::mpsc::UnboundedSender<crate::ua::ClockSyncCmd>,
    ip: &str,
    port: u16,
    entries: Vec<punch_writer::PersonEntry>,
) -> Result<Vec<punch_writer::WriteOutcome>, String> {
    let (reply, rx) = tokio::sync::oneshot::channel();
    sync_tx
        .send(crate::ua::ClockSyncCmd::WritePeople {
            ip: ip.to_string(),
            port,
            entries,
            reply,
        })
        .map_err(|_| "同步 worker 已結束".to_string())?;
    tokio::time::timeout(Duration::from_secs(300), rx)
        .await
        .map_err(|_| "等待卡鐘寫入回應逾時（300 秒）".to_string())?
        .map_err(|_| "同步 worker 未回應寫入結果".to_string())?
}

/// 把全範圍回讀委託給 clock-sync worker（同樣先釋放 session，避免撞單一 master 規則）。
async fn read_via_worker(
    sync_tx: &tokio::sync::mpsc::UnboundedSender<crate::ua::ClockSyncCmd>,
    ip: &str,
    port: u16,
) -> Result<Vec<punch_writer::ReadUser>, String> {
    let (reply, rx) = tokio::sync::oneshot::channel();
    sync_tx
        .send(crate::ua::ClockSyncCmd::ReadUsers {
            ip: ip.to_string(),
            port,
            start: 1,
            end: 0x800,
            reply,
        })
        .map_err(|_| "同步 worker 已結束".to_string())?;
    tokio::time::timeout(Duration::from_secs(300), rx)
        .await
        .map_err(|_| "等待卡鐘回讀逾時（300 秒）".to_string())?
        .map_err(|_| "同步 worker 未回應回讀結果".to_string())?
}

/// 位址欄 → `None`(留空=自動) 或合法的 `Some(addr)`
    fn addr_from_edit(&self) -> Result<Option<u16>, String> {
        let s = self.edit_addr.trim();
        if s.is_empty() {
            return Ok(None);
        }
        match s.parse::<u16>() {
            Ok(a) => Ok(Some(a)),
            Err(_) => Err(format!("錯誤：人員位址需為 1~65535 的數字，收到 {s:?}")),
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let tick = iced::time::every(Duration::from_millis(300)).map(|_| Message::Tick);
        let win_events = window::events().map(|(id, ev)| Message::WindowEvent((id, ev)));
        Subscription::batch([tick, win_events])
    }

    fn view(&self) -> Element<'_, Message, Theme, iced::Renderer> {
        let header = row![
            column![
                text("打卡機中轉程式").size(22),
                text("等待卡鐘連線 → 轉拋到 GCP").size(14),
            ]
            .spacing(4),
            text("關閉視窗＝收進工作匣").size(12),
            text("").width(Length::Fill),
            text(current_version()).size(13),
        ]
        .align_y(Alignment::Center)
        .spacing(16);

        let tab_bar = row![
            self.tab_button("監控", Tab::Monitor),
            self.tab_button("人員管理", Tab::People),
            self.tab_button("設定", Tab::Settings),
        ]
        .spacing(8);

        let content = match self.tab {
            Tab::Monitor => self.monitor_view(),
            Tab::People => self.people_view(),
            Tab::Settings => self.settings_view(),
        };

        column![header, tab_bar, content]
            .spacing(12)
            .padding(16)
            .into()
    }

    fn tab_button(&self, label: &'static str, tab: Tab) -> Element<'_, Message, Theme, iced::Renderer> {
        let selected = self.tab == tab;
        let btn = button(text(label)).on_press(Message::TabSelected(tab));
        if selected {
            btn.style(button::primary).into()
        } else {
            btn.style(button::secondary).into()
        }
    }

    /// 監控頁：本機 IP／卡鐘狀態／最近刷卡
    fn monitor_view(&self) -> Element<'_, Message, Theme, iced::Renderer> {
        let local_ips = self.local_ips.iter().fold(
            column![].push(text("本機 IPv4：").size(16)),
            |col, ip| col.push(text(ip.clone()).size(28)),
        );

        let connected = {
            let mut parts: Vec<String> = Vec::new();
            if self.clock_online && !self.clock_ip.is_empty() {
                parts.push(format!("{}（RTC 輪詢）", self.clock_ip));
            }
            if let Ok(set) = self
                .active_devices
                .lock()
                .map(|m| m.keys().cloned().collect::<Vec<_>>())
            {
                for ip in set {
                    parts.push(format!("{ip}（訊息埠）"));
                }
            }
            if parts.is_empty() {
                "（目前無卡鐘連線）".to_string()
            } else {
                parts.join(", ")
            }
        };

        let gcp_line = match &self.gcp_status {
            Some((true, d)) => format!("GCP 送達成功：{d}"),
            Some((false, d)) => format!("GCP 送達失敗：{d}"),
            None => "尚未送出任何事件".to_string(),
        };

        let status = column![
            text(&self.status_line).size(14),
            text(format!("卡鐘 RTC：{}", self.clock_rtc)).size(13),
            text(format!("監聽：{}", self.listen_addr)).size(13),
            text(format!("已連線卡鐘：{connected}")).size(13),
            text(gcp_line).size(13),
        ]
        .spacing(4);

        let event_rows = self.last_events.iter().fold(
            column![],
            |col, (time, uid, event, ip)| {
                col.push(text(format!("{time}  {uid}  {event}  來自 {ip}")).size(13))
            },
        );
        let events = column![
            text("最近刷卡").size(18),
            if self.last_events.is_empty() {
                column![text("（尚無）")].push(text(""))
            } else {
                event_rows
            },
        ]
        .spacing(4);

        column![local_ips, status, events]
            .spacing(12)
            .into()
    }

    /// 人員管理頁：單筆新增＋批次匯入＋寫入紀錄
    fn people_view(&self) -> Element<'_, Message, Theme, iced::Renderer> {
        let form = column![
            text("新增人員").size(18),
            row![
                text("卡號    "),
                text_input("64867:29942 或 16碼HEX", &self.edit_card)
                    .on_input(Message::EditCard)
                    .width(Length::Fixed(260.0)),
            ]
            .spacing(8),
            row![
                text("姓名    "),
                text_input("選填，寫入卡鐘姓名（Big5，至多 8 字）", &self.edit_name)
                    .on_input(Message::EditName)
                    .width(Length::Fixed(260.0)),
            ]
            .spacing(8),
            row![
                text("位址    "),
                text_input("留空＝自動找下一個空位", &self.edit_addr)
                    .on_input(Message::EditAddr)
                    .width(Length::Fixed(120.0)),
            ]
            .spacing(8),
            row![
                text("通行模式"),
                pick_list(MODE_OPTIONS, Some(self.access_mode.label()), |s| {
                    Message::AccessModeSelected(s.to_string())
                })
                .width(Length::Shrink),
            ]
            .spacing(8),
            row![
                button("新增人員").on_press(Message::AddPerson),
                button("匯入 CSV（批次新增）").on_press(Message::ImportCsv),
                button("取得已註冊人員並匯出 CSV").on_press(Message::ExportUsers),
                if self.people_busy {
                    text("處理中…").size(13)
                } else {
                    text("").size(13)
                },
            ]
            .spacing(8),
            text("批次匯入格式：每行 ─ 卡號,姓名（姓名可省略）；卡號為 `site:card` 或 16碼 HEX；檔案支援 UTF-8 / Big5")
                .size(12),
        ]
        .spacing(8);

        let log_rows = self
            .people_log
            .iter()
            .fold(column![].spacing(2), |col, line| {
                col.push(text(line).size(13))
            });
        let log_area = scrollable(
            column![
                text("寫入紀錄").size(18),
                if self.people_log.is_empty() {
                    column![text("（尚無）").size(13)]
                } else {
                    log_rows
                },
            ]
            .spacing(4),
        )
        .height(Length::Fill);

        column![form, log_area].spacing(12).into()
    }

    /// 設定頁：卡鐘與 GCP 設定
    fn settings_view(&self) -> Element<'_, Message, Theme, iced::Renderer> {
        let settings = column![
            text("卡鐘設定").size(18),
            row![
                text("卡鐘 IP      "),
                text_input("192.168.1.127", &self.edit_punch_clock_ip)
                    .on_input(Message::EditPunchClockIp)
                    .width(Length::Fixed(150.0)),
            ]
            .spacing(8),
            row![
                text("指令埠        "),
                text_input("1621", &self.edit_port)
                    .on_input(Message::EditPort)
                    .width(Length::Fixed(70.0)),
                button("測試連線").on_press(Message::TestConnect),
                button("卡鐘校時").on_press(Message::ClockSyncNow),
            ]
            .spacing(8),
            match &self.test_status {
                Some(s) => text(s).size(13),
                None => text(""),
            },
            text("GCP 後台").size(18),
            row![
                text("Endpoint "),
                text_input("https://.../api/punch-events", &self.edit_endpoint)
                    .on_input(Message::EditEndpoint)
                    .width(Length::Fill),
            ]
            .spacing(8),
            row![
                text("API Key   "),
                text_input("X-Api-Key 的值", &self.edit_api_key).on_input(Message::EditApiKey),
            ]
            .spacing(8),
            button("儲存設定（寫入 config.json）").on_press(Message::Save),
        ]
        .spacing(8);

        column![
            text(&self.status_line).size(14),
            settings,
            text("在「人員管理」新增的人員是直接寫入卡鐘（84H 新增 + 2EH 姓名）。")
                .size(12),
        ]
        .spacing(12)
        .into()
    }
}

// ---------------------------------------------------------------------------
// Helpers used by async Tasks
// ---------------------------------------------------------------------------

async fn save_config(
    external: Arc<tokio::sync::RwLock<Config>>,
    cfg_path: Option<PathBuf>,
    punch_ip: String,
    port: String,
    endpoint: String,
    api_key: String,
) -> Result<(), String> {
    let mut cfg = external.write().await;

    cfg.punch_clock.ip = Some(punch_ip.clone());
    if let Ok(p) = port.parse::<u16>() {
        cfg.punch_clock.command_port = p;
    }
    cfg.gcp.endpoint_url = if endpoint.is_empty() {
        None
    } else {
        Some(endpoint.clone())
    };
    cfg.gcp.api_key_value = if api_key.is_empty() {
        None
    } else {
        Some(api_key.clone())
    };
    cfg.gcp.api_key_header = Some("X-Api-Key".to_string());
    drop(cfg);

    // persist to disk
    let cfg = external.read().await;
    let json = serde_json::to_string_pretty(&*cfg).map_err(|e| e.to_string())?;
    drop(cfg);

    let path = match &cfg_path {
        Some(p) => p.clone(),
        None => PathBuf::from("config.json"),
    };
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(())
}

async fn test_tcp(ip: String, port: u16) -> Result<String, String> {
    if ip.is_empty() {
        return Err("請先輸入卡鐘 IP".to_string());
    }
    let addr = format!("{ip}:{port}");
    match TcpStream::connect_timeout(
        &addr
            .parse::<std::net::SocketAddr>()
            .map_err(|e| e.to_string())?,
        Duration::from_secs(3),
    ) {
        Ok(_) => Ok(format!("{addr} 連線成功（卡鐘應已連上網路後台）")),
        Err(e) => Err(format!("{addr} 連線失敗：{e}")),
    }
}

// ---------------------------------------------------------------------------
// CJK font regression guard
// ---------------------------------------------------------------------------
//
// iced 0.14 renders text through cosmic-text, which loads the OS font set on
// startup. This test pins down, on this machine, that:
//   1. the system font set is actually loaded (Microsoft JhengHei appears), and
//   2. that face covers every CJK character used anywhere in the UI.
// If a future upgrade regresses the font loading, this test fails.

#[cfg(all(test, target_os = "windows"))]
mod ui_font_tests {
    use iced_graphics::text::cosmic_text;

    const CJK_SAMPLE: &str =
        "打卡機中轉程式等待卡鐘連線轉拋儲存設定已連線離線最近刷卡命令埠測試錯誤GCP送達成功尚無本機位址填入後台";

    fn face_covers_all(face: &ttf_parser::Face) -> bool {
        CJK_SAMPLE.chars().all(|c| face.glyph_index(c).is_some())
    }

    #[test]
    fn system_fonts_loaded_and_cover_ui_text() {
        let font_system = cosmic_text::FontSystem::new();
        let (_locale, db) = font_system.into_locale_and_db();

        let sample = std::fs::read_to_string("C:\\Windows\\Fonts\\msjh.ttc").unwrap_or_default();
        if db.faces().next().is_none() && sample.is_empty() {
            panic!("no system fonts loaded and msjh.ttc missing - system fonts not loaded");
        }

        let jhenghei: Vec<_> = db
            .faces()
            .filter(|f| {
                f.families
                    .iter()
                    .any(|(name, _)| name == "Microsoft JhengHei")
            })
            .map(|f| f.id)
            .collect();

        assert!(
            !jhenghei.is_empty(),
            "system font set did not load 'Microsoft JhengHei' (fonts ignored?)"
        );

        for id in jhenghei {
            if db
                .with_face_data(id, |data, index| {
                    ttf_parser::Face::parse(data, index)
                        .map(|face| face_covers_all(&face))
                        .unwrap_or(false)
                })
                .unwrap_or(false)
            {
                return;
            }
        }

        panic!(
            "no 'Microsoft JhengHei' face covers all CJK chars: {CJK_SAMPLE}"
        );
    }
}