pub struct EventInfo {
    pub code: u32,
    pub m_code: String,
    pub en: &'static str,
    pub zh: &'static str,
}

pub fn lookup(code: u32) -> Option<EventInfo> {
    let (en, zh) = match code {
        0 => ("Site code error", "系統識別碼錯誤"),
        1 => ("Invalid user PIN", "無效用戶位址或不允許密碼通行"),
        2 => ("Keypad Locked by over error limits times", "連續發生錯誤,按鍵鎖定"),
        3 => ("Invalid card", "無效卡片"),
        4 => ("Time Zone error", "通行時段錯誤"),
        5 => ("Door Group error", "通行門組錯誤"),
        6 => ("Expiry Date", "通行日期管制"),
        7 => ("Over access times", "超出通行次數限制"),
        8 => ("PIN Code error", "密碼輸入錯誤"),
        9 => ("Press duress PB", "緊急求救鈕已啟動"),
        10 => ("Access by Card and PIN", "以刷卡加密碼方式通行"),
        11 => ("Normal Access", "正常進出"),
        12 => ("Force Controller Relay ON", "強制開啟繼電器"),
        13 => ("Force Controller Relay Off", "強制關閉繼電器"),
        14 => ("Controller armed", "設定保全"),
        15 => ("Controller disarmed", "解除保全"),
        16 => ("Egress", "室內開門鈕"),
        17 => ("Alarm event", "警報事件"),
        20 => ("Controller Power Off", "控制器關機"),
        21 => ("Duress", "被脅迫"),
        22 => ("Guards for help", "求救訊息"),
        23 => ("Cleaner access", "清潔人員進出"),
        24 => ("Controller Power On", "控制器開機"),
        28 => ("Access by PIN", "密碼進出"),
29 => ("Digital input actives", "數位輸入(DI)觸發"),
30 => ("Anti-pass back Error", "違反進出管制"),
31 => ("Slave reader off line", "副讀卡機離線"),
32 => ("Slave reader on line", "副讀卡機連線"),
33 => ("User PIN code changed", "用戶修改密碼"),
34 => ("Change user PIN error", "用戶修改密碼失敗"),
35 => ("Enter Auto Door Open Procedure", "進入自動開門流程"),
        36 => ("Exit Auto Door Open Procedure", "離開自動開門流程"),
        39 => ("Access by fingerprint", "指紋進出"),
        40 => ("Fingerprint identify failed", "指紋辨識失敗"),
        42 => ("Remote control Up Key pressed", "遙控器上鍵"),
        43 => ("Disable Reader", "停用讀卡機"),
        44 => ("Enable Reader", "啟用讀卡機"),
        45 => ("Remote control Panic Key pressed", "遙控器緊急鍵"),
        86 => ("Black table tag accessed", "黑名單卡刷卡"),
        100 => ("Access ok : access via vein", "靜脈進出成功"),
        101 => ("Access reject : access via vein", "靜脈進出失敗"),
        104 => ("Fire alarm input trigged", "火警輸入觸發"),
        108 => ("Face ID Passed", "人臉辨識通行成功"),
        109 => ("Face ID Rejected", "人臉辨識通行失敗"),
        112 => ("Black list of Face ID", "人臉識別黑名單"),
        114 => ("Remote Time Attendance", "遠端考勤"),
        _ => return None,
    };
    Some(EventInfo {
        code,
        m_code: format!("M{code}"),
        en,
        zh,
    })
}

pub fn event_code(code: u32) -> String {
    format!("M{code}")
}

/// 轉拋到 GCP 的允許清單：目前只轉送
/// **M11（正常進出／刷卡）** 與 **M108（人臉辨識通行成功）**，
/// 其餘事件仍會在 UI 顯示與記錄，但不送往後台。
pub fn is_gcp_forwardable(event_code: &str) -> bool {
    matches!(event_code, "M11" | "M108")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gcp_allowlist_only_m11_m108() {
        assert!(is_gcp_forwardable("M11"));
        assert!(is_gcp_forwardable("M108"));
        assert!(!is_gcp_forwardable("M24"));
        assert!(!is_gcp_forwardable("M3"));
        assert!(!is_gcp_forwardable("M109"));
        assert!(!is_gcp_forwardable(""));
    }

    #[test]
    fn face_codes_present() {
        assert_eq!(lookup(108).unwrap().zh, "人臉辨識通行成功");
        assert_eq!(lookup(109).unwrap().zh, "人臉辨識通行失敗");
        assert_eq!(lookup(112).unwrap().zh, "人臉識別黑名單");
    }
}