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

use iced::widget::{button, column, row, text, text_input};
use iced::{Alignment, Element, Font, Length, Size, Subscription, Task, Theme};

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
            initial: InitialSettings::default(),
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
        .default_font(default_font())
        .window_size(Size::new(780.0, 700.0))
        .resizable(true)
        .run()
}

/// The Traditional-Chinese font bundled with Windows. System fonts are loaded
/// automatically by iced 0.14, so this family always resolves on Windows.
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
        App {
            external: flags.config,
            cfg_path: flags.cfg_path,
            rx: flags.ui_rx,
            active_devices: flags.active_devices,
            listen_addr: flags.listen_addr,
            local_ips,
            edit_punch_clock_ip: flags.initial.punch_clock_ip,
            edit_port: flags.initial.command_port,
            edit_endpoint: flags.initial.endpoint,
            edit_api_key: flags.initial.api_key,
            status_line: flags.initial.status_line,
            test_status: None,
            last_events: Vec::new(),
            gcp_status: None,
        }
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
            Message::Tick => self.drain_events(),
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
                    UiEvent::Info(s) => {
                        self.status_line = s;
                    }
                    UiEvent::Error(e) => {
                        self.status_line = format!("錯誤：{e}");
                    }
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
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        iced::time::every(Duration::from_millis(300)).map(|_| Message::Tick)
    }

    fn view(&self) -> Element<'_, Message, Theme, iced::Renderer> {
        let local_ips = self.local_ips.iter().fold(
            column![].push(text("本機 IPv4（填入卡鐘後台 Message Server IP 1st）：").size(16)),
            |col, ip| col.push(text(ip.clone()).size(28)),
        );

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
                text_input("X-Api-Key 的值", &self.edit_api_key).on_input(Message::EditApiKey),
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
            .align_y(Alignment::Start)
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
    use iced_graphics::text::cosmic_text::{self, fontdb};

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