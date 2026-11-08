use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use reqwest::StatusCode;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::config::{Config, GcpConfig};
use crate::model::{GcpIngestBatch, GcpPunchEvent};

/// 同一批刷卡事件要送到哪一套後台。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Gcp,
    Hr,
}

impl Target {
    pub fn label(self) -> &'static str {
        match self {
            Target::Gcp => "GCP",
            Target::Hr => "人資",
        }
    }
}

/// Shared config used by the forwarder only for its endpoint subset; all fields
/// are read fresh on each delivery so the desktop UI can change the endpoint /
/// API key without a restart.
#[derive(Debug, Clone)]
pub struct Delivery {
    config: Arc<RwLock<Config>>,
    client: reqwest::Client,
    spool_dir: PathBuf,
    target: Target,
}

enum Outcome {
    FailedPermanently(String),
    FailedRetryable(String),
}

impl Delivery {
    pub async fn new(
        config: Arc<RwLock<Config>>,
        spool_dir: PathBuf,
        target: Target,
    ) -> anyhow::Result<Self> {
        let timeout = {
            let g = config.read().await;
            match target {
                Target::Gcp => g.gcp.timeout_secs,
                Target::Hr => g.hr.timeout_secs,
            }
        }
        .max(1);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout))
            .build()
            .map_err(|e| anyhow::anyhow!("cannot build http client: {e}"))?;
        std::fs::create_dir_all(&spool_dir)?;
        Ok(Self {
            config,
            client,
            spool_dir,
            target,
        })
    }

    pub fn target(&self) -> Target {
        self.target
    }

    fn endpoint<'a>(&self, cfg: &'a Config) -> &'a GcpConfig {
        match self.target {
            Target::Gcp => &cfg.gcp,
            Target::Hr => &cfg.hr,
        }
    }

    pub async fn configured(&self) -> bool {
        let cfg = self.config.read().await;
        self.endpoint(&cfg).endpoint_url.is_some()
    }

    pub async fn deliver(&self, events: &[GcpPunchEvent]) -> Result<u64, String> {
        let ep = {
            let cfg = self.config.read().await;
            self.endpoint(&cfg).clone()
        };
        if ep.endpoint_url.is_none() {
            return Err(format!("{} endpoint_url is not configured", self.target.label()));
        }
        if events.is_empty() {
            return Ok(0);
        }
        let url = ep.endpoint_url.as_deref().unwrap_or_default().to_string();
        let attempts = ep.retry_attempts + 1;
        let mut delay = Duration::from_millis(500);
        for attempt in 1..=attempts {
            match self.post_once(&ep, events).await {
                Ok(()) => return Ok(events.len() as u64),
                Err(Outcome::FailedPermanently(msg)) => {
                    warn!(url = %url, %msg, "delivery rejected permanently");
                    return Err(msg);
                }
                Err(Outcome::FailedRetryable(msg)) => {
                    if attempt < attempts {
                        warn!(url = %url, attempt, delay_ms = delay.as_millis(), %msg, "delivery failed, retrying");
                        tokio::time::sleep(delay).await;
                        delay *= 2;
                    } else {
                        return Err(msg);
                    }
                }
            }
        }
        Err("delivery exhausted".to_string())
    }

    async fn post_once(&self, ep: &GcpConfig, events: &[GcpPunchEvent]) -> Result<(), Outcome> {
        let Some(url) = ep.endpoint_url.as_deref() else {
            return Err(Outcome::FailedPermanently("no endpoint".into()));
        };
        let payload = GcpIngestBatch {
            events: events.to_vec(),
        };
        let mut req = self.client.post(url).json(&payload);
        if let Some(tok) = ep.bearer_token.as_deref() {
            req = req.bearer_auth(tok);
        }
        if let Some(value) = ep
            .api_key_value
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            // header 缺省或空白時預設 X-Api-Key（後台唯一支援的 API key header），
            // 避免舊 UI 儲存把 api_key_header 洗回 null 導致 401。
            let header = ep
                .api_key_header
                .as_deref()
                .map(str::trim)
                .filter(|h| !h.is_empty())
                .unwrap_or("X-Api-Key");
            req = req.header(header, value);
        }
        let res = match req.send().await {
            Ok(r) => r,
            Err(e) => return Err(Outcome::FailedRetryable(format!("request error: {e}"))),
        };
        let status = res.status();
        if status.is_success() {
            return Ok(());
        }
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            return Err(Outcome::FailedRetryable(format!("got {status}")));
        }
        Err(Outcome::FailedPermanently(format!("got {status}")))
    }

    pub fn spool(&self, events: &[GcpPunchEvent]) -> anyhow::Result<PathBuf> {
        let ts = chrono::Utc::now().format("%Y%m%d%H%M%S");
        let path = self.spool_dir.join(format!("{}-{}.jsonl", ts, uuid::Uuid::new_v4()));
        let file = std::fs::File::create(&path)?;
        let mut buf = std::io::BufWriter::new(file);
        for ev in events {
            let line = serde_json::to_string(ev)?;
            buf.write_all(line.as_bytes())?;
            buf.write_all(b"\n")?;
        }
        buf.flush()?;
        Ok(path)
    }

    pub async fn replay_spool(&self) -> anyhow::Result<(usize, usize)> {
        if !self.configured().await {
            return Ok((0, 0));
        }
        let mut sent = 0usize;
        let mut kept = 0usize;
        let mut files = std::fs::read_dir(&self.spool_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map_or(false, |x| x == "jsonl"))
            .collect::<Vec<_>>();
        files.sort();
        for path in files {
            if replay_file(&self, &path).await {
                sent += 1;
            } else {
                kept += 1;
            }
        }
        // 子目錄（例如 spool/hr）存在時 remove_dir 會失敗，不是錯誤。
        if std::fs::read_dir(&self.spool_dir)
            .map(|mut d| d.next().is_none())
            .unwrap_or(false)
        {
            let _ = std::fs::remove_dir(&self.spool_dir);
        }
        Ok((sent, kept))
    }
}

async fn replay_file(delivery: &Delivery, path: &Path) -> bool {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            warn!(file=%path.display(), err=%e, "cannot read spool file, keeping");
            return false;
        }
    };
    let events: Vec<GcpPunchEvent> = content
        .lines()
        .filter_map(|l| serde_json::from_str::<GcpPunchEvent>(l).ok())
        .collect();
    if events.is_empty() {
        let _ = std::fs::remove_file(path);
        return true;
    }
    match delivery.deliver(&events).await {
        Ok(_) => {
            info!(file=%path.display(), count=events.len(), "spool file replayed");
            match std::fs::remove_file(path) {
                Ok(()) => true,
                Err(e) => {
                    warn!(file=%path.display(), err=%e, "cannot delete replayed spool file");
                    false
                }
            }
        }
        Err(msg) => {
            warn!(file=%path.display(), %msg, "spool replay deferred");
            false
        }
    }
}