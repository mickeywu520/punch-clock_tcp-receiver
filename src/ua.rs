//! SOYAL µA (E-series) protocol client: active TCP polling of the punch clock
//! and RTC read/sync against the host PC clock.
//!
//! The card clock accepts our polling connection on its own command port
//! (default 1621) while it also pushes events to the message port (8031).
//! Commands used:
//!   * `24H` - read device real time clock
//!   * `23H` - set device real time clock (BCD: sec/min/hour/week/day/month/year)
//! Frame (standard): `7E <len> <node> <cmd> <data...> <xor> <sum>`

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Datelike, Local, Timelike};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::config::Config;
use crate::ui::{UiBus, UiEvent};

/// Commands the GUI can send to the clock-sync worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockSyncCmd {
    /// 立即以 PC 時間校準卡鐘 RTC（寫入 23H）
    SyncNow,
}

/// Parsed RTC reading (`24H` echo / function `0x03`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtcReading {
    pub sec: u32,
    pub min: u32,
    pub hour: u32,
    /// 1=Sunday..7=Saturday
    pub weekday: u32,
    pub day: u32,
    pub month: u32,
    pub year: u32,
}

impl RtcReading {
    /// The device RTC as a naive local date/time.
    fn naive(&self) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(self.year as i32, self.month, self.day)
            .and_then(|d| d.and_hms_opt(self.hour, self.min, self.sec))
            .unwrap_or_else(|| chrono::Utc::now().naive_utc())
    }

    fn fmt_local(&self) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.min, self.sec
        )
    }
}

fn bcd2dec(v: u8) -> u32 {
    ((v >> 4) * 10 + (v & 0x0f)) as u32
}

fn dec2bcd(d: u32) -> u8 {
    (((d / 10) << 4) | (d % 10)) as u8
}

/// Encodes one RTC field (0..=99) into a wire byte. Raw decimal unless BCD.
fn field_byte(value: u32, bcd: bool) -> u8 {
    if bcd {
        dec2bcd(value)
    } else {
        value as u8
    }
}

/// Decodes one RTC field byte back to a number. Raw decimal unless BCD.
fn field_value(byte: u8, bcd: bool) -> u32 {
    if bcd {
        bcd2dec(byte)
    } else {
        byte as u32
    }
}

/// Builds a standard µA frame: `7E len node cmd data... xor sum`.
/// Length counts everything after the length byte (node..sum inclusive).
fn encode_cmd(cmd: u8, data: &[u8]) -> Vec<u8> {
    let mut body = vec![0x01, cmd];
    body.extend_from_slice(data);
    let xor = body.iter().fold(0xFFu8, |a, &b| a ^ b);
    let sum = (body.iter().fold(0u16, |a, &b| a + b as u16) + xor as u16) & 0xFF;
    let mut out = vec![0x7E, (body.len() + 2) as u8];
    out.extend_from_slice(&body);
    out.push(xor);
    out.push(sum as u8);
    out
}

/// Reads one frame (`7E` header + length byte + payload). Time-bounded and
/// checksum-verified.
async fn read_frame(stream: &mut TcpStream, timeout: Duration) -> Result<Vec<u8>, String> {
    let mut hdr = [0u8; 2];
    read_exact(stream, &mut hdr, timeout).await?;
    if hdr[0] != 0x7E {
        return Err(format!("bad frame header 0x{:02X}", hdr[0]));
    }
    let len = hdr[1] as usize;
    if !(3..=1400).contains(&len) {
        return Err(format!("frame length out of range: {len}"));
    }
    let mut buf = vec![0u8; len];
    read_exact(stream, &mut buf, timeout).await?;

    let (payload, checksum) = buf.split_at(len - 2);
    let xor_calc = payload.iter().fold(0xFFu8, |a, &b| a ^ b);
    let sum_calc =
        (payload.iter().fold(0u16, |a, &b| a + b as u16) + xor_calc as u16) & 0xFF;
    if xor_calc != checksum[0] || (sum_calc as u8) != checksum[1] {
        return Err("frame checksum mismatch".to_string());
    }
    Ok(buf)
}

