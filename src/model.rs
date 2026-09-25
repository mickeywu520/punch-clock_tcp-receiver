use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DeviceMeta {
    pub maker: String,
    pub model: String,
}

impl Default for DeviceMeta {
    fn default() -> Self {
        Self {
            maker: "SOYAL".to_string(),
            model: "AR837EF".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PunchEvent {
    pub node_id: u32,
    pub sub_code: u32,
    pub function_code: u32,
    pub event_code: String,
    pub description: String,
    pub door_no: Option<u32>,
    pub uid_hex: String,
    pub uid_decimal: Option<u64>,
    pub username_raw: String,
    pub username: String,
    pub occurred_at: DateTime<FixedOffset>,
    pub punch_type: String,
    pub duty_code: Option<u8>,
    pub duty_label: Option<String>,
    pub raw: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GcpDevice {
    pub maker: String,
    pub model: String,
    pub node_id: u32,
    pub ip: String,
    pub source_sub_code: u32,
    /// Semantic alias of `source_sub_code` (SOYAL Port Number: 17 main port,
    /// 18 WG1, 19 WG2, 1..=16 RS485 sub readers). PRD §5.2.
    pub port_number: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GcpEvent {
    pub function_code: u32,
    pub event_code: String,
    pub description: String,
    pub door_no: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GcpCard {
    pub uid_hex: String,
    pub uid_decimal: Option<u64>,
    pub card_number_hi: Option<u32>,
    pub card_number_lo: Option<u32>,
    /// Semantic alias of `card_number_hi` (Tag UID bit31~16). PRD §5.2.
    pub site_code: Option<u32>,
    /// Semantic alias of `card_number_lo` (Tag UID bit15~0). PRD §5.2.
    pub card_code: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GcpPerson {
    pub alias: Option<String>,
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GcpPunch {
    pub punch_type: String,
    pub duty_code: Option<u8>,
    pub duty_label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GcpIngestedBy {
    pub receiver_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GcpPunchEvent {
    pub schema_version: String,
    pub event_id: String,
    pub message_type: String,
    pub occurred_at: String,
    pub received_at: String,
    pub device: GcpDevice,
    pub event: GcpEvent,
    pub card: GcpCard,
    pub person: GcpPerson,
    pub punch: GcpPunch,
    pub ingested_by: GcpIngestedBy,
    pub raw_message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GcpIngestBatch {
    pub events: Vec<GcpPunchEvent>,
}

impl GcpPunchEvent {
    pub fn from_punch(
        p: &PunchEvent,
        device: &DeviceMeta,
        peer_ip: &str,
        receiver_id: &str,
        received_at: DateTime<FixedOffset>,
    ) -> Self {
        let uid_decimal = p.uid_decimal;
        let (card_hi, card_lo) = split_soyal_card_number(uid_decimal);
        Self {
            schema_version: "v1".to_string(),
            event_id: uuid::Uuid::new_v4().to_string(),
            message_type: "punch_event".to_string(),
            occurred_at: p.occurred_at.to_rfc3339(),
            received_at: received_at.to_rfc3339(),
            device: GcpDevice {
                maker: device.maker.clone(),
                model: device.model.clone(),
                node_id: p.node_id,
                ip: peer_ip.to_string(),
                source_sub_code: p.sub_code,
                port_number: p.sub_code,
            },
            event: GcpEvent {
                function_code: p.function_code,
                event_code: p.event_code.clone(),
                description: p.description.clone(),
                door_no: p.door_no,
            },
            card: GcpCard {
                uid_hex: p.uid_hex.clone(),
                uid_decimal,
                card_number_hi: card_hi,
                card_number_lo: card_lo,
                site_code: card_hi,
                card_code: card_lo,
            },
            person: GcpPerson {
                alias: Some(p.username.clone()),
                user_id: None,
            },
            punch: GcpPunch {
                punch_type: p.punch_type.clone(),
                duty_code: p.duty_code,
                duty_label: p.duty_label.clone(),
            },
            ingested_by: GcpIngestedBy {
                receiver_id: receiver_id.to_string(),
            },
            raw_message: p.raw.clone(),
        }
    }
}

fn split_soyal_card_number(uid: Option<u64>) -> (Option<u32>, Option<u32>) {
    match uid {
        Some(v) => {
            let low32 = (v & 0xFFFF_FFFF) as u32;
            (Some(low32 >> 16), Some(low32 & 0xFFFF))
        }
        None => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample() -> PunchEvent {
        let offset = FixedOffset::east_opt(28800).unwrap();
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
            occurred_at: chrono::Utc
                .with_ymd_and_hms(2021, 5, 12, 13, 38, 54)
                .unwrap()
                .with_timezone(&offset),
            punch_type: "unknown".into(),
            duty_code: None,
            duty_label: None,
            raw: "21'05/12 13:38:54 [001.17:0B](0)00000000D4B81403 rSammi (M11)Normal Access".into(),
        }
    }

    #[test]
    fn converts_and_splits_card() {
        let dev = DeviceMeta::default();
        let now = chrono::Utc::now().fixed_offset();
        let gcp = GcpPunchEvent::from_punch(&sample(), &dev, "10.0.0.5", "recv-1", now);
        assert_eq!(gcp.card.uid_hex, "00000000D4B81403");
        assert_eq!(gcp.card.card_number_hi, Some(0xD4B8));
        assert_eq!(gcp.card.card_number_lo, Some(0x1403));
        assert_eq!(gcp.card.site_code, Some(0xD4B8));
        assert_eq!(gcp.card.card_code, Some(0x1403));
        assert_eq!(gcp.device.node_id, 1);
        assert_eq!(gcp.device.source_sub_code, 17);
        assert_eq!(gcp.device.port_number, 17);
        assert_eq!(gcp.event.event_code, "M11");
        assert_eq!(gcp.message_type, "punch_event");
        assert!(gcp.occurred_at.ends_with("+08:00"));
        assert!(serde_json::to_value(&gcp).is_ok());
    }
}