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
        30 => ("Slave reader off line", "副讀卡機離線"),
        31 => ("Slave reader on line", "副讀卡機連線"),
        32 => ("User PIN code changed", "用戶修改密碼"),
        33 => ("Change user PIN error", "用戶修改密碼失敗"),
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