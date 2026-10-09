//! SOYAL 控制器寫入人員協定（83H/84H 設定人員＋2EH 寫姓名＋87H 空位掃描）。
//! 由 `tools/soyal_proto.py` + `tools/punch_admin.py` 的實作移植而來：
//!
//! * 短封包：`7E <len> DID CMD <data...> XOR SUM`
//!   * `len` = body(DID..data) 長度 + 2（XOR+SUM）
//!   * `xor` = 0xFF 逐一 XOR body；`sum` = (sum(body) + xor) & 0xFF
//! * 84H 寫入回 ACK（0x04）/ NACK（0x05）/ 認證錯誤（0x06）/ 協定錯誤（0x0C）
//! * 26-byte record：`Addr(2) UID(8) PIN(4) Mode(1) Zone(1) G1(1) G2(1) Y(1) M(1) D(1) Level(1) Option(1) 保留(3)`
//! * 2EH 姓名：`Addr(3) count(1) 16B Big5 字串`（大端）
//! * 87H：`Addr(2) nums(1)`，回 24-byte records（不含位址）

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// 協定常數
// ---------------------------------------------------------------------------

pub const HEAD_SHORT: u8 = 0x7E;
pub const HEAD_LARGE: [u8; 4] = [0xFF, 0x00, 0x5A, 0xA5];

pub const CMD_DATA: u8 = 0x03; // 資料回覆
pub const ECHO_ACK: u8 = 0x04; // 寫入成功
pub const ECHO_NACK: u8 = 0x05; // 拒絕
pub const ECHO_AUTH_ERR: u8 = 0x06; // 認證錯誤
pub const ECHO_PROTO_ERR: u8 = 0x0C; // 協定/格式錯誤

pub const CMD_SET_USER: u8 = 0x84; // 新增/覆寫人員（無 APB）
#[allow(dead_code)]
pub const CMD_SET_USER_APB: u8 = 0x83; // 同上（含 anti-pass-back）
pub const CMD_WRITE_ALIAS: u8 = 0x2E; // 寫入姓名
pub const CMD_READ_ALIAS: u8 = 0x2E; // 回讀姓名（同一指令，靠封包長度區分讀/寫）
pub const CMD_READ_USER: u8 = 0x87; // 回讀人員
pub const CMD_READ_RTC: u8 = 0x24; // 讀取時間＋韌體版本（連線暖身用）

/// 是否為寫入指令的「終結」回應碼（ACK/NACK/認證/協定錯誤）。
pub fn is_terminal(cmd: u8) -> bool {
    matches!(
        cmd,
        ECHO_ACK | ECHO_NACK | ECHO_AUTH_ERR | ECHO_PROTO_ERR
    )
}

const OP_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_AUTO_SCAN: u16 = 300;

/// 通行模式（Mode byte 8.19：bit7~6）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccessMode {
    /// 僅卡片
    #[default]
    Card,
    /// 卡片或密碼
    CardOrPin,
    /// 卡片＋密碼
    CardPlusPin,
}

impl AccessMode {
    pub fn label(self) -> &'static str {
        match self {
            AccessMode::Card => "卡片驗證",
            AccessMode::CardOrPin => "卡片或密碼",
            AccessMode::CardPlusPin => "卡片+密碼",
        }
    }

    pub fn from_label(label: &str) -> Self {
        match label {
            "卡片或密碼" => AccessMode::CardOrPin,
            "卡片+密碼" => AccessMode::CardPlusPin,
            _ => AccessMode::Card,
        }
    }

    fn mode_byte(self) -> u8 {
        // Bit4「Card omitted after fingerprint」＋ Bit3「Fingerprint omitted after card」。
        // 實機（AR-821EFv5，含人臉模組）實測：若這兩個位元為 0，控制器會把每次刷卡
        // 視為「卡＋生物特徵」多因子，卡單獨刷無法完成（面板停在「影像 + 讀卡/密碼」，
        // 且不產生 M11 事件）；設為 1 後卡單獨刷即可完成並產生 M11。
        // 本機自行登錄的人員 Mode = 0x58（＝此二位元已設），故比照設定。
        //   0x40 → 0x58（卡片驗證）、0x80 → 0x98（卡片或密碼）、0xC0 → 0xD8（卡片+密碼）
        const SKIP_BIOMETRIC: u8 = 0x18;
        match self {
            AccessMode::Card => 0x40 | SKIP_BIOMETRIC,
            AccessMode::CardOrPin => 0x80 | SKIP_BIOMETRIC,
            AccessMode::CardPlusPin => 0xC0 | SKIP_BIOMETRIC,
        }
    }
}

/// 一筆要寫入的人員
#[derive(Debug, Clone)]
pub struct PersonEntry {
    /// `site:card`（十進位）或 16 碼 HEX UID
    pub card_spec: String,
    pub name: Option<String>,
    /// 指定人員位址；`None` = 自動掃描下一個空位
    pub addr: Option<u16>,
    pub mode: AccessMode,
}

/// 單筆寫入結果
#[derive(Debug, Clone)]
pub struct WriteOutcome {
    pub addr: u16,
    pub uid_hex: String,
    pub name: Option<String>,
    pub ok: bool,
    pub detail: String,
}

/// 87H 回讀到的一筆已註冊人員（不含姓名：姓名只能寫入、無法從 87H 回讀）。
#[derive(Debug, Clone)]
pub struct ReadUser {
    pub addr: u16,
    pub uid_hex: String,
    pub site: u32,
    pub card: u32,
    pub mode: AccessMode,
    pub zone: u8,
    pub group1: u8,
    pub group2: u8,
    pub expire: Option<String>,
    pub level: u8,
    /// 姓名（2EH 回讀；87H 不含姓名）。無姓名或讀取失敗為 `None`。
    pub name: Option<String>,
}

// ---------------------------------------------------------------------------
// 封包組裝 / 解析（純函式）
// ---------------------------------------------------------------------------

pub fn checksum(body: &[u8]) -> (u8, u8) {
    let mut xor = 0xFFu8;
    let mut total = 0u32;
    for &b in body {
        xor ^= b;
        total += b as u32;
    }
    (xor, ((total + xor as u32) & 0xFF) as u8)
}