async fn read_exact(
    stream: &mut TcpStream,
    buf: &mut [u8],
    timeout: Duration,
) -> Result<(), String> {
    tokio::time::timeout(timeout, stream.read_exact(buf))
        .await
        .map_err(|_| "read timed out".to_string())?
        .map(|_| ())
        .map_err(|e| format!("read error: {e}"))
}

/// Reads the card clock RTC via `24H`.
async fn read_rtc(stream: &mut TcpStream, bcd: bool) -> Result<RtcReading, String> {
    let frame = encode_cmd(0x24, &[]);
    stream
        .write_all(&frame)
        .await
        .map_err(|e| format!("write error: {e}"))?;
    let body = read_frame(stream, Duration::from_secs(4)).await?;
    parse_reply_body(&body, bcd)
}

/// Parses a `24H` echo payload: `node, function(0x03), source, data[..], xor, sum`.
fn parse_reply_body(body: &[u8], bcd: bool) -> Result<RtcReading, String> {
    if body.len() < 10 || body[1] != 0x03 {
        return Err(format!(
            "unexpected 24H reply: func=0x{:02X} len={}",
            body.get(1).copied().unwrap_or(0),
            body.len()
        ));
    }
    Ok(RtcReading {
        sec: field_value(body[3], bcd),
        min: field_value(body[4], bcd),
        hour: field_value(body[5], bcd),
        weekday: body[6] as u32,
        day: field_value(body[7], bcd),
        month: field_value(body[8], bcd),
        year: 2000 + field_value(body[9], bcd),
    })
}

/// Sets the card clock RTC via `23H`. Raises on NACK / wrong echo.
async fn write_rtc(
    stream: &mut TcpStream,
    t: &chrono::NaiveDateTime,
    weekday: u32,
    bcd: bool,
) -> Result<(), String> {
    let data = [
        field_byte(t.second(), bcd),
        field_byte(t.minute(), bcd),
        field_byte(t.hour(), bcd),
        (weekday.clamp(1, 7)) as u8,
        field_byte(t.day(), bcd),
        field_byte(t.month(), bcd),
        field_byte((t.year() % 100) as u32, bcd),
    ];
    let frame = encode_cmd(0x23, &data);
    stream
        .write_all(&frame)
        .await
        .map_err(|e| format!("write error: {e}"))?;
    let body = read_frame(stream, Duration::from_secs(4)).await?;
    // body = node, function(echo), source, ...
    match body.get(1).copied() {
        Some(0x04) => Ok(()),
        Some(0x05) => Err("NACK from card clock (write rejected)".to_string()),
        Some(f) => Err(format!("unexpected 23H echo 0x{f:02X}")),
        None => Err("empty 23H echo".to_string()),
    }
}

/// The current wall-clock time on the host PC (naive local) plus SOYAL weekday
/// (1=Sunday..7=Saturday).
fn host_now() -> (chrono::NaiveDateTime, u32) {
    let now = Local::now();
    let weekday = now.weekday().num_days_from_sunday() + 1;
    (now.naive_local(), weekday)
}

fn target(cfg: &Config) -> Option<(String, u16)> {
    let ip = cfg.punch_clock.ip.clone()?;
    Some((ip, cfg.punch_clock.command_port))
}

/// Runs the clock-sync worker: keep one polling TCP session to the card clock,
/// periodically compare RTC to the host clock, auto-correct when drift exceeds
/// `clock_sync.max_drift_secs`, and honour manual `ClockSyncCmd::SyncNow`.
pub async fn run_clock_sync(
    cfg: Arc<RwLock<Config>>,
    ui: Option<UiBus>,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<ClockSyncCmd>,
) -> anyhow::Result<()> {
    let (ip, port) = loop {
        if let Some(t) = { let g = cfg.read().await; target(&g) } {
            break t;
        }
        // no IP configured yet: wait for a manual request or retry shortly
        match tokio::time::timeout(Duration::from_secs(5), cmd_rx.recv()).await {
            Ok(Some(ClockSyncCmd::SyncNow)) => {
                if let Some(ui) = &ui {
                    ui.send(UiEvent::Error("尚未設定卡鐘 IP，無法校時".to_string()));
                }
            }
            Ok(None) => return Ok(()),
            Err(_) => {}
        }
    };

    let addr: SocketAddr = match format!("{ip}:{port}").parse() {
        Ok(a) => a,
        Err(e) => {
            warn!(%ip, port, err = %e, "invalid punch clock address");
            return Ok(());
        }
    };

    info!(%addr, "clock sync worker starting");
    loop {
        let reconnect_secs = cfg.read().await.clock_sync.reconnect_secs.max(1);
        if let Err(e) = manage_session(&cfg, &ui, &mut cmd_rx, addr).await {
            warn!(%addr, err = %e, "clock sync session ended");
        }
        if let Some(ui) = &ui {
            ui.send(UiEvent::ClockStatus {
                online: false,
                rtc: None,
                note: Some(format!("輪詢中斷，{reconnect_secs} 秒後重連…")),
            });
        }
        // wait with backoff, but allow a manual re-trigger to shorten the wait
        match tokio::time::timeout(Duration::from_secs(reconnect_secs), cmd_rx.recv()).await {
            Ok(Some(ClockSyncCmd::SyncNow)) => continue,
            Ok(None) => return Ok(()),
            Err(_) => continue,
        }
    }
}

