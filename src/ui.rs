//! Desktop UI (iced) for the punch-clock intermediary.
//!
//! Shows the local IPv4 addresses (so the installer can enter them into the
//! punch clock backend), lets the operator set the punch clock IP and the GCP
//! endpoint / API key, tests the punch clock connectivity, and shows live
//! events / forwarding status fed from the tokio runtime via channels.

use std::collections::HashMap;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iced::widget::{button, column, row, text, text_input};
use iced::{Alignment, Application, Command, Element, Length, Settings, Subscription, Theme, Size};

use crate::config::Config;

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

pub struct Flags {
    pub ui_rx: std::sync::mpsc::Receiver<UiEvent>,
    pub config: Arc<tokio::sync::RwLock<Config>>,
    pub cfg_path: Option<PathBuf>,
    pub active_devices: Arc<Mutex<HashMap<String, u32>>>,
    pub listen_addr: String,
}

impl Default for Flags {
    fn default() -> Self {
        let (_tx, rx) = std::sync::mpsc::channel::<UiEvent>();
        Self {
            ui_rx: rx,
            config: Arc::new(tokio::sync::RwLock::new(Config::default())),
            cfg_path: None,
            active_devices: Arc::new(Mutex::new(HashMap::new())),
            listen_addr: String::new(),
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
}

struct App {
    external: Arc<tokio::sync::RwLock<Config>>,
    cfg_path: Option<PathBuf>,
    rx: std::sync::mpsc::Receiver<UiEvent>,
    active_devices: Arc<Mutex<HashMap<String, u32>>>,
    listen_addr: String,

    local_ips: Vec<String>,
    edit_punch_clock_ip: String,
    edit_port: String,
    edit_endpoint: String,
    edit_api_key: String,

    status_line: String,
    test_status: Option<String>,
    last_events: Vec<(String, String, String, String)>, // time, uid, event, ip
    gcp_status: Option<(bool, String)>,
}

pub fn run(flags: Flags) -> iced::Result {
    let settings = Settings {
        flags,
        window: iced::window::Settings {
            size: Size::new(780.0, 700.0),
            resizable: true,
            ..Default::default()
        },
        ..Default::default()
    };
    App::run(settings)
}

impl App {
    fn drain_events(&mut self) {
        let events: Vec<UiEvent> = self.rx.try_iter().collect();
        for ev in events {
            let _ = self.update(Message::Ui(ev));
        }
    }
}

impl Application for App {
    type Executor = iced::executor::Default;
    type Message = Message;
    type Theme = Theme;
    type Flags = Flags;

    fn new(flags: Self::Flags) -> (Self, Command<Message>) {
        let initial = {
            let listen = flags.config.blocking_read();
            (
                listen.punch_clock.ip.clone().unwrap_or_default(),
                listen.punch_clock.command_port.to_string(),
                listen.gcp.endpoint_url.clone().unwrap_or_default(),
                listen.gcp.api_key_value.clone().unwrap_or_default(),
                format!(
                    "就緒。本機監聽 {}:{}，GCP {}。",
                    listen.listen.bind,
                    listen.listen.port,
                    if listen.gcp.endpoint_url.is_some() {
                        "已設定"
                    } else {
                        "未設定"
                    }
                ),
            )
        };
        let local_ips = local_ipv4();
        let listen_addr = flags.listen_addr.clone();
        let mut app = App {
            external: flags.config,
            cfg_path: flags.cfg_path,
            rx: flags.ui_rx,
            active_devices: flags.active_devices,
            listen_addr,
            local_ips,
            edit_punch_clock_ip: String::new(),
            edit_port: String::new(),
            edit_endpoint: String::new(),
            edit_api_key: String::new(),
            status_line: initial.4,
            test_status: None,
            last_events: Vec::new(),
            gcp_status: None,
        };
        app.edit_punch_clock_ip = initial.0;
        app.edit_port = initial.1;
        app.edit_endpoint = initial.2;
        app.edit_api_key = initial.3;
        (app, Command::none())
    }

    fn title(&self) -> String {
        "打卡機中轉程式 (Punch Clock Receiver)".to_string()
    }

    fn update(&mut self, message: Message) -> Command<Message> {
        match message {
            Message::Tick => {
                self.drain_events();
                Command::none()
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
                        self.last_events.insert(
                            0,
                            (time, uid, event, ip),
                        );
                        self.last_events.truncate(200);
                    }
                    UiEvent::GcpStatus { ok, count, detail } => {
                        self.gcp_status = Some((ok, format!("{count} 筆 {detail}")));
                    }
                    UiEvent::Info(s) => {
                        self.status_line = s;
                    }
                    UiEvent::Error(e) => {
                        self.status_line = format!("錯誤：{e}");
                    }
                }
                Command::none()
            }
            Message::EditPunchClockIp(s) => {
                self.edit_punch_clock_ip = s;
                Command::none()
            }
            Message::EditPort(s) => {
                self.edit_port = s;
                Command::none()
            }
            Message::EditEndpoint(s) => {
                self.edit_endpoint = s;
                Command::none()
            }
            Message::EditApiKey(s) => {
                self.edit_api_key = s;
                Command::none()
            }
            Message::Save => {
                let cfg_path = self.cfg_path.clone();
                let external = self.external.clone();
                let punch_ip = self.edit_punch_clock_ip.trim().to_string();
                let port = self.edit_port.trim().to_string();
                let endpoint = self.edit_endpoint.trim().to_string();
                let api_key = self.edit_api_key.trim().to_string();
                Command::perform(
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
                Command::none()
            }
            Message::TestConnect => {
                let ip = self.edit_punch_clock_ip.trim().to_string();
                let port = self
                    .edit_port
                    .trim()
                    .parse::<u16>()
                    .unwrap_or(1621);
                Command::perform(
                    async move { test_tcp(ip, port).await },
                    Message::TestResult,
                )
            }
            Message::TestResult(res) => {
                self.test_status = Some(match res {
                    Ok(m) => m,
                    Err(e) => e,
                });
                Command::none()
            }
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        iced::time::every(Duration::from_millis(300)).map(|_| Message::Tick)
    }

    fn view(&self) -> Element<'_, Message, Theme, iced::Renderer> {
        let local_ips = self
            .local_ips
            .iter()
            .fold(column![].push(text("本機 IPv4（填入卡鐘後台 Message Server IP 1st）：").size(16)), |col, ip| {
                col.push(text(ip.clone()).size(28))
            });

        let connected = {
            let set = self
                .active_devices
                .lock()
                .map(|m| m.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            if set.is_empty() {
                "（目前無卡鐘連線）".to_string()
            } else {
                set.join(", ")
            }
        };

        let gcp_line = match &self.gcp_status {
            Some((true, d)) => format!("GCP 送達成功：{d}"),
            Some((false, d)) => format!("GCP 送達失敗：{d}"),
            None => "尚未送出任何事件".to_string(),
        };

        // Settings panel
        let settings = column![
            text("卡鐘設定").size(18),
            row![
                text("卡鐘 IP      "),
                text_input("如 192.168.1.127", &self.edit_punch_clock_ip)
                    .on_input(Message::EditPunchClockIp),
            ]
            .spacing(8),
            row![
                text("指令埠        "),
                text_input("1621", &self.edit_port).on_input(Message::EditPort),
                button("測試連線").on_press(Message::TestConnect),
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
                text_input("X-Api-Key 的值", &self.edit_api_key)
                    .on_input(Message::EditApiKey),
            ]
            .spacing(8),
            button("儲存設定（寫入 config.json）").on_press(Message::Save),
        ]
        .spacing(8);

        let status = column![
            text(&self.status_line).size(14),
            text(format!("監聽：{}", self.listen_addr)).size(13),
            text(format!("已連線卡鐘：{connected}")).size(13),
            text(gcp_line).size(13),
        ]
        .spacing(4);

        // Recent events
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

        column![
            row![
                column![
                    text("打卡機中轉程式").size(22),
                    text("等待卡鐘連線 → 轉拋到 GCP").size(14),
                ]
                .spacing(4)
                .width(Length::Fill),
                settings.width(Length::Shrink),
            ]
            .align_items(Alignment::Start)
            .spacing(24),
            local_ips,
            status,
            events,
        ]
        .spacing(12)
        .padding(16)
        .into()
    }
}

// ---------------------------------------------------------------------------
// Helpers used by async Commands
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
        Ok(_) => Ok(format!("✅ {addr} 連線成功（卡鐘應已連上網路後台）")),
        Err(e) => Err(format!("❌ {addr} 連線失敗：{e}")),
    }
}