/// 組出 SOYAL 短封包。
pub fn build_short(did: u8, cmd: u8, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(data.len() + 2);
    body.push(did);
    body.push(cmd);
    body.extend_from_slice(data);
    let len = body.len() + 2;
    let (xor, total) = checksum(&body);
    let mut pkt = Vec::with_capacity(len + 2);
    pkt.push(HEAD_SHORT);
    pkt.push(len as u8);
    pkt.extend_from_slice(&body);
    pkt.push(xor);
    pkt.push(total);
    pkt
}

/// 已解析的一包資料。
#[derive(Debug, Clone)]
pub struct Reply {
    #[allow(dead_code)]
    pub did: u8,
    pub cmd: u8,
    pub data: Vec<u8>,
    #[allow(dead_code)]
    pub checksum_ok: bool,
}

/// 從緩衝區頭部取出完整一包（短或長）。資料不足或需併收時回 `Err(NeedMore)`。
enum Extract {
    Packet(Reply),
    NeedMore,
    Noise,
}

fn extract(buf: &mut Vec<u8>) -> Extract {
    if buf.is_empty() {
        return Extract::NeedMore;
    }
    if buf[0] == HEAD_SHORT {
        if buf.len() < 5 {
            return Extract::NeedMore;
        }
        let total = buf[1] as usize + 2;
        if buf.len() < total {
            return Extract::NeedMore;
        }
        let raw: Vec<u8> = buf.drain(..total).collect();
        let body = &raw[2..total - 2];
        let (xor, sum) = checksum(body);
        return Extract::Packet(Reply {
            did: raw[2],
            cmd: raw[3],
            data: body[2..].to_vec(),
            checksum_ok: raw[total - 2] == xor && raw[total - 1] == sum,
        });
    }
    if buf.starts_with(&HEAD_LARGE) {
        if buf.len() < 6 {
            return Extract::NeedMore;
        }
        let len_field = u16::from_be_bytes([buf[4], buf[5]]) & 0x0FFF;
        let total = 6 + len_field as usize;
        if buf.len() < total {
            return Extract::NeedMore;
        }
        let raw: Vec<u8> = buf.drain(..total).collect();
        let body = &raw[6..total - 2];
        let (xor, sum) = checksum(body);
        return Extract::Packet(Reply {
            did: raw[6],
            cmd: raw[7],
            data: body[2..].to_vec(),
            checksum_ok: raw[total - 2] == xor && raw[total - 1] == sum,
        });
    }
    // 丟棄擋在開頭的非封包雜訊，直到找到短/長標頭
    if let Some(pos) = buf
        .iter()
        .position(|&b| b == HEAD_SHORT || b == HEAD_LARGE[0])
    {
        buf.drain(..pos);
        Extract::Noise
    } else {
        buf.clear();
        Extract::Noise
    }
}

// ---------------------------------------------------------------------------
// 欄位編碼
// ---------------------------------------------------------------------------

/// `site:card`（十進位）或 16 碼 HEX → 8 bytes UID（big-endian）。
pub fn card_spec_to_uid_bytes(spec: &str) -> Result<[u8; 8], String> {
    let clean = spec.trim().replace(' ', "").replace('-', "");
    if clean.is_empty() {
        return Err("卡號不可為空".to_string());
    }
    let hex: String = if let Some((site_txt, card_txt)) = clean.split_once(':') {
        let site: u32 = site_txt
            .trim()
            .parse()
            .map_err(|_| format!("site:card 的 Site 需為十進位數字，收到 {site_txt:?}"))?;
        let card: u32 = card_txt
            .trim()
            .parse()
            .map_err(|_| format!("site:card 的 Card 需為十進位數字，收到 {card_txt:?}"))?;
        if site > 0xFFFF || card > 0xFFFF {
            return Err(format!("site/card 超出 16 bits 範圍：{site}:{card}"));
        }
        let tag = (site << 16) | card;
        format!("00000000{tag:08X}")
    } else {
        clean
    };
    if hex.len() > 16 {
        return Err(format!("UID 超過 8 bytes：{spec}"));
    }
    if hex.len() % 2 != 0 {
        return Err(format!("UID 需為偶數位 HEX：{spec}"));
    }
    let padded = format!("{:0>16}", hex);
    let mut out = [0u8; 8];
    for (i, ch) in padded.as_bytes().chunks(2).enumerate() {
        let byte = u8::from_str_radix(std::str::from_utf8(ch).unwrap(), 16)
            .map_err(|_| format!("UID 含非 HEX 字元：{spec}"))?;
        out[i] = byte;
    }
    Ok(out)
}

/// 26-byte 人員 record（Mode byte 由 `AccessMode` 提供）。
pub fn build_user_record(addr: u16, uid: &[u8; 8], mode: u8) -> Vec<u8> {
    let mut rec = Vec::with_capacity(26);
    rec.extend_from_slice(&addr.to_be_bytes()); //      Addr(2)
    rec.extend_from_slice(uid); //                       UID(8)
    rec.extend_from_slice(&0u32.to_be_bytes()); //      PIN(4)=0
    rec.push(mode); //                                    Mode(1)
    rec.push(0x00); //                                    Zone(1)
    rec.push(0xFF); //                                    G1(1)
    rec.push(0xFF); //                                    G2(1)
    rec.push(0x4F); //                                    Y=2079
    rec.push(0x0C); //                                    M=12
    rec.push(0x1F); //                                    D=31
    rec.push(0x00); //                                    Level(1)
    rec.push(0x00); //                                    Option(1)
    rec.extend_from_slice(&[0x00, 0x00, 0x00]); //       保留(3)
    rec
}

/// 2EH 姓名封包資料：`Addr(3) count(1) 16B Big5`。
pub fn build_alias_data(addr: u16, name: &str, encoding: &'static encoding_rs::Encoding) -> Result<Vec<u8>, String> {
    let (bytes, _, _) = encoding.encode(name);
    let raw = bytes.into_owned();
    if raw.is_empty() {
        return Err("姓名不可為空".to_string());
    }
    let mut padded = [0u8; 16];
    let n = raw.len().min(16);
    padded[..n].copy_from_slice(&raw[..n]);
    let mut data = Vec::with_capacity(20);
    data.push(0); // 位址 3 bytes 大端，先 MSB（0）
    data.extend_from_slice(&addr.to_be_bytes());
    data.push(1); // count
    data.extend_from_slice(&padded);
    Ok(data)
}

