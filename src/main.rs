mod config;
mod forwarder;
mod function_codes;
mod model;
mod parser;
mod server;
mod ui;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::sync::RwLock;
use tokio::time::MissedTickBehavior;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use config::Config;
use forwarder::Delivery;
use model::GcpPunchEvent;
use ui::{UiBus, UiEvent};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // wgpu's DX12 backend segfaults (0xC0000005) during adapter enumeration on
    // multi-GPU machines (RTX + AMD + Basic Render Driver). Force the GL
    // backend by default on Windows unless the caller set WGPU_BACKEND.
    #[cfg(target_os = "windows")]
    if std::env::var_os("WGPU_BACKEND").is_none() {
        std::env::set_var("WGPU_BACKEND", "gl");
    }

    let cfg_path = std::env::args().nth(1);
    let cfg = config::load(cfg_path.as_deref())?;
    init_tracing(&cfg.log_level)?;
    info!(
        port = cfg.listen.port,
        mode = ?cfg.listen.mode,
        receiver_id = %cfg.receiver_id,
        ui_enabled = cfg.ui.enabled,
        "punch-clock tcp receiver starting"
    );

    // Single shared config: the desktop UI can change fields (punch clock IP,
    // GCP endpoint / API key) and they take effect without a restart.
    let shared = Arc::new(RwLock::new(cfg));
    let (tx, rx) = mpsc::unbounded_channel();

    let (ui_tx, ui_rx) = std::sync::mpsc::channel::<UiEvent>();
    let ui = UiBus::new(ui_tx);
    let devices: server::DeviceTable = Arc::new(Mutex::new(Default::default()));

    let server_task = {
        let shared = shared.clone();
        let ui = ui.clone();
        let devices = devices.clone();
        tokio::spawn(async move { server::run(shared, tx, Some(ui), devices).await })
    };

    let delivery = Arc::new(Delivery::new(shared.clone(), spool_dir_from(&shared).await).await?);
    let delivery_task = {
        let shared = shared.clone();
        let ui = ui.clone();
        tokio::spawn(run_delivery(delivery, shared, Some(ui), rx))
    };

    match ui_enabled_from(&shared).await {
        true => {
            info!("starting desktop UI");
            let initial = ui::InitialSettings {
                punch_clock_ip: shared.read().await.punch_clock.ip.clone().unwrap_or_default(),
                command_port: shared.read().await.punch_clock.command_port.to_string(),
                endpoint: shared.read().await.gcp.endpoint_url.clone().unwrap_or_default(),
                api_key: shared.read().await.gcp.api_key_value.clone().unwrap_or_default(),
                status_line: format!(
                    "就緒。本機監聽 {}:{}，GCP {}。",
                    shared.read().await.listen.bind,
                    shared.read().await.listen.port,
                    if shared.read().await.gcp.endpoint_url.is_some() {
                        "已設定"
                    } else {
                        "未設定"
                    }
                ),
            };
            let flags = ui::Flags {
                ui_rx,
                config: shared.clone(),
                cfg_path: cfg_path.map(std::path::PathBuf::from),
                active_devices: devices,
                listen_addr: listen_addr_from(&shared).await,
                initial,
            };
            ui::run(flags)?;
        }
        false => {
            shutdown_signal().await;
        }
    }

    info!("shutting down");
    server_task.abort();
    let _ = server_task.await;
    let _ = delivery_task.await;
    info!("shutdown complete");
    Ok(())
}

async fn spool_dir_from(shared: &Arc<RwLock<Config>>) -> std::path::PathBuf {
    shared.read().await.spool_dir.clone()
}

async fn ui_enabled_from(shared: &Arc<RwLock<Config>>) -> bool {
    shared.read().await.ui.enabled
}

async fn listen_addr_from(shared: &Arc<RwLock<Config>>) -> String {
    let g = shared.read().await;
    format!("{}:{}", g.listen.bind, g.listen.port)
}

fn init_tracing(level: &str) -> anyhow::Result<()> {
    let default = if level.is_empty() { "info" } else { level };
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default)))
        .with_target(false)
        .init();
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn run_delivery(
    delivery: Arc<Delivery>,
    shared: Arc<RwLock<Config>>,
    ui: Option<UiBus>,
    rx: UnboundedReceiver<GcpPunchEvent>,
) -> anyhow::Result<()> {
    let (batch_enabled, spool_dir) = {
        let g = shared.read().await;
        (g.gcp.batch_enabled, g.spool_dir.clone())
    };

    if delivery.configured().await {
        let (sent, kept) = delivery.replay_spool().await?;
        info!(sent, kept, "spool replay finished");
    } else {
        warn!(
            "gcp.endpoint_url is not configured; events will only be spooled to {}",
            spool_dir.display()
        );
        if let Some(ui) = &ui {
            ui.send(UiEvent::Error(
                "尚未設定 GCP Endpoint，刷卡事件僅會暫存到 spool".to_string(),
            ));
        }
    }

    if batch_enabled {
        run_batched(delivery, shared, ui, rx).await
    } else {
        run_immediate(delivery, ui, rx).await
    }
}

async fn run_immediate(
    delivery: Arc<Delivery>,
    ui: Option<UiBus>,
    mut rx: UnboundedReceiver<GcpPunchEvent>,
) -> anyhow::Result<()> {
    while let Some(ev) = rx.recv().await {
        deliver_batch(&delivery, &ui, &[ev]).await;
    }
    Ok(())
}

async fn run_batched(
    delivery: Arc<Delivery>,
    shared: Arc<RwLock<Config>>,
    ui: Option<UiBus>,
    mut rx: UnboundedReceiver<GcpPunchEvent>,
) -> anyhow::Result<()> {
    let (flush_secs, max_items) = {
        let g = shared.read().await;
        (
            g.gcp.batch_flush_interval_secs.max(1) as u64,
            g.gcp.batch_max_items.max(1),
        )
    };
    let mut buffer: Vec<GcpPunchEvent> = Vec::new();
    let mut tick = tokio::time::interval(Duration::from_secs(flush_secs));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Some(ev) => {
                    buffer.push(ev);
                    if buffer.len() >= max_items {
                        deliver_batch(&delivery, &ui, &buffer).await;
                        buffer.clear();
                    }
                }
                None => {
                    if !buffer.is_empty() {
                        deliver_batch(&delivery, &ui, &buffer).await;
                    }
                    return Ok(());
                }
            },
            _ = tick.tick() => {
                if !buffer.is_empty() {
                    deliver_batch(&delivery, &ui, &buffer).await;
                    buffer.clear();
                }
            }
        }
    }
}

async fn deliver_batch(delivery: &Delivery, ui: &Option<UiBus>, events: &[GcpPunchEvent]) {
    if events.is_empty() {
        return;
    }
    let n = events.len();
    match delivery.deliver(events).await {
        Ok(sent) => {
            info!(n, "events delivered to gcp");
            if let Some(ui) = ui {
                ui.send(UiEvent::GcpStatus {
                    ok: true,
                    count: sent as usize,
                    detail: "success".to_string(),
                });
            }
        }
        Err(msg) => {
            warn!(n, %msg, "delivery failed, spooling events");
            if let Some(ui) = ui {
                ui.send(UiEvent::GcpStatus {
                    ok: false,
                    count: 0,
                    detail: msg.clone(),
                });
            }
            if let Err(e) = delivery.spool(events) {
                error!(err = %e, n, "cannot spool events");
            }
        }
    }
}