use chrono::{Timelike, Utc};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::config::{Config, ListenMode, PunchWindow};
use crate::model::{GcpPunchEvent, PunchEvent};
use crate::parser;
use crate::ui::{UiBus, UiEvent};

/// Live connection count per source IP, shared with the GUI.
pub type DeviceTable = Arc<Mutex<HashMap<String, u32>>>;

pub async fn run(
    cfg: Arc<RwLock<Config>>,
    tx: UnboundedSender<GcpPunchEvent>,
    ui: Option<UiBus>,
    devices: DeviceTable,
) -> anyhow::Result<()> {
    if cfg.read().await.listen.mode != ListenMode::Text {
        anyhow::bail!(
            "listen.mode=hex (two-way 8033) is not implemented yet; set listen.mode to `text`"
        );
    }

    let addr = get_listen_addr(&cfg).await;
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| anyhow::anyhow!("cannot bind {addr}: {e}"))?;
    info!(addr = %addr, "punch-clock TCP receiver listening");
    if let Some(ui) = &ui {
        ui.send(UiEvent::Info(format!("正在監聽 {addr}")));
    }

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                warn!(err = %e, "accept error");
                continue;
            }
        };
        let cfg = cfg.clone();
        let tx = tx.clone();
        let ui = ui.clone();
        let devices = devices.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(stream, peer, &cfg, tx, ui, devices).await {
                warn!(peer = %peer, err = %e, "connection handler error");
            }
        });
    }
}

async fn get_listen_addr(cfg: &Arc<RwLock<Config>>) -> String {
    let guard = cfg.read().await;
    format!("{}:{}", guard.listen.bind, guard.listen.port)
}

async fn handle_conn(
    stream: TcpStream,
    peer: SocketAddr,
    cfg: &Arc<RwLock<Config>>,
    tx: UnboundedSender<GcpPunchEvent>,
    ui: Option<UiBus>,
    devices: DeviceTable,
) -> anyhow::Result<()> {
    stream.set_nodelay(true).ok();
    info!(peer = %peer, "device connected");
    let ip = peer.ip().to_string();
    {
        let mut table = devices.lock().unwrap_or_else(|e| e.into_inner());
        *table.entry(ip.clone()).or_insert(0) += 1;
    }
    if let Some(ui) = &ui {
        ui.send(UiEvent::DeviceConnected(ip.clone()));
    }

    let reader = BufReader::new(stream);
    let mut lines = reader.lines();
    let mut count: u64 = 0;
    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let guard = cfg.read().await;
        let classify_enabled = guard.classify.enabled;
        let windows = guard.classify.windows.clone();
        let timezone_offset = guard.timezone_offset_seconds;
        let device = guard.device.clone();
        let receiver_id = guard.receiver_id.clone();
        drop(guard);

        match parser::parse_text_line(trimmed, timezone_offset) {
            Ok(punch) => {
                let mut punch = punch;
                if classify_enabled && !windows.is_empty() {
                    punch = classify_windows(punch, &windows);
                }
                let received_at = Utc::now().fixed_offset();
                let gcp = GcpPunchEvent::from_punch(
                    &punch,
                    &device,
                    &peer.ip().to_string(),
                    &receiver_id,
                    received_at,
                );
                count += 1;
                tx.send(gcp)
                    .map_err(|_| anyhow::anyhow!("delivery worker is gone"))?;
                info!(
                    peer = %peer,
                    node = punch.node_id,
                    event = %punch.event_code,
                    uid = %punch.uid_hex,
                    occurred_at = %punch.occurred_at.to_rfc3339(),
                    "punch event parsed"
                );
                if let Some(ui) = &ui {
                    ui.send(UiEvent::Punch {
                        time: punch.occurred_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                        uid: punch.uid_hex.clone(),
                        event: punch.event_code.clone(),
                        ip: peer.ip().to_string(),
                    });
                }
            }
            Err(e) => {
                warn!(peer = %peer, err = %e, raw = %truncate(trimmed, 160), "unparsable message");
            }
        }
    }
    {
        let mut table = devices.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = table.get_mut(&ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                table.remove(&ip);
            }
        }
    }
    if let Some(ui) = &ui {
        ui.send(UiEvent::DeviceDisconnected(ip));
    }
    info!(peer = %peer, count, "device disconnected");
    Ok(())
}

fn classify_windows(mut punch: PunchEvent, windows: &[PunchWindow]) -> PunchEvent {
    let minutes = punch.occurred_at.hour() as u32 * 60 + punch.occurred_at.minute() as u32;
    for w in windows {
        if window_contains(w, minutes) {
            punch.punch_type = w.kind.clone();
            return punch;
        }
    }
    punch
}

fn window_contains(w: &PunchWindow, minutes: u32) -> bool {
    let Some(from) = parse_hhmm(&w.from) else {
        return false;
    };
    let Some(to) = parse_hhmm(&w.to) else {
        return false;
    };
    if from <= to {
        minutes >= from && minutes < to
    } else {
        minutes >= from || minutes < to
    }
}

fn parse_hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.split_once(':')?;
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some(h * 60 + m)
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use crate::config::Config;

    fn punch_at(h: u32, m: u32) -> PunchEvent {
        let offset = chrono::FixedOffset::east_opt(28800).unwrap();
        PunchEvent {
            node_id: 1,
            sub_code: 17,
            function_code: 11,
            event_code: "M11".into(),
            description: "Normal Access".into(),
            door_no: Some(0),
            uid_hex: "00000000D4B81403".into(),
            uid_decimal: Some(0x0000_0000_D4B8_1403),
            username_raw: "rSammi".into(),
            username: "rSammi".into(),
            occurred_at: offset.with_ymd_and_hms(2021, 5, 12, h, m, 0).unwrap(),
            punch_type: "unknown".into(),
            duty_code: None,
            duty_label: None,
            raw: "".into(),
        }
    }

    #[test]
    fn classifies_by_window() {
        let cfg = Config::default();
        let morning = classify_windows(punch_at(8, 5), &cfg.classify.windows);
        assert_eq!(morning.punch_type, "check_in");
        let evening = classify_windows(punch_at(15, 30), &cfg.classify.windows);
        assert_eq!(evening.punch_type, "check_out");
    }
}