// ---------------------------------------------------------------------------
// TCP 收送
// ---------------------------------------------------------------------------

struct Conn {
    ip: String,
    port: u16,
    stream: TcpStream,
    buf: Vec<u8>,
}

impl Conn {
    async fn open(ip: &str, port: u16) -> Result<Self, String> {
        let addr = format!("{ip}:{port}")
            .parse::<std::net::SocketAddr>()
            .map_err(|e| format!("無法解析 {ip}:{port}：{e}"))?;
        debug!(%ip, port, "write: 建立 TCP 連線…");
        let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
            .await
            .map_err(|_| format!("連線 {ip}:{port} 逾時"))?
            .map_err(|e| format!("連線 {ip}:{port} 失敗：{e}"))?;
        debug!(%ip, port, "write: TCP 連線成功");
        Ok(Conn {
            ip: ip.to_string(),
            port,
            stream,
            buf: Vec::new(),
        })
    }

    /// 重連（部分機型在 87H 多筆讀取等操作後會關閉連線，需重連重試）。
    async fn reconnect(&mut self) -> Result<(), String> {
        debug!(ip = %self.ip, port = self.port, "write: 連線被關閉，重連…");
        let addr = format!("{}:{}", self.ip, self.port)
            .parse::<std::net::SocketAddr>()
            .map_err(|e| format!("無法解析 {}:{}：{e}", self.ip, self.port))?;
        self.stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
            .await
            .map_err(|_| format!("重連 {}:{} 逾時", self.ip, self.port))?
            .map_err(|e| format!("重連 {}:{} 失敗：{e}", self.ip, self.port))?;
        self.buf.clear();
        debug!(ip = %self.ip, port = self.port, "write: 重連成功");
        Ok(())
    }

