//! SOYAL µA (E-series) protocol client: active TCP polling of the punch clock,
//! RTC read/sync against the host PC clock, and 25H/37H event-queue pull.
//!
//! The card clock accepts our polling connection on its own command port
//! (default 1621) while it also pushes events to the message port (8031).
//! On this unit (AR-821EFv5, firmware 4V6) the 8031 push only fires on
//! power-on/reboot, so real-time punches are fetched via the event-queue pull
//! instead (25H read one record -> 37H delete -> repeat until the queue is
//! empty). See PRD §2.9.
//! Commands used:
//!   * `24H` - read device real time clock
//!   * `23H` - set device real time clock (BCD: sec/min/hour/week/day/month/year)
//!   * `25H` - read next event-log record (top of queue)
//!   * `37H` - delete the record just read
//! Frame (standard): `7E <len> <node> <cmd> <data...> <xor> <sum>`

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Datelike, FixedOffset, Local, NaiveDate, TimeZone, Timelike, Utc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::config::Config;
use crate::dedup::PunchDedup;
use crate::function_codes;
use crate::model::{GcpPunchEvent, PunchEvent};
use crate::server::classify_windows;
use crate::ui::{UiBus, UiEvent};

/// Commands the GUI can send to the clock-sync worker.
#[derive(Debug)]
pub enum ClockSyncCmd {
    /// 立即以 PC 時間校準卡鐘 RTC。帶 GUI 輸入框的位址，手動校時不依賴 config。
    SyncNow { ip: String, port: u16 },
    /// GUI 新增/匯入人員：worker 先釋放自己的 1621 session（SOYAL 只接受單一連線），
    /// 再以一次性連線寫入；完成後把結果回傳 UI，然後恢復輪詢。
    WritePeople {
        ip: String,
        port: u16,
        entries: Vec<crate::punch_writer::PersonEntry>,
        reply: tokio::sync::oneshot::Sender<
            Result<Vec<crate::punch_writer::WriteOutcome>, String>,
        >,
    },
    /// GUI「取得已註冊人員」：worker 先釋放自己的 1621 session，再以一次性連線
    /// 87H 全範圍回讀已註冊人員；完成後回傳結果給 UI，然後恢復輪詢。
    ReadUsers {
        ip: String,
        port: u16,
        start: u16,
        end: u16,
        reply: tokio::sync::oneshot::Sender<Result<Vec<crate::punch_writer::ReadUser>, String>>,
    },
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
/// `clock_sync.max_drift_secs`, honour manual `ClockSyncCmd::SyncNow`, and pull
/// the 25H/37H event queue on a separate tick into the UI / GCP pipeline.
pub async fn run_clock_sync(
    cfg: Arc<RwLock<Config>>,
    ui: Option<UiBus>,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<ClockSyncCmd>,
    tx: UnboundedSender<GcpPunchEvent>,
    dedup: Arc<std::sync::Mutex<PunchDedup>>,
) -> anyhow::Result<()> {
    let (ip, port) = loop {
        if let Some(t) = { let g = cfg.read().await; target(&g) } {
            break t;
        }
        // No IP in config yet: keep waiting, but manual "卡鐘校時" must still work
        // using the address passed from the GUI input box.
        match tokio::time::timeout(Duration::from_secs(5), cmd_rx.recv()).await {
            Ok(Some(ClockSyncCmd::SyncNow { ip, port })) => {
                oneshot_sync(&cfg, &ui, &ip, port).await.ok();
            }
            Ok(Some(ClockSyncCmd::WritePeople { ip, port, entries, reply })) => {
                write_people_oneshot(&ip, port, entries, reply).await;
            }
            Ok(Some(ClockSyncCmd::ReadUsers { ip, port, start, end, reply })) => {
                read_users_oneshot(&ip, port, start, end, reply).await;
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
    let mut first_session = true;
    loop {
        let reconnect_secs = cfg.read().await.clock_sync.reconnect_secs.max(1);
        if let Err(e) = manage_session(
            &cfg,
            &ui,
            &mut cmd_rx,
            &tx,
            &dedup,
            addr,
            first_session,
        )
        .await
        {
            warn!(%addr, err = %e, "clock sync session ended");
        }
        first_session = false;
        if let Some(ui) = &ui {
            ui.send(UiEvent::ClockStatus {
                online: false,
                rtc: None,
                note: Some(format!("輪詢中斷，{reconnect_secs} 秒後重連…")),
            });
        }
        // wait with backoff, but allow a manual re-trigger to shorten the wait
        match tokio::time::timeout(Duration::from_secs(reconnect_secs), cmd_rx.recv()).await {
            Ok(Some(ClockSyncCmd::SyncNow { ip, port })) => {
                let _ = oneshot_sync(&cfg, &ui, &ip, port).await;
            }
            Ok(Some(ClockSyncCmd::WritePeople { ip, port, entries, reply })) => {
                write_people_oneshot(&ip, port, entries, reply).await;
            }
            Ok(Some(ClockSyncCmd::ReadUsers { ip, port, start, end, reply })) => {
                read_users_oneshot(&ip, port, start, end, reply).await;
            }
            Ok(None) => return Ok(()),
            Err(_) => {}
        }
    }
}

/// One TCP session to the card clock; returns on connection loss / protocol error.
async fn manage_session(
    cfg: &Arc<RwLock<Config>>,
    ui: &Option<UiBus>,
    cmd_rx: &mut tokio::sync::mpsc::UnboundedReceiver<ClockSyncCmd>,
    tx: &UnboundedSender<GcpPunchEvent>,
    dedup: &Arc<std::sync::Mutex<PunchDedup>>,
    addr: SocketAddr,
    first_session: bool,
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

    // initial event-queue drain: only the *first* session may opt to silently
    // discard the pre-commissioning backlog (`forward_initial = false`) so the
    // 2010-era records do not reach GCP. On every reconnect we MUST forward
    // whatever accumulated while offline — otherwise a punch that arrived while
    // the session was down (e.g. 1621 contested by the backend) is deleted by
    // 37H without ever reaching the UI / GCP pipeline.
    let (ev_enabled, forward) = {
        let g = cfg.read().await;
        (g.event_pull.enabled, g.event_pull.forward_initial || !first_session)
    };
    if ev_enabled {
        pull_events(cfg, ui, &mut stream, tx, dedup, forward).await?;
    }

    let sync_interval = { let g = cfg.read().await; g.clock_sync.interval_secs.max(10) };
    let ev_interval = { let g = cfg.read().await; g.event_pull.interval_secs.max(1) };
    let mut sync_tick = tokio::time::interval(Duration::from_secs(sync_interval));
    let mut ev_tick = tokio::time::interval(Duration::from_secs(ev_interval));

    loop {
        let sync_enabled = cfg.read().await.clock_sync.enabled;
        let ev_enabled = cfg.read().await.event_pull.enabled;
        tokio::select! {
            _ = sync_tick.tick(), if sync_enabled => {
                sync_once(cfg, ui, &mut stream).await?;
            }
            _ = ev_tick.tick(), if ev_enabled => {
                pull_events(cfg, ui, &mut stream, tx, dedup, true).await?;
            }
            cmd = cmd_rx.recv() => match cmd {
                Some(ClockSyncCmd::SyncNow { ip, port }) => {
                    match parse_addr(&ip, port) {
                        Some(a) if a == addr => force_sync(cfg, ui, &mut stream).await?,
                        _ => {
                            // manual sync to a different address: run as one-shot
                            oneshot_sync(cfg, ui, &ip, port).await.ok();
                        }
                    }
                }
                Some(ClockSyncCmd::WritePeople { ip, port, entries, reply }) => {
                    info!(%ip, port, n = entries.len(), "write: worker 接手寫入，先釋放 session");
                    drop(stream);
                    let res = crate::punch_writer::add_people(&ip, port, 1, entries).await;
                    info!(
                        ok = matches!(res, Ok(_)),
                        "write: worker 寫入結束，回傳結果給 UI",
                    );
                    let _ = reply.send(res);
                    return Ok(());
                }
                Some(ClockSyncCmd::ReadUsers { ip, port, start, end, reply }) => {
                    info!(%ip, port, start, end, "read: worker 接手回讀，先釋放 session");
                    drop(stream);
                    let res = crate::punch_writer::read_users(&ip, port, 1, start, end).await;
                    info!(
                        ok = matches!(res, Ok(_)),
                        "read: worker 回讀結束，回傳結果給 UI",
                    );
                    let _ = reply.send(res);
                    return Ok(());
                }
                None => return Ok(()),
            },
        }
    }
}

fn parse_addr(ip: &str, port: u16) -> Option<SocketAddr> {
    format!("{ip}:{port}").parse().ok()
}

/// 一次性人員寫入（worker 沒有活躍 session 時使用；有 session 則是用
/// `manage_session` 的 WritePeople 分支先釋放連線再寫入）。
async fn write_people_oneshot(
    ip: &str,
    port: u16,
    entries: Vec<crate::punch_writer::PersonEntry>,
    reply: tokio::sync::oneshot::Sender<
        Result<Vec<crate::punch_writer::WriteOutcome>, String>,
    >,
) {
    info!(%ip, port, n = entries.len(), "write: worker（無 session）直接一次性寫入");
    let res = crate::punch_writer::add_people(ip, port, 1, entries).await;
    let _ = reply.send(res);
}

/// 一次性全範圍回讀（worker 沒有活躍 session 時使用）。
async fn read_users_oneshot(
    ip: &str,
    port: u16,
    start: u16,
    end: u16,
    reply: tokio::sync::oneshot::Sender<Result<Vec<crate::punch_writer::ReadUser>, String>>,
) {
    info!(%ip, port, start, end, "read: worker（無 session）直接一次性回讀");
    let res = crate::punch_writer::read_users(ip, port, 1, start, end).await;
    let _ = reply.send(res);
}

/// One-shot manual sync to an explicit address (independent of config / session).
/// Reports errors to the UI itself.
async fn oneshot_sync(
    cfg: &Arc<RwLock<Config>>,
    ui: &Option<UiBus>,
    ip: &str,
    port: u16,
) -> Result<(), String> {
    if ip.trim().is_empty() {
        return Err("尚未設定卡鐘 IP".to_string());
    }
    let addr = match parse_addr(ip, port) {
        Some(a) => a,
        None => return Err(format!("無效位址 {ip}:{port}")),
    };
    let bcd = cfg.read().await.clock_sync.bcd_encoding;
    let result = async {
        let mut stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
            .await
            .map_err(|_| "連線逾時".to_string())?
            .map_err(|e| format!("連線失敗：{e}"))?;
        stream.set_nodelay(true).ok();
        let (now, weekday) = host_now();
        write_rtc(&mut stream, &now, weekday, bcd).await?;
        let shown = read_rtc(&mut stream, bcd).await.unwrap_or(RtcReading {
            sec: now.second(),
            min: now.minute(),
            hour: now.hour(),
            weekday,
            day: now.day(),
            month: now.month(),
            year: now.year() as u32,
        });
        Ok::<_, String>((shown, now))
    }
    .await;
    match result {
        Ok((shown, now)) => {
            info!(target = %addr, time = %now.format("%Y-%m-%d %H:%M:%S"), "manual RTC sync written");
            if let Some(ui) = ui {
                ui.send(UiEvent::ClockStatus {
                    online: true,
                    rtc: Some(shown.fmt_local()),
                    note: Some(format!(
                        "已手動校時 {addr}，與 PC 同步 {}",
                        now.format("%Y-%m-%d %H:%M:%S")
                    )),
                });
            }
            Ok(())
        }
        Err(e) => {
            if let Some(ui) = ui {
                ui.send(UiEvent::Error(format!("校時失敗：{e}")));
            }
            Err(e)
        }
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

/// Drains the 25H/37H event queue (FIFO) while records keep arriving.
/// Each record is deleted (37H) right after reading so the queue advances;
/// no-UID records (power-on etc.) are skipped. When `forward` is set, parsed
/// punch events flow into the classify -> GCP pipeline and the UI.
async fn pull_events(
    cfg: &Arc<RwLock<Config>>,
    ui: &Option<UiBus>,
    stream: &mut TcpStream,
    tx: &UnboundedSender<GcpPunchEvent>,
    dedup: &Arc<std::sync::Mutex<PunchDedup>>,
    forward: bool,
) -> Result<(), String> {
    let max_events = {
        let g = cfg.read().await;
        g.event_pull.max_events_per_tick.max(1)
    };
    let tz_offset = cfg.read().await.timezone_offset_seconds;
    let ip = {
        let g = cfg.read().await;
        g.punch_clock.ip.clone().unwrap_or_default()
    };

    let mut seen = 0usize;
    loop {
        if seen >= max_events {
            warn!(max_events, "event pull hit per-tick cap; remaining records stay queued");
            return Ok(());
        }
        let frame = encode_cmd(0x25, &[]);
        stream
            .write_all(&frame)
            .await
            .map_err(|e| format!("25H write error: {e}"))?;
        let body = match read_frame(stream, Duration::from_secs(2)).await {
            Ok(b) => b,
            Err(e) if e.contains("timed out") => {
                // Device momentarily busy / queue draining elsewhere; keep the
                // session and retry on the next tick.
                warn!(err = %e, "25H pull read timed out, keeping session");
                return Ok(());
            }
            Err(e) => {
                // Connection dropped (early eof / reset / abort): tear the
                // session down so manage_session reconnects promptly instead of
                // hammering a dead socket.
                warn!(err = %e, "25H pull read error, closing session");
                return Err(e);
            }
        };
        match body.get(1).copied() {
            Some(0x04) => return Ok(()), // ACK = queue empty
            Some(0x05) => {
                warn!("25H NACK, stopping pull");
                return Ok(());
            }
            Some(_) => {}
            None => return Ok(()),
        }
        seen += 1;

        if forward {
            if let Some(ev) = parse_event_reply(&body, tz_offset) {
                forward_event(cfg, ui, tx, dedup, ev, &ip).await?;
            }
        }

        let del = encode_cmd(0x37, &[]);
        stream
            .write_all(&del)
            .await
            .map_err(|e| format!("37H write error: {e}"))?;
        if let Err(e) = read_frame(stream, Duration::from_secs(2)).await {
            warn!(err = %e, "37H ack read error, keeping session");
        }
    }
}

/// Classifies a pulled event (time-window) and pushes it into the GCP pipeline
/// (`tx`) and the UI as a punch.
async fn forward_event(
    cfg: &Arc<RwLock<Config>>,
    ui: &Option<UiBus>,
    tx: &UnboundedSender<GcpPunchEvent>,
    dedup: &Arc<std::sync::Mutex<PunchDedup>>,
    mut ev: PunchEvent,
    ip: &str,
) -> Result<(), String> {
    let dup = {
        let mut guard = dedup.lock().unwrap_or_else(|e| e.into_inner());
        guard.is_duplicate(
            ev.occurred_at.timestamp(),
            &ev.uid_hex,
            &ev.event_code,
        )
    };
    if dup {
        info!(
            node = ev.node_id,
            event = %ev.event_code,
            uid = %ev.uid_hex,
            occurred_at = %ev.occurred_at.to_rfc3339(),
            "punch event duplicate, skipping"
        );
        return Ok(());
    }
    let (classify_enabled, windows, device, receiver_id) = {
        let g = cfg.read().await;
        (
            g.classify.enabled,
            g.classify.windows.clone(),
            g.device.clone(),
            g.receiver_id.clone(),
        )
    };
    if classify_enabled && !windows.is_empty() {
        ev = classify_windows(ev, &windows);
    }
    let received_at = Utc::now().fixed_offset();
    let gcp = GcpPunchEvent::from_punch(&ev, &device, ip, &receiver_id, received_at);
    tx.send(gcp).map_err(|_| "delivery worker is gone".to_string())?;
    info!(
        node = ev.node_id,
        event = %ev.event_code,
        uid = %ev.uid_hex,
        occurred_at = %ev.occurred_at.to_rfc3339(),
        "punch event pulled via 25H"
    );
    if let Some(ui) = ui {
        ui.send(UiEvent::Punch {
            time: ev.occurred_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            uid: ev.uid_hex.clone(),
            event: ev.event_code.clone(),
            ip: ip.to_string(),
        });
    }
    Ok(())
}

/// Parses a 25H event-log record into a `PunchEvent`. Returns `None` when the
/// record carries no valid card UID (e.g. M24 power-on) or is malformed —
/// those records are still deleted so the queue advances. Layout (µA event
/// record, PRD §2.9): node func src sec min hour weekday day month year port
/// [Data9..], with tag bytes = Data21 Data15 Data16 Data19 Data20 (big-endian).
fn parse_event_reply(body: &[u8], tz_offset_seconds: i32) -> Option<PunchEvent> {
    if body.len() < 24 || body.get(1).copied() == Some(0x04) {
        return None;
    }
    let sec = body[3] as u32;
    let min = body[4] as u32;
    let hour = body[5] as u32;
    let day = body[7] as u32;
    let month = body[8] as u32;
    let year = 2000 + body[9] as u32;
    let naive = NaiveDate::from_ymd_opt(year as i32, month, day)?.and_hms_opt(hour, min, sec)?;
    let offset = FixedOffset::east_opt(tz_offset_seconds)?;
    let occurred_at = offset.from_local_datetime(&naive).earliest()?;

    let func = body[1] as u32;
    let node = body[0] as u32;
    let port = body[10] as u32;
    let door_no = Some(body[19] as u32);

    let tag = ((body[23] as u64) << 32)
        | ((body[17] as u64) << 24)
        | ((body[18] as u64) << 16)
        | ((body[21] as u64) << 8)
        | (body[22] as u64);
    if tag == 0 {
        return None; // no scanned card -> not a punch event (e.g. power-on)
    }
    let uid_hex = format!("{tag:016X}");
    let description = function_codes::lookup(func)
        .map(|i| i.en.to_string())
        .unwrap_or_else(|| "Unknown".to_string());
    let event_code = function_codes::event_code(func);
    let raw = format!(
        "{:02}'{:02}/{:02} {:02}:{:02}:{:02} [{:03}.{:02}:{:02X}]({}){:016X} (M{}){}",
        body[9], month, day, hour, min, sec, node, port, func, door_no.unwrap_or(0), tag, func,
        description
    );
    Some(PunchEvent {
        node_id: node,
        sub_code: port,
        function_code: func,
        event_code,
        description,
        door_no,
        uid_hex,
        uid_decimal: Some(tag),
        username_raw: String::new(),
        username: String::new(),
        occurred_at,
        punch_type: "unknown".to_string(),
        duty_code: None,
        duty_label: None,
        raw,
    })
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

    #[test]
    fn encodes_expected_25h_frame() {
        // frame actually used on the live device
        let f = encode_cmd(0x25, &[]);
        assert_eq!(f, vec![0x7E, 0x04, 0x01, 0x25, 0xDB, 0x01]);
    }

    #[test]
    fn encodes_expected_37h_frame() {
        // frame actually used on the live device
        let f = encode_cmd(0x37, &[]);
        assert_eq!(f, vec![0x7E, 0x04, 0x01, 0x37, 0xC9, 0x01]);
    }

    fn checksummed(data: &[u8]) -> Vec<u8> {
        let mut body = data.to_vec();
        let xor = body.iter().fold(0xFFu8, |a, &b| a ^ b);
        let sum = (body.iter().fold(0u16, |a, &b| a + b as u16) + xor as u16) & 0xFF;
        body.push(xor);
        body.push(sum as u8);
        body
    }

    #[test]
    fn parses_25h_m03_event_with_card() {
        // real capture 2026-09-25 19:54:06, Invalid card, UID 00000000FD6374F6,
        // door 1: 7E 21 00 03 01 06 36 13 06 19 09 1A 11 74 F6 00 00 10 40 FD 63 01 00 74 F6
        //           00 00 00 00 00 00 00 00 0C 37
        let mut data = vec![0x00, 0x03, 0x01, 0x06, 0x36, 0x13, 0x06, 0x19, 0x09, 0x1A, 0x11];
        data.extend_from_slice(&[
            0x74, 0xF6, 0x00, 0x00, 0x10, 0x40, 0xFD, 0x63, 0x01, 0x00, 0x74, 0xF6, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ]);
        let body = checksummed(&data);

        let ev = parse_event_reply(&body, 28800).unwrap();
        assert_eq!(ev.function_code, 3);
        assert_eq!(ev.event_code, "M3");
        assert_eq!(ev.description, "Invalid card");
        assert_eq!(ev.uid_hex, "00000000FD6374F6");
        assert_eq!(ev.uid_decimal, Some(0xFD63_74F6));
        assert_eq!(ev.door_no, Some(1));
        assert_eq!(ev.node_id, 0);
        assert_eq!(ev.sub_code, 17);
        assert_eq!(
            ev.occurred_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-09-25 19:54:06"
        );
    }

    #[test]
    fn skips_event_without_card() {
        // M24 power-on: tag bytes all zero -> not a punch event
        let mut data = vec![0x00, 0x18, 0x01, 0x32, 0x22, 0x0E, 0x05, 0x15, 0x0A, 0x0A, 0x11];
        data.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ]);
        let body = checksummed(&data);
        assert!(parse_event_reply(&body, 28800).is_none());
    }

    #[test]
    fn ack_frame_is_empty_queue() {
        // empty-queue ACK: 7E 0F 00 04 01 C3 46 0F 91 10 10 00 00 00 00 E1 AF
        let data = [0x00, 0x04, 0x01, 0xC3, 0x46, 0x0F, 0x91, 0x10, 0x10, 0x00, 0x00, 0x00, 0x00];
        let body = checksummed(&data);
        assert!(parse_event_reply(&body, 28800).is_none());
    }
}