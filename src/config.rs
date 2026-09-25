use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::DeviceMeta;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ListenMode {
    Text,
    Hex,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ListenConfig {
    pub bind: String,
    pub port: u16,
    pub mode: ListenMode,
}

impl Default for ListenConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0".to_string(),
            port: 8031,
            mode: ListenMode::Text,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct UiConfig {
    pub enabled: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PunchClockConfig {
    /// 卡鐘的 IP（GUI 輸入，供「測試連線」與顯示用；收案仍接受任何來源連線）
    pub ip: Option<String>,
    /// 卡鐘 TCP 指令埠（預設 1621），「測試連線」使用
    pub command_port: u16,
}

impl Default for PunchClockConfig {
    fn default() -> Self {
        Self {
            ip: None,
            command_port: 1621,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PunchWindow {
    pub from: String,
    pub to: String,
    pub kind: String,
}

impl Default for PunchWindow {
    fn default() -> Self {
        Self {
            from: "05:00".to_string(),
            to: "12:00".to_string(),
            kind: "check_in".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ClockSyncConfig {
    /// 定時比對卡鐘 RTC（對 Host PC 時間），偏差超過 `max_drift_secs` 才自動校時
    pub enabled: bool,
    /// 比對週期（秒）
    pub interval_secs: u64,
    /// 允許最大偏差（秒）；超過才自動寫入 23H
    pub max_drift_secs: i64,
    /// 中斷後重連等待（秒）
    pub reconnect_secs: u64,
    /// RTC 欄位編碼：`true` = BCD（協定文件），`false` = 單一位元組十進位。
    /// 本單位 AR837EF（firmware 4.6）實測為 raw decimal。
    pub bcd_encoding: bool,
}

impl Default for ClockSyncConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 300,
            max_drift_secs: 60,
            reconnect_secs: 5,
            bcd_encoding: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ClassifyConfig {
    pub enabled: bool,
    pub windows: Vec<PunchWindow>,
}

impl Default for ClassifyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            windows: vec![
                PunchWindow {
                    from: "05:00".to_string(),
                    to: "12:00".to_string(),
                    kind: "check_in".to_string(),
                },
                PunchWindow {
                    from: "12:00".to_string(),
                    to: "23:59".to_string(),
                    kind: "check_out".to_string(),
                },
            ],
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct GcpConfig {
    pub endpoint_url: Option<String>,
    pub bearer_token: Option<String>,
    pub api_key_header: Option<String>,
    pub api_key_value: Option<String>,
    pub timeout_secs: u64,
    pub retry_attempts: u32,
    pub batch_enabled: bool,
    pub batch_max_items: usize,
    pub batch_flush_interval_secs: u64,
}

impl Default for GcpConfig {
    fn default() -> Self {
        Self {
            endpoint_url: None,
            bearer_token: None,
            api_key_header: None,
            api_key_value: None,
            timeout_secs: 10,
            retry_attempts: 5,
            batch_enabled: false,
            batch_max_items: 100,
            batch_flush_interval_secs: 5,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub listen: ListenConfig,
    pub ui: UiConfig,
    pub punch_clock: PunchClockConfig,
    pub device: DeviceMeta,
    pub receiver_id: String,
    pub timezone_offset_seconds: i32,
    pub spool_dir: PathBuf,
    pub log_level: String,
    pub classify: ClassifyConfig,
    pub gcp: GcpConfig,
    pub clock_sync: ClockSyncConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: ListenConfig::default(),
            ui: UiConfig::default(),
            punch_clock: PunchClockConfig::default(),
            device: DeviceMeta::default(),
            receiver_id: "punch-clock-01".to_string(),
            timezone_offset_seconds: 28800,
            spool_dir: PathBuf::from("./spool"),
            log_level: "info".to_string(),
            classify: ClassifyConfig::default(),
            gcp: GcpConfig::default(),
            clock_sync: ClockSyncConfig::default(),
        }
    }
}

pub fn load(path: Option<&str>) -> anyhow::Result<Config> {
    let mut cfg: Config = match path {
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .map_err(|e| anyhow::anyhow!("cannot read config file {p}: {e}"))?;
            serde_json::from_str(&text)
                .map_err(|e| anyhow::anyhow!("invalid config file {p}: {e}"))?
        }
        None => Config::default(),
    };
    apply_env_overrides(&mut cfg);
    Ok(cfg)
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn apply_env_overrides(cfg: &mut Config) {
    if let Some(bind) = env_string("PUNCH_BIND") {
        cfg.listen.bind = bind;
    }
    if let Some(port) = env_string("PUNCH_PORT").and_then(|v| v.parse().ok()) {
        cfg.listen.port = port;
    }
    if let Some(v) = env_string("PUNCH_UI_ENABLED") {
        cfg.ui.enabled = !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no");
    }
    if let Some(ip) = env_string("PUNCH_PUNCHCLOCK_IP") {
        cfg.punch_clock.ip = Some(ip);
    }
    if let Some(cp) = env_string("PUNCH_PUNCHCLOCK_PORT").and_then(|v| v.parse().ok()) {
        cfg.punch_clock.command_port = cp;
    }
    if let Some(url) = env_string("PUNCH_GCP_URL") {
        cfg.gcp.endpoint_url = Some(url);
    }
    if let Some(tok) = env_string("PUNCH_GCP_TOKEN") {
        cfg.gcp.bearer_token = Some(tok);
    }
    if let Some(k) = env_string("PUNCH_GCP_API_KEY") {
        cfg.gcp.api_key_value = Some(k);
    }
    if let Some(h) = env_string("PUNCH_GCP_API_KEY_HEADER") {
        cfg.gcp.api_key_header = Some(h);
    }
    if let Some(id) = env_string("PUNCH_RECEIVER_ID") {
        cfg.receiver_id = id;
    }
    if let Some(lv) = env_string("PUNCH_LOG_LEVEL") {
        cfg.log_level = lv;
    }
    if let Some(v) = env_string("PUNCH_CLOCK_SYNC_ENABLED") {
        cfg.clock_sync.enabled = !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example_config() {
        let raw = std::fs::read_to_string("config.example.json").unwrap();
        let cfg: Config = serde_json::from_str(&raw).unwrap();
        assert_eq!(cfg.listen.port, 8031);
        assert_eq!(cfg.receiver_id, "punch-clock-01");
        assert_eq!(cfg.timezone_offset_seconds, 28800);
    }
}