    async fn next_packet(&mut self, deadline: tokio::time::Instant) -> Result<Reply, String> {
        loop {
            match extract(&mut self.buf) {
                Extract::Packet(p) => return Ok(p),
                Extract::NeedMore => {}
                Extract::Noise => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "等待卡鐘回應逾時（已收 {} bytes）",
                    self.buf.len()
                ));
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let mut chunk = [0u8; 4096];
            let n = tokio::time::timeout(remaining, self.stream.read(&mut chunk))
                .await
                .map_err(|_| "等待卡鐘回應逾時".to_string())?
                .map_err(|_| "與卡鐘連線中斷".to_string())?;
            if n == 0 {
                return Err("與卡鐘連線中斷（EOF）".to_string());
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// 送出並等待一包。連線被關閉（EOF）時自動重連並重新送出一次
    /// （對應實機「部分機型會關掉連線」的實務，見 `tools/punch_admin.py` 掃描註記）。
    async fn request(&mut self, pkt: &[u8], timeout: Duration) -> Result<Reply, String> {
        match self.raw_request(pkt, timeout).await {
            Err(e) if e.contains("EOF") => {
                warn!(ip = %self.ip, port = self.port, "write: EOF，自動重連並重送一次");
                self.reconnect().await?;
                self.raw_request(pkt, timeout).await
            }
            r => r,
        }
    }

    async fn raw_request(&mut self, pkt: &[u8], timeout: Duration) -> Result<Reply, String> {
        debug!(ip = %self.ip, port = self.port, hex = %hex_dbg(pkt), "write: 送出封包");
        self.stream
            .write_all(pkt)
            .await
            .map_err(|e| format!("送出封包失敗：{e}"))?;
        let deadline = tokio::time::Instant::now() + timeout;
        let r = self.next_packet(deadline).await;
        match &r {
            Ok(p) => debug!(ip = %self.ip, port = self.port, cmd = %format!("0x{:02X}", p.cmd), bytes = p.data.len(), "write: 收到回覆"),
            Err(e) => warn!(ip = %self.ip, port = self.port, %e, "write: 回覆失敗"),
        }
        r
    }

    /// 寫入指令後，跳過 echo（含 0x00 / 自身 cmd），等到終結 code 才回。
    /// 連線被關閉時重連並重送原指令一次（84H/2EH 對同槽位冪等，重送安全）。
    async fn wait_ack(&mut self, pkt: &[u8], timeout: Duration) -> Result<Reply, String> {
        let mut retried = false;
        loop {
            let r = match self.next_packet(tokio::time::Instant::now() + timeout).await {
                Ok(r) => r,
                Err(e) if e.contains("EOF") && !retried => {
                    retried = true;
                    self.reconnect().await?;
                    self.raw_request(pkt, timeout).await.ok();
                    continue;
                }
                Err(e) => return Err(e),
            };
            match r.cmd {
                ECHO_ACK | ECHO_NACK | ECHO_AUTH_ERR | ECHO_PROTO_ERR => return Ok(r),
                _ => continue,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 掃描與寫入
// ---------------------------------------------------------------------------

/// 判斷 24 bytes 回讀記錄是否為「空槽」。
///
/// 實機（AR-821EF v5 / 4V6）對空槽的回讀是
/// `00 00 00 00 FF FF FF FF 00 00 00 00 00 80 FF FF 4F 0C 1F 00 00 00 00 00`
/// （UID 低 4 bytes = FF FF FF FF、mode=0x00 / zone=0x80），並非 datasheet 常見的
/// 全 FF。故只要 UID 的最低 4 bytes 全為 FF（代表「沒有卡」）即視為空槽，
/// 同時保留全 FF 相容。
fn is_empty_record(rec: &[u8]) -> bool {
    if rec.len() >= 8 {
        // UID 低 32 bits 全 FF ＝ 沒有卡（此機空槽形狀）
        if rec[4..8].iter().all(|&b| b == 0xFF) {
            return true;
        }
    }
    rec.iter().all(|&b| b == 0xFF) || rec.iter().take(8).all(|&b| b == 0xFF)
}

/// 87H 多筆讀取：實機（AR-821EF v5 / 4V6）在 nums<=10 時才完整回 24 bytes/筆；
/// 過大回應會被截斷（或部分機型直接關閉連線），故每批固定 10 筆。
const READ_BATCH: u8 = 10;

/// 87H 從 `start` 找下一個空位（每次讀一至多筆，掃到 `start+MAX_AUTO_SCAN`）。
async fn find_free_addr(
    conn: &mut Conn,
    did: u8,
    start: u16,
) -> Result<u16, String> {
    let mut addr = start;
    let mut batch: u16 = READ_BATCH as u16;
    let end = start.saturating_add(MAX_AUTO_SCAN);
    while addr < end {
        let mut data = Vec::with_capacity(3);
        data.extend_from_slice(&addr.to_be_bytes());
        data.push(batch as u8);
        let pkt = build_short(did, CMD_READ_USER, &data);
        debug!(addr, batch, "write: 87H 掃描空位");
        let r = conn.request(&pkt, OP_TIMEOUT).await?;
        debug!(addr, cmd = %format!("0x{:02X}", r.cmd), bytes = r.data.len(), "write: 87H 回覆");
        match r.cmd {
            CMD_DATA => {
                if r.data.is_empty() {
                    debug!(addr, "write: 87H 回覆無資料 → 此位址為空");
                    // 沒有回傳紀錄：此位址起即為空
                    return Ok(addr);
                }
                let truncated = r.data.len() % 24 != 0;
                let recs: Vec<&[u8]> = r.data.chunks(24).collect();
                if truncated {
                    // 實機於 nums 過大時回應會被截斷 → 改逐筆讀取，避免取到錯誤空位或引發斷線
                    batch = 1;
                }
                let free: Vec<u16> = recs
                    .iter()
                    .enumerate()
                    .filter(|(_, rec)| is_empty_record(rec))
                    .map(|(i, _)| addr + i as u16)
                    .collect();
                if let Some(&f) = free.first() {
                    info!(f, "write: 87H 找到空位");
                    return Ok(f);
                }
                let got = recs.len() as u16;
                if got < batch {
                    // 回傳筆數少於要求 → 該段剩餘即為空，直接用下一格
                    return Ok(addr + got);
                }
                addr = addr.saturating_add(got);
            }
            ECHO_NACK => return Ok(addr),
            _ => {
                // 非預期回覆（例如 0x00），跳過此段再試
                addr = addr.saturating_add(batch.max(1));
            }
        }
    }
    Err(format!("未找到空位人員位址（已掃描 {MAX_AUTO_SCAN} 個位址）"))
}

/// 以 24H 讀取 RTC 作為連線暖身。
///
/// 實機（AR-821EF v5 / 4V6）若以 87H 作為新連線的第一道指令會完全不回應
/// （0 bytes 逾時）；先送任一無副作用指令（24H 最安全，純讀取）後，
/// 87H 才正常回 `0x03` 資料。
async fn warm_up(conn: &mut Conn, did: u8, timeout: Duration) -> Result<(), String> {
    let pkt = build_short(did, CMD_READ_RTC, &[]);
    let r = conn.request(&pkt, timeout).await?;
    debug!(cmd = %format!("0x{:02X}", r.cmd), bytes = r.data.len(), "write: 24H 暖身回覆");
    Ok(())
}

/// 87H 回讀一筆 24-byte 記錄的「通行模式」（Mode byte bit7~6）。
fn access_mode_from_byte(mode: u8) -> AccessMode {
    match mode >> 6 {
        2 => AccessMode::CardOrPin,
        3 => AccessMode::CardPlusPin,
        _ => AccessMode::Card,
    }
}

/// 把 87H 回讀的 24-byte record（read24 版面，UID 從 offset 0 開始）解析成 `ReadUser`。
fn parse_read_user(addr: u16, rec: &[u8]) -> ReadUser {
    let uid: [u8; 8] = rec[0..8].try_into().unwrap_or([0u8; 8]);
    let tag32 = &uid[4..8];
    let site = u32::from(tag32[0]) << 8 | u32::from(tag32[1]);
    let card = u32::from(tag32[2]) << 8 | u32::from(tag32[3]);
    let y = rec[16];
    let m = rec[17];
    let d = rec[18];
    let expire = if y | m | d == 0 {
        None
    } else {
        Some(format!("20{y:02}-{m:02}-{d:02}"))
    };
    ReadUser {
        addr,
        uid_hex: uid.iter().map(|b| format!("{b:02X}")).collect(),
        site,
        card,
        mode: access_mode_from_byte(rec[12]),
        zone: rec[13],
        group1: rec[14],
        group2: rec[15],
        expire,
        level: rec[19] >> 6,
        name: None,
    }
}

/// 組出 2EH 讀取姓名的資料欄：`Index(3, big-endian) + Records(1)`。
pub fn build_read_alias_data(addr: u16, count: u8) -> Vec<u8> {
    vec![0x00, (addr >> 8) as u8, addr as u8, count]
}

/// 解析單筆 2EH 姓名（16 bytes，Big5；首個 0x00 為結束）。
/// 全空或解碼後為空字串回 `None`。
fn decode_alias(rec: &[u8]) -> Option<String> {
    let end = rec.iter().position(|&b| b == 0).unwrap_or(rec.len());
    let slice = &rec[..end];
    if slice.is_empty() || slice.iter().all(|&b| b == 0xFF) {
        return None;
    }
    let (text, _, _) = encoding_rs::BIG5.decode(slice);
    let t = text.trim().to_string();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// 2EH 回讀姓名：`addr` 起連續 `count` 筆，每筆 16 bytes Big5。
///
/// 實機（AR-821EF v5 / 4V6）驗證：回覆 Command `0x03`、**無 Source ID**、
/// 每筆固定 16 bytes（與 87H 不同）；空位址回 16×`FF`。寫入/回讀成對驗證通過。
async fn read_aliases(
    conn: &mut Conn,
    did: u8,
    addr: u16,
    count: u16,
) -> Result<Vec<Option<String>>, String> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let data = build_read_alias_data(addr, count.min(255) as u8);
    let pkt = build_short(did, CMD_READ_ALIAS, &data);
    let r = conn.request(&pkt, OP_TIMEOUT).await?;
    if r.cmd != CMD_DATA {
        return Err(format!("姓名回讀未取得資料（echo 0x{:02X}）", r.cmd));
    }
    let body = &r.data; // 2EH 回覆不含 Source ID
    let full = body.len() / 16;
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        if i < full {
            out.push(decode_alias(&body[i * 16..i * 16 + 16]));
        } else {
            out.push(None);
        }
    }
    Ok(out)
}

/// 87H 全範圍回讀已註冊人員（唯讀）。每批最多 10 筆；回覆截斷時自動降為逐筆。
/// 遇到 NACK（0x05）視為表格結束並停止。
pub async fn read_users(
    ip: &str,
    port: u16,
    did: u8,
    start: u16,
    end: u16,
) -> Result<Vec<ReadUser>, String> {
    if start == 0 || end < start || end.saturating_sub(start) > 0x0FFF {
        return Err(format!("無效的掃描範圍 {start}~{end}（需為 1..=2049 且 start<=end）"));
    }
    info!(ip, port, start, end, "read: 開始回讀已註冊人員");
    let mut conn = Conn::open(ip, port).await?;
    warm_up(&mut conn, did, OP_TIMEOUT).await?;
    let mut users = Vec::new();
    let mut addr = start;
    let mut batch: u16 = READ_BATCH as u16;
    while addr <= end {
        batch = batch.min(end - addr + 1);
        let mut data = Vec::with_capacity(3);
        data.extend_from_slice(&addr.to_be_bytes());
        data.push(batch as u8);
        let pkt = build_short(did, CMD_READ_USER, &data);
        debug!(addr, batch, "read: 87H 回讀人員");
        let r = conn.request(&pkt, OP_TIMEOUT).await?;
        match r.cmd {
            CMD_DATA => {
                if r.data.is_empty() {
                    addr = addr.saturating_add(batch);
                    continue;
                }
                let len = r.data.len().saturating_sub(1);
                let trailing = len % 24;
                let body = &r.data[1..];
                let full = body.len() / 24;
                if trailing != 0 {
                    debug!(addr, bytes = r.data.len(), "read: 87H 回覆截斷，降為逐筆");
                    batch = 1;
                }
                // 2EH 回讀同段姓名（best-effort：失敗僅省略姓名，不影響人員匯出）
                let aliases = if full > 0 {
                    match read_aliases(&mut conn, did, addr, full as u16).await {
                        Ok(a) => Some(a),
                        Err(e) => {
                            debug!(addr, %e, "read: 姓名回讀失敗，省略姓名");
                            None
                        }
                    }
                } else {
                    None
                };
                for i in 0..full {
                    let rec = &body[i * 24..i * 24 + 24];
                    let a = addr.saturating_add(i as u16);
                    if a > end {
                        break;
                    }
                    if !is_empty_record(rec) {
                        let mut u = parse_read_user(a, rec);
                        u.name = aliases
                            .as_ref()
                            .and_then(|v| v.get(i))
                            .and_then(|n| n.clone());
                        info!(addr = a, uid = %u.uid_hex, name = u.name.as_deref().unwrap_or(""), "read: 已註冊人員");
                        users.push(u);
                    }
                }
                addr = addr.saturating_add(full as u16);
            }
            ECHO_NACK => {
                debug!(addr, "read: 87H NACK，視為表格結束");
                break;
            }
            _ => {
                debug!(addr, cmd = %format!("0x{:02X}", r.cmd), "read: 非預期回覆，跳過此段");
                addr = addr.saturating_add(batch);
            }
        }
    }
    info!(ip, port, n = users.len(), "read: 人員回讀完成");
    Ok(users)
}

/// 把回讀的人員轉成 CSV（第一行表頭），供 Excel 開啟。欄位：
/// 位址、姓名（由 2EH 回讀，無則留空）、卡號（site:card）、卡號(HEX)、
/// 通行方式、到期日、時區、門組1、門組2、等級。
pub fn users_csv(users: &[ReadUser]) -> String {
    fn cell(s: &str) -> String {
        format!("\"{}\"", s.replace('"', "\"\""))
    }
    let mut out = String::from("\u{FEFF}位址,姓名,卡號,卡號(HEX),通行方式,到期日,時區,門組1,門組2,等級\r\n");
    for u in users {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{}\r\n",
            u.addr,
            cell(u.name.as_deref().unwrap_or("")),
            cell(&format!("{}:{}", u.site, u.card)),
            cell(&u.uid_hex),
            cell(u.mode.label()),
            cell(u.expire.as_deref().unwrap_or("")),
            u.zone,
            u.group1,
            u.group2,
            u.level
        ));
    }
    out
}

/// 批次寫入人員。連接一次、逐筆 84H→ACK、選填 2EH→姓名。
///
/// `entries` 中 `addr: None` 者自動掃描空位（依序遞進寫入位址）。
/// 個別筆失敗會記入 `WriteOutcome` 而不中斷整批。
pub async fn add_people(
    ip: &str,
    port: u16,
    did: u8,
    entries: Vec<PersonEntry>,
) -> Result<Vec<WriteOutcome>, String> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    info!(ip, port, n = entries.len(), "write: 開始人員寫入");
    let mut conn = Conn::open(ip, port).await?;
    info!(ip, port, "write: 已連線，準備寫入");
    warm_up(&mut conn, did, OP_TIMEOUT).await?;
    let mut next_addr: u16 = 1;
    let mut results = Vec::with_capacity(entries.len());

    for ent in entries {
        // 解析卡號
        let uid = match card_spec_to_uid_bytes(&ent.card_spec) {
            Ok(u) => u,
            Err(msg) => {
                info!(addr = ent.addr.unwrap_or(0), spec = %ent.card_spec, %msg, "write: 卡號解析失敗");
                results.push(WriteOutcome {
                    addr: ent.addr.unwrap_or(0),
                    uid_hex: ent.card_spec.clone(),
                    name: ent.name.clone(),
                    ok: false,
                    detail: msg,
                });
                continue;
            }
        };
        let uid_hex = uid.iter().map(|b| format!("{b:02X}")).collect::<String>();

        // 位址
        let addr = match ent.addr {
            Some(a) => a,
            None => match find_free_addr(&mut conn, did, next_addr).await {
                Ok(a) => a,
                Err(msg) => {
                    info!(addr = next_addr, %msg, "write: 掃描空位失敗");
                    results.push(WriteOutcome {
                        addr: next_addr,
                        uid_hex,
                        name: ent.name.clone(),
                        ok: false,
                        detail: format!("掃描空位失敗（起始 位址{next_addr}）：{msg}"),
                    });
                    continue;
                }
            },
        };

        // 84H 寫入單筆
        let record = build_user_record(addr, &uid, ent.mode.mode_byte());
        info!(addr, uid = %uid_hex, "write: 84H 寫入人員");
        let mut payload = Vec::with_capacity(27);
        payload.push(1); // records count (1 byte BE)
        payload.extend_from_slice(&record);
        let pkt = build_short(did, CMD_SET_USER, &payload);

        let mut detail = String::new();
        let mut ok = true;
        // 實機（AR-821EF v5 / 4V6）對 84H 直接回終結碼 0x04（ACK），沒有先導 echo；
        // 若「第一個回覆」已是終結碼就採用，不再多等一包（否則會逾時/被斷線）。
        match conn.request(&pkt, OP_TIMEOUT).await {
            Ok(r) if is_terminal(r.cmd) => {
                if r.cmd != ECHO_ACK {
                    ok = false;
                    detail = format!("卡鐘回應：{} (0x{:02X})", echo_name(r.cmd), r.cmd);
                }
            }
            Ok(_first) => match conn.wait_ack(&pkt, OP_TIMEOUT).await {
                Ok(r) if r.cmd == ECHO_ACK => {}
                Ok(r) => {
                    ok = false;
                    detail = format!("卡鐘回應：{} (0x{:02X})", echo_name(r.cmd), r.cmd);
                }
                Err(msg) => {
                    ok = false;
                    detail = msg;
                }
            },
            Err(msg) => {
                ok = false;
                detail = msg;
            }
        }
        if ok {
            next_addr = addr.saturating_add(1);
        }

        // 2EH 姓名（選填）
        if ok {
            if let Some(name) = ent.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                match build_alias_data(addr, name, encoding_rs::BIG5) {
                    Ok(ali) => {
                        debug!(addr, %name, "write: 2EH 寫入姓名");
                        let apkt = build_short(did, CMD_WRITE_ALIAS, &ali);
                        match conn.request(&apkt, OP_TIMEOUT).await {
                            Ok(r) if is_terminal(r.cmd) => {
                                if r.cmd != ECHO_ACK {
                                    detail = format!(
                                        "新增成功但姓名寫入失敗：{} (0x{:02X})",
                                        echo_name(r.cmd),
                                        r.cmd
                                    )
                                }
                            }
                            Ok(_) => match conn.wait_ack(&apkt, OP_TIMEOUT).await {
                                Ok(r) if r.cmd == ECHO_ACK => {}
                                Ok(r) => {
                                    detail = format!(
                                        "新增成功但姓名寫入失敗：{} (0x{:02X})",
                                        echo_name(r.cmd),
                                        r.cmd
                                    )
                                }
                                Err(msg) => detail = format!("新增成功但姓名寫入失敗：{msg}"),
                            },
                            Err(msg) => detail = format!("新增成功但姓名寫入失敗：{msg}"),
                        }
                    }
                    Err(msg) => detail = format!("姓名編碼失敗：{msg}"),
                }
            }
        }

        let outcome = WriteOutcome {
            addr,
            uid_hex: uid_hex.clone(),
            name: ent.name,
            ok,
            detail: detail.clone(),
        };
        info!(addr = outcome.addr, uid = %outcome.uid_hex, ok = outcome.ok, detail = %outcome.detail, "write: 單筆結果");
        results.push(outcome);
    }
    info!(ip, port, n = results.len(), "write: 人員寫入完成");
    Ok(results)
}

/// Echo code 人類可讀名稱。
pub fn echo_name(cmd: u8) -> &'static str {
    match cmd {
        CMD_DATA => "資料回覆",
        ECHO_ACK => "ACK（成功）",
        ECHO_NACK => "NACK（拒絕）",
        ECHO_AUTH_ERR => "認證錯誤",
        ECHO_PROTO_ERR => "協定/格式錯誤",
        _ => "未知回應",
    }
}