/// One TCP session to the card clock; returns on connection loss / protocol error.
async fn manage_session(
    cfg: &Arc<RwLock<Config>>,
    ui: &Option<UiBus>,
    cmd_rx: &mut tokio::sync::mpsc::UnboundedReceiver<ClockSyncCmd>,
    addr: SocketAddr,
) -> Result<(), String> {
    let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .map_err(|_| "connect timed out".to_string())?
        .map_err(|e| format!("connect failed: {e}"))?;
    stream.set_nodelay(true).ok();

    if let Some(ui) = ui {
        ui.send(UiEvent::ClockStatus {
            online: true,
            rtc: None,
            note: Some(format!("輪詢已連線 {addr}")),
        });
    }

    // initial sync right after connecting
    sync_once(cfg, ui, &mut stream).await?;

    loop {
        let (enabled, interval) = {
            let g = cfg.read().await;
            (
                g.clock_sync.enabled,
                g.clock_sync.interval_secs.max(10),
            )
        };
        if !enabled {
            // auto-sync disabled: idle until a manual request arrives or config flips
            closed_loop(cmd_rx).await?;
            continue;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(interval)) => {
                sync_once(cfg, ui, &mut stream).await?;
            }
            cmd = cmd_rx.recv() => match cmd {
                Some(ClockSyncCmd::SyncNow) => force_sync(cfg, ui, &mut stream).await?,
                None => return Ok(()),
            },
        }
    }
}

/// Holds the session when auto-sync is disabled; still honours manual sync.
async fn closed_loop(cmd_rx: &mut tokio::sync::mpsc::UnboundedReceiver<ClockSyncCmd>) -> Result<(), String> {
    match cmd_rx.recv().await {
        Some(ClockSyncCmd::SyncNow) => Ok(()),
        None => Err("clock sync channel closed".to_string()),
    }
}

/// Periodic check: read RTC, show it, auto-write when drift exceeds threshold.
async fn sync_once(
    cfg: &Arc<RwLock<Config>>,
    ui: &Option<UiBus>,
    stream: &mut TcpStream,
) -> Result<(), String> {
    if { let g = cfg.read().await; target(&g) }.is_none() {
        return Err("punch clock IP removed".to_string());
    }
    let (max_drift, bcd) = {
        let g = cfg.read().await;
        (g.clock_sync.max_drift_secs, g.clock_sync.bcd_encoding)
    };
    let rtc = read_rtc(stream, bcd).await?;
    let (now, _weekday) = host_now();
    let drift = rtc.naive().signed_duration_since(now).num_seconds();

    if drift.abs() > max_drift {
        warn!(
            rtc = %rtc.fmt_local(),
            drift_secs = drift,
            "card clock drift over threshold, auto-syncing from PC time"
        );
        let (now, weekday) = host_now();
        write_rtc(stream, &now, weekday, bcd).await?;
        let shown = read_rtc(stream, bcd).await.unwrap_or(rtc);
        if let Some(ui) = ui {
            ui.send(UiEvent::ClockStatus {
                online: true,
                rtc: Some(shown.fmt_local()),
                note: Some(format!(
                    "偏差 {} 秒，已自動同步 PC 時間 {}",
                    drift.abs(),
                    now.format("%Y-%m-%d %H:%M:%S")
                )),
            });
        }
    } else {
        if let Some(ui) = ui {
            ui.send(UiEvent::ClockStatus {
                online: true,
                rtc: Some(rtc.fmt_local()),
                note: None,
            });
        }
    }
    Ok(())
}

/// Manual sync triggered by the GUI button.
async fn force_sync(
    cfg: &Arc<RwLock<Config>>,
    ui: &Option<UiBus>,
    stream: &mut TcpStream,
) -> Result<(), String> {
    if { let g = cfg.read().await; target(&g) }.is_none() {
        return Err("punch clock IP removed".to_string());
    }
    let bcd = cfg.read().await.clock_sync.bcd_encoding;
    let (now, weekday) = host_now();
    write_rtc(stream, &now, weekday, bcd).await?;
    info!(time = %now.format("%Y-%m-%d %H:%M:%S"), "manual RTC sync written");
    let shown = read_rtc(stream, bcd).await.unwrap_or(RtcReading {
        sec: now.second(),
        min: now.minute(),
        hour: now.hour(),
        weekday,
        day: now.day(),
        month: now.month(),
        year: now.year() as u32,
    });
    if let Some(ui) = ui {
        ui.send(UiEvent::ClockStatus {
            online: true,
            rtc: Some(shown.fmt_local()),
            note: Some(format!("已手動校時，與 PC 同步 {}", now.format("%Y-%m-%d %H:%M:%S"))),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bcd_round_trips() {
        for v in 0..=59u32 {
            assert_eq!(bcd2dec(dec2bcd(v)), v, "bcd value {v}");
        }
    }

    #[test]
    fn field_byte_matches_encoding_mode() {
        assert_eq!(field_byte(41, false), 0x29); // raw decimal: 41
        assert_eq!(field_byte(41, true), 0x41); // BCD: 0b0100_0001
        assert_eq!(field_value(0x29, false), 41);
        assert_eq!(field_value(0x29, true), 29);
    }

    #[test]
    fn encodes_expected_24h_frame() {
        // doc example: 7E 04 01 24 DA FF
        let f = encode_cmd(0x24, &[]);
        assert_eq!(f, vec![0x7E, 0x04, 0x01, 0x24, 0xDA, 0xFF]);
    }

    #[test]
    fn encodes_expected_23h_frame_raw_decimal() {
        // 2026-09-25 17:41:12 Friday(6), raw-decimal encoding (this device).
        let f = encode_cmd(
            0x23,
            &[
                field_byte(12, false),
                field_byte(41, false),
                field_byte(17, false),
                6,
                field_byte(25, false),
                field_byte(9, false),
                field_byte(26, false),
            ],
        );
        // frame actually sent to the live device: 7E 0B 01 23 0C 29 11 06 19 09 1A E5 91
        let mut expected = vec![0x7E, 0x0B, 0x01, 0x23, 0x0C, 0x29, 0x11, 0x06, 0x19, 0x09, 0x1A];
        expected.push(0xE5); // XOR
        expected.push(0x91); // SUM
        assert_eq!(f, expected);
    }

    #[test]
    fn parses_24h_reply_raw_decimal() {
        // real capture: 7E 24 00 03 01 1E 29 11 06 19 09 1A 46 ...
        // -> 30s 41m 17h Fri 25th Sep 26 (raw-decimal fields)
        let mut body = vec![0x00, 0x03, 0x01, 0x1E, 0x29, 0x11, 0x06, 0x19, 0x09, 0x1A, 0x46];
        body.extend_from_slice(&[0x01, 0x02, 0x00, 0xC3]);
        body.extend_from_slice(&[0x00; 27]);
        // recompute checksum
        let xor = body.iter().fold(0xFFu8, |a, &b| a ^ b);
        let sum = (body.iter().fold(0u16, |a, &b| a + b as u16) + xor as u16) & 0xFF;
        body.push(xor);
        body.push(sum as u8);

        let r = parse_reply_body(&body, false).unwrap();
        assert_eq!(r.sec, 30);
        assert_eq!(r.min, 41);
        assert_eq!(r.hour, 17);
        assert_eq!(r.weekday, 6); // Friday
        assert_eq!(r.day, 25);
        assert_eq!(r.month, 9);
        assert_eq!(r.year, 2026);
    }
}