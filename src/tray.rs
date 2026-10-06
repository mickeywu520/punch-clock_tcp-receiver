//! System tray icon + context menu（方案 A：點「X」→ 收進工作匣）。
//!
//! tray-icon 0.19 在 Windows 必須與其 win32 message pump 同執行緒建立，因此
//! `build_tray` 要在 iced 主執行緒（`App::new`）執行；選單事件由 muda 以
//! `MenuEvent::receiver()` 提供，我們在每個 UI tick 輪詢即可（無須額外 thread）。

/// Tray menu commands forwarded into the UI loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCmd {
    /// 顯示主視窗
    Show,
    /// 結束程式（離開 tray）
    Quit,
}

/// Windows 上為 tray-icon 的 `TrayIcon`；其他平台為 unit（佔位）。
#[cfg(target_os = "windows")]
pub type TrayHandle = tray_icon::TrayIcon;
#[cfg(not(target_os = "windows"))]
pub type TrayHandle = ();

/// Poll pending tray menu events. Call every UI tick so it runs on the iced
/// main thread (Windows tray icons need that thread's win32 message pump).
#[cfg(target_os = "windows")]
pub fn poll_tray() -> Option<TrayCmd> {
    let ev = match muda::MenuEvent::receiver().try_recv() {
        Ok(ev) => ev,
        Err(_) => return None,
    };
    match ev.id().as_ref() {
        "show" => Some(TrayCmd::Show),
        "quit" => Some(TrayCmd::Quit),
        _ => None,
    }
}

#[cfg(not(target_os = "windows"))]
pub fn poll_tray() -> Option<TrayCmd> {
    None
}

/// Build the tray icon + context menu. Must run on the iced main thread.
#[cfg(target_os = "windows")]
pub fn build_tray() -> Result<TrayHandle, String> {
    let menu = muda::Menu::new();
    let show = muda::MenuItem::with_id("show", "顯示主視窗", true, None);
    let quit = muda::MenuItem::with_id("quit", "離開程式", true, None);
    menu.append_items(&[&show, &quit])
        .map_err(|e| format!("建立 tray 選單失敗：{e}"))?;

    let icon = tray_icon::Icon::from_rgba(tray_icon_bytes(), 32, 32)
        .map_err(|e| format!("建立 tray 圖示失敗：{e:?}"))?;

    tray_icon::TrayIconBuilder::new()
        .with_tooltip("打卡機中轉程式 (Punch Clock Receiver)")
        .with_menu(Box::new(menu))
        .with_icon(icon)
        .build()
        .map_err(|e| format!("新增 tray 圖示失敗：{e:?}"))
}

#[cfg(not(target_os = "windows"))]
pub fn build_tray() -> Result<TrayHandle, String> {
    Ok(())
}

/// 程式碼內建的 32x32 時鐘圖示（RGBA）。原本用白色，在淺色工作列上會「隱形」，
/// 改為深藍（IKB/DeepBlue，RGB 0,70,153）：淺色與深色工作列都能清楚看見。
#[cfg(target_os = "windows")]
fn tray_icon_bytes() -> Vec<u8> {
    const SIZE: usize = 32;
    const CX: i32 = 15;
    const CY: i32 = 16;
    const R: i32 = 12;
    const RGB: [u8; 3] = [0x00, 0x46, 0x99];
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as i32 - CX;
            let dy = y as i32 - CY;
            let dist2 = dx * dx + dy * dy;
            let ring = ((R - 2) * (R - 2)..=(R * R)).contains(&dist2);
            let hand_min = line_hit(dx, dy, 0.0, 7.0);
            let hand_hour = line_hit(dx, dy, 2.0, 5.0);
            let on = ring || hand_min || hand_hour;
            if on {
                let i = (y * SIZE + x) * 4;
                rgba[i] = RGB[0];
                rgba[i + 1] = RGB[1];
                rgba[i + 2] = RGB[2];
                rgba[i + 3] = 255;
            }
        }
    }
    rgba
}

/// 檢查 (dx, dy) 是否落在從原點往角度 `angle` 長度 `len` 的粗線段上。
#[cfg(target_os = "windows")]
fn line_hit(dx: i32, dy: i32, angle: f64, len: f64) -> bool {
    let rad = angle.to_radians();
    let ex = (len * rad.cos()).round() as i32;
    let ey = (len * rad.sin()).round() as i32;
    let (ex, ey) = if ex == 0 && ey == 0 { (0, 1) } else { (ex, ey) };

    // 投影判斷距離線段是否夠近（線寬約 2px）
    let len2 = ex * ex + ey * ey;
    if len2 == 0 {
        return false;
    }
    let t = (dx * ex + dy * ey) as f64 / len2 as f64;
    if !(0.0..=1.0).contains(&t) {
        return false;
    }
    let px = t * ex as f64;
    let py = t * ey as f64;
    let d2 = (dx as f64 - px).powi(2) + (dy as f64 - py).powi(2);
    d2 <= 2.0
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    #[test]
    fn tray_icon_bytes_valid_rgba() {
        let rgba = super::tray_icon_bytes();
        assert_eq!(rgba.len(), 32 * 32 * 4);
        assert!(rgba.iter().any(|&b| b != 0), "icon 不應全透明");
    }
}