/// 封包十六進位字串（除錯用）。
fn hex_dbg(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// CSV 批次匯入解析
// ---------------------------------------------------------------------------

/// 解析批次匯入 CSV。每行一筆：第一欄卡號（`site:card` 或 HEX），第二欄（選填）姓名。
/// 支援 UTF-8（含 BOM）與 Big5 兩種來源編碼；以逗號/分號/定位鍵分隔。
pub fn parse_csv(bytes: &[u8]) -> Result<Vec<PersonEntry>, String> {
    let text = decode_csv(bytes)?;
    let mut entries = Vec::new();
    for (idx, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (card, name) = split_row(line);
        let card = card.trim();
        if card.is_empty() {
            return Err(format!("第 {} 行缺卡號", idx + 1));
        }
        // 解析以提早報錯（不中斷），但保留原字串給 add_people 重解析
        card_spec_to_uid_bytes(card).map_err(|e| format!("第 {} 行：{e}", idx + 1))?;
        entries.push(PersonEntry {
            card_spec: card.to_string(),
            name: if name.trim().is_empty() {
                None
            } else {
                Some(name.trim().to_string())
            },
            addr: None,
            mode: AccessMode::Card,
        });
    }
    if entries.is_empty() {
        return Err("CSV 中沒有可匯入的資料列".to_string());
    }
    Ok(entries)
}

fn decode_csv(bytes: &[u8]) -> Result<String, String> {
    let bytes = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &bytes[3..]
    } else {
        bytes
    };
    match std::str::from_utf8(bytes) {
        Ok(s) => Ok(s.to_string()),
        Err(_) => {
            let (decoded, _, _) = encoding_rs::BIG5.decode(bytes);
            Ok(decoded.into_owned())
        }
    }
}

/// 以逗號/分號/定位鍵切第一與第二欄。
fn split_row(line: &str) -> (&str, &str) {
    for sep in [',', ';', '\t'] {
        if let Some(pos) = line.find(sep) {
            return (&line[..pos], &line[pos + 1..]);
        }
    }
    (line, "")
}

// ---------------------------------------------------------------------------
// 測試
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_poll_vector() {
        // 官方範例：7E 04 01 18 E6 FF
        let pkt = build_short(1, 0x18, &[]);
        assert_eq!(pkt, vec![0x7E, 0x04, 0x01, 0x18, 0xE6, 0xFF]);
    }

    #[test]
    fn build_short_known_record_packet_checksum() {
        let uid = *b"\x00\x00\x00\x00\xD4\xB8\x14\x03";
        let rec = build_user_record(1000, &uid, 0x40);
        assert_eq!(rec.len(), 26);
        let mut payload = vec![1u8];
        payload.extend_from_slice(&rec);
        let pkt = build_short(1, CMD_SET_USER, &payload);
        let body = &pkt[2..pkt.len() - 2];
        let (xor, sum) = checksum(body);
        assert_eq!(pkt[pkt.len() - 2], xor);
        assert_eq!(pkt[pkt.len() - 1], sum);
    }

    #[test]
    fn card_spec_site_colon_card() {
        // PRD 範例：64867:29942 → tag 0xFD6374F6
        let uid = card_spec_to_uid_bytes("64867:29942").unwrap();
        assert_eq!(uid, [0x00, 0x00, 0x00, 0x00, 0xFD, 0x63, 0x74, 0xF6]);
    }

    #[test]
    fn card_spec_hex_and_whitespace() {
        let a = card_spec_to_uid_bytes("00 00 00 00 FD 63 74 F6").unwrap();
        let b = card_spec_to_uid_bytes("fd6374f6").unwrap();
        assert_eq!(a, b);
        assert_eq!(a, [0x00, 0x00, 0x00, 0x00, 0xFD, 0x63, 0x74, 0xF6]);
    }

    #[test]
    fn card_spec_rejects_oob() {
        assert!(card_spec_to_uid_bytes("70000:1").is_err());
        assert!(card_spec_to_uid_bytes("1:70000").is_err());
        assert!(card_spec_to_uid_bytes("abcd12zz").is_err());
    }

    #[test]
    fn record_layout_flat() {
        // 實測寫入後 87H 讀回：00 00 00 00 8E A1 4A FE | 00 00 00 00 | 40 | 00 | FF | FF | 4F 0C 1F | 00 | 00 | 00 00 00
        let uid = [0x00, 0x00, 0x00, 0x00, 0x8E, 0xA1, 0x4A, 0xFE];
        let rec = build_user_record(1000, &uid, 0x40);
        let expected: Vec<u8> = vec![
            0x03, 0xE8, 0x00, 0x00, 0x00, 0x00, 0x8E, 0xA1, 0x4A, 0xFE, 0x00, 0x00, 0x00, 0x00,
            0x40, 0x00, 0xFF, 0xFF, 0x4F, 0x0C, 0x1F, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(rec, expected);
    }

    #[test]
    fn alias_data_big5_padding() {
        // "測試" Big5 = B4 FA B8 D5
        let data = build_alias_data(123, "測試", encoding_rs::BIG5).unwrap();
        assert_eq!(data.len(), 20);
        assert_eq!(&data[..3], &[0x00, 0x00, 0x7B]); // addr 123 3 bytes BE
        assert_eq!(data[3], 1); // count
        assert_eq!(&data[4..8], &[0xB4, 0xFA, 0xB8, 0xD5]);
        assert!(data[8..].iter().all(|&b| b == 0));
    }

    #[test]
    fn extract_two_packets_from_stream() {
        let mut buf = build_short(1, 0x18, &[]);
        buf.extend_from_slice(&build_short(1, ECHO_ACK, &[]));
        let got = match extract(&mut buf) {
            Extract::Packet(p) => p,
            _ => panic!("應取出一包"),
        };
        assert_eq!(got.cmd, 0x18);
        assert!(got.checksum_ok);
        let got2 = match extract(&mut buf) {
            Extract::Packet(p) => p,
            _ => panic!("應取出第二包"),
        };
        assert_eq!(got2.cmd, ECHO_ACK);
    }

    #[test]
    fn empty_record_detection() {
        assert!(is_empty_record(&[0xFF; 24]));
        assert!(is_empty_record(&[0xFF; 8]));
        assert!(!is_empty_record(&[0x00; 24]));
    }

    #[test]
    fn empty_record_detection_real_machine_shape() {
        // 實機（AR-821EF v5 / 4V6）空槽 24B 回讀：UID 低 4 bytes 全 FF、mode=0x00/zone=0x80
        let slot = [
            0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, //
            0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0xFF, 0xFF, //
            0x4F, 0x0C, 0x1F, 0x00, 0x00, 0x00, 0x00, 0x00, //
        ];
        assert!(is_empty_record(&slot));
        // 有卡記錄（李雅英 FD6374F6）不是空槽
        let card = [
            0x00, 0x00, 0x00, 0x00, 0xFD, 0x63, 0x74, 0xF6, //
            0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0xFF, 0xFF, //
            0x4F, 0x0C, 0x1F, 0x00, 0x00, 0x00, 0x00, 0x00, //
        ];
        assert!(!is_empty_record(&card));
    }

    #[test]
    fn terminal_codes_are_ack_nack_auth_proto() {
        // 實機 AR-821EF v5 對 84H/2EH 直接回終結碼（無先導 echo）
        assert!(is_terminal(ECHO_ACK));
        assert!(is_terminal(ECHO_NACK));
        assert!(is_terminal(ECHO_AUTH_ERR));
        assert!(is_terminal(ECHO_PROTO_ERR));
        // 資料回覆（87H/25H/2AH 的回應）不是寫入終結碼
        assert!(!is_terminal(CMD_DATA));
        assert!(!is_terminal(0x00));
    }

    #[test]
    fn access_mode_map() {
        assert_eq!(AccessMode::from_label("卡片或密碼"), AccessMode::CardOrPin);
        // 實機（AR-821EFv5，含人臉模組）驗證：card 模式須帶 bit4/bit3（0x18）才會產生 M11。
        assert_eq!(AccessMode::Card.mode_byte(), 0x58);
        assert_eq!(AccessMode::CardOrPin.mode_byte(), 0x98);
        assert_eq!(AccessMode::CardPlusPin.mode_byte(), 0xD8);
        // bit7~6 仍為存取模式（01/10/11）
        assert_eq!(AccessMode::Card.mode_byte() >> 6, 0x01);
        assert_eq!(AccessMode::CardOrPin.mode_byte() >> 6, 0x02);
        assert_eq!(AccessMode::CardPlusPin.mode_byte() >> 6, 0x03);
    }

    #[test]
    fn csv_parse_utf8_and_big5() {
        let utf8 = "64867:29942,王小明\n00 00 00 00 FD 63 74 F6,李四\n";
        let entries = parse_csv(utf8.as_bytes()).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name.as_deref(), Some("王小明"));
        assert_eq!(entries[1].card_spec, "00 00 00 00 FD 63 74 F6");

        let big5 = encoding_rs::BIG5.encode("64867:29942,測試").0.into_owned();
        let entries = parse_csv(&big5).unwrap();
        assert_eq!(entries[0].name.as_deref(), Some("測試"));
    }

    #[test]
    fn csv_skips_comments_and_blank() {
        let s = "# 註解\n\n64867:1,  \n";
        let entries = parse_csv(s.as_bytes()).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].name.is_none());
    }

    #[test]
    fn csv_invalid_line_reports_row() {
        let s = "not-a-card\n";
        assert!(parse_csv(s.as_bytes()).unwrap_err().contains("第 1 行"));
    }

    #[test]
    fn parse_read_user_real_vector() {
        // 87H 回覆 data（source byte + 24B record）＝ 實機向量（writer 回讀 addr 1000 後）
        let data = [
            0x01, 0x00, 0x00, 0x00, 0x00, 0x8E, 0xA1, 0x4A, // source + UID(8)
            0xFE, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0xFF, // UID tail + PIN(4)+Mode
            0xFF, 0x4F, 0x0C, 0x1F, 0x00, 0x00, 0x00, 0x00, // G1+G2+YMD+Level+Option+spare
            0x00, //
        ];
        let rec: Vec<u8> = data[1..].to_vec();
        assert_eq!(rec.len(), 24);
        assert!(!is_empty_record(&rec));
        let u = parse_read_user(1000, &rec);
        assert_eq!(u.uid_hex, "000000008EA14AFE");
        assert_eq!(u.site, 0x8EA1);
        assert_eq!(u.card, 0x4AFE);
        assert_eq!(u.mode, AccessMode::Card);
        assert_eq!(u.zone, 0x00);
        assert_eq!(u.group1, 0xFF);
        assert_eq!(u.group2, 0xFF);
        assert_eq!(u.expire.as_deref(), Some("2079-12-31"));
        assert_eq!(u.level, 0);
    }

    #[test]
    fn parse_read_user_no_expiry_when_zeroes() {
        // 到期日三欄全 0 → None；Level byte 高 2 bits 是等級
        let rec = [
            0x00, 0x00, 0x00, 0x00, 0xFD, 0x63, 0x74, 0xF6, //
            0x00, 0x00, 0x00, 0x00, 0x80, 0x00, 0xFF, 0xFF, //
            0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, //
        ];
        let u = parse_read_user(7, &rec);
        assert_eq!(u.mode, AccessMode::CardOrPin);
        assert_eq!(u.expire, None);
        assert_eq!(u.level, 1);
        assert_eq!(u.uid_hex, "00000000FD6374F6");
    }

    #[test]
    fn users_csv_layout() {
        let u = parse_read_user(2, &[
            0x00, 0x00, 0x00, 0x00, 0xFD, 0x63, 0x74, 0xF6, //
            0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0xFF, 0xFF, //
            0x4F, 0x0C, 0x1F, 0x00, 0x00, 0x00, 0x00, 0x00, //
        ]);
        let csv = users_csv(&[u]);
        assert!(csv.starts_with('\u{FEFF}'));
        assert!(csv.contains("位址,姓名,卡號"));
        assert!(csv.contains("2,\"\",\"64867:29942\",\"00000000FD6374F6\",\"卡片驗證\",\"2079-12-31\",0,255,255,0"));
        assert!(csv.ends_with("\r\n"));
    }

    #[test]
    fn read_users_rejects_bad_range() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(read_users("192.0.2.1", 1621, 1, 0, 10))
            .unwrap_err();
        assert!(err.contains("無效的掃描範圍"));
    }

    #[test]
    fn read_alias_data_layout() {
        // 2EH 讀取：Index(3, BE) + Records(1)
        assert_eq!(build_read_alias_data(1, 1), vec![0x00, 0x00, 0x01, 0x01]);
        assert_eq!(build_read_alias_data(256, 3), vec![0x00, 0x01, 0x00, 0x03]);
    }

    #[test]
    fn decode_alias_terminates_at_nul() {
        // 實機 addr1 回讀：'Lee\0' + 殘留 → 應只取 "Lee"
        let mut rec = [0u8; 16];
        rec[..4].copy_from_slice(b"Lee\0");
        rec[4..8].copy_from_slice(b"uMod");
        assert_eq!(decode_alias(&rec).as_deref(), Some("Lee"));
    }

    #[test]
    fn decode_alias_big5_and_empty() {
        // Big5「王小明」= A4 FD A4 70 A9 FA（實機寫入/回讀驗證向量）
        let mut rec = [0x00u8; 16];
        rec[..6].copy_from_slice(&[0xA4, 0xFD, 0xA4, 0x70, 0xA9, 0xFA]);
        assert_eq!(decode_alias(&rec).as_deref(), Some("王小明"));
        // 空槽 16 bytes 全 0xFF → None
        assert_eq!(decode_alias(&[0xFF; 16]), None);
        // 全 0x00 → None
        assert_eq!(decode_alias(&[0x00; 16]), None);
    }

    #[test]
    fn users_csv_includes_name() {
        let mut u = parse_read_user(2, &[
            0x00, 0x00, 0x00, 0x00, 0xFD, 0x63, 0x74, 0xF6, //
            0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0xFF, 0xFF, //
            0x4F, 0x0C, 0x1F, 0x00, 0x00, 0x00, 0x00, 0x00, //
        ]);
        u.name = Some("李雅英".to_string());
        let csv = users_csv(&[u]);
        assert!(csv.contains("2,\"李雅英\",\"64867:29942\",\"00000000FD6374F6\",\"卡片驗證\",\"2079-12-31\",0,255,255,0"));
    }
}