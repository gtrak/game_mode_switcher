use std::time::Instant;

use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleDC, CreateDIBSection, CreateSolidBrush, DeleteDC, DeleteObject,
    Ellipse, GetDC, GetStockObject, NULL_PEN, ReleaseDC, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, DIB_RGB_COLORS,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const TRAY_ID: u32 = 1;
const CB_MSG: u32 = WM_APP + 1;
const ID_AUTO: usize = 2001;
const ID_GAME: usize = 2002;
const ID_IDLE: usize = 2003;
const ID_EXIT: usize = 2004;
const ID_HDR: usize = 2005;
const TIMER_ID: usize = 1;

struct TrayState {
    cfg: crate::Config,
    hwnd: HWND,
    auto: bool,
    current_target: Option<u32>,
    last_seen: Option<Instant>,
    icon_idle: HICON,
    icon_game: HICON,
}

fn tray_log(msg: &str) {
    let path = crate::config_path(None)
        .parent()
        .map(|d| d.join("dsc_off_tray.log"));
    if let Some(p) = path {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "[{}] {}", crate::unix_ts(), msg);
        }
    }
}

fn rgb(r: u32, g: u32, b: u32) -> COLORREF {
    COLORREF((b << 16) | (g << 8) | r)
}

unsafe fn make_icon(color: COLORREF) -> HICON {
    let size = GetSystemMetrics(SM_CXSMICON).max(16);
    let hdc_screen = GetDC(None);
    let hdc = CreateCompatibleDC(Some(hdc_screen));
    let mut bmi = BITMAPINFO::default();
    bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = size;
    bmi.bmiHeader.biHeight = -size;
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = 0;
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let hbmp = CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits, Some(HANDLE::default()), 0)
        .unwrap_or_else(|_| CreateBitmap(size, size, 1, 32, None));
    let _old_bmp = SelectObject(hdc, hbmp.into());
    let brush = CreateSolidBrush(color);
    let _old_brush = SelectObject(hdc, brush.into());
    let _old_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
    let _ = Ellipse(hdc, 1, 1, size - 1, size - 1);
    if !bits.is_null() {
        let p = bits as *mut u8;
        let total = (size * size * 4) as usize;
        for i in (0..total).step_by(4) {
            if *p.add(i) != 0 || *p.add(i + 1) != 0 || *p.add(i + 2) != 0 {
                *p.add(i + 3) = 0xFF;
            }
        }
    }
    let mask = CreateBitmap(size, size, 1, 1, None);
    let ii = ICONINFO {
        fIcon: BOOL(1),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask,
        hbmColor: hbmp,
    };
    let icon = CreateIconIndirect(&ii).expect("CreateIconIndirect");
    let _ = DeleteObject(brush.into());
    let _ = DeleteObject(mask.into());
    let _ = DeleteObject(hbmp.into());
    let _ = DeleteDC(hdc);
    ReleaseDC(None, hdc_screen);
    icon
}

fn set_tip(s: &mut TrayState, text: &str) {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        ..Default::default()
    };
    nid.hWnd = s.hwnd;
    nid.uID = TRAY_ID;
    nid.uFlags = NIF_ICON | NIF_TIP;
    nid.hIcon = if s.current_target == Some(s.cfg.game_hz) {
        s.icon_game
    } else {
        s.icon_idle
    };
    let tip = text.encode_utf16().take(127).collect::<Vec<u16>>();
    nid.szTip[..tip.len()].copy_from_slice(&tip);
    nid.szTip[tip.len()] = 0;
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn apply_target(s: &mut TrayState, hz: u32, reason: &str) {
    let outs = crate::enumerate_outputs();
    let mut desc = String::from("no mode available");
    match crate::pick_by_freq(&outs, &s.cfg.device, hz) {
        Some((o, m)) => {
            match crate::apply_mode(&o.device_name, m, true) {
                Ok(()) => {
                    desc = format!("{}x{} @ {} Hz", m.w, m.h, m.freq);
                    tray_log(&format!("applied {} ({})", desc, reason));
                }
                Err(e) => {
                    tray_log(&format!("apply failed: {}", e));
                }
            }
            if m.freq != hz {
                tray_log(&format!(
                    "{} Hz unavailable; closest is {} Hz",
                    hz, m.freq
                ));
            }
        }
        None => {
            tray_log(&format!("no mode for {} Hz target ({})", hz, reason));
        }
    }
    s.current_target = Some(hz);
    set_tip(s, &format!("dsc_off: {} ({})", desc, reason));
}

fn tick(s: &mut TrayState) {
    let procs = crate::running_processes();
    let active = crate::game_active(&s.cfg, &procs);
    if active {
        s.last_seen = Some(Instant::now());
    }
    let (hz, reason) = if active {
        (s.cfg.game_hz, "game running")
    } else if let Some(t) = s.last_seen {
        if t.elapsed().as_secs() >= s.cfg.grace_secs {
            s.last_seen = None;
            (s.cfg.idle_hz, "idle")
        } else {
            return;
        }
    } else {
        return;
    };
    if s.current_target == Some(hz) {
        return;
    }
    apply_target(s, hz, reason);
}

fn current_mode_text() -> String {
    crate::enumerate_outputs()
        .first()
        .and_then(|o| o.current)
        .map(|c| format!("{}x{} @ {} Hz", c.w, c.h, c.freq))
        .unwrap_or_else(|| String::from("no current mode"))
}

unsafe fn show_menu(s: &mut TrayState, hwnd: HWND) {
    let menu = match CreatePopupMenu() {
        Ok(m) => m,
        Err(_) => return,
    };
    let cur_text = current_mode_text();
    let cur_w = crate::to_widez(&cur_text);
    let auto_w = crate::to_widez("Auto: switch to game Hz on game launch");
    let game_w = crate::to_widez(&format!("{} Hz  (DSC on)", s.cfg.game_hz));
    let idle_w = crate::to_widez(&format!("{} Hz  (DSC off)", s.cfg.idle_hz));
    let hdr_cur = crate::hdr::hdr_enabled();
    let hdr_label = match hdr_cur {
        Some(true) => "HDR: on  (click to turn off)",
        Some(false) => "HDR: off  (click to turn on)",
        None => "HDR: not supported",
    };
    let hdr_w = crate::to_widez(hdr_label);
    let hdr_flags = if hdr_cur.is_some() { MF_STRING } else { MF_STRING | MF_GRAYED };
    let exit_w = crate::to_widez("Exit");
    let _ = AppendMenuW(
        menu,
        MF_STRING | MF_DISABLED,
        0,
        PCWSTR::from_raw(cur_w.as_ptr()),
    );
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let auto_flags = if s.auto { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = AppendMenuW(
        menu,
        auto_flags,
        ID_AUTO,
        PCWSTR::from_raw(auto_w.as_ptr()),
    );
    let _ = AppendMenuW(menu, MF_STRING, ID_GAME, PCWSTR::from_raw(game_w.as_ptr()));
    let _ = AppendMenuW(menu, MF_STRING, ID_IDLE, PCWSTR::from_raw(idle_w.as_ptr()));
    let _ = AppendMenuW(menu, hdr_flags, ID_HDR, PCWSTR::from_raw(hdr_w.as_ptr()));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, PCWSTR::from_raw(exit_w.as_ptr()));

    let _ = SetForegroundWindow(hwnd);
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let flags = (TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_RETURNCMD).0;
    let id = TrackPopupMenuEx(menu, flags, pt.x, pt.y, hwnd, None).0 as u32 as usize;
    let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);
    match id {
        ID_AUTO => {
            s.auto = !s.auto;
            tray_log(&format!("auto = {}", s.auto));
            if s.auto {
                SetTimer(
                    Some(hwnd),
                    TIMER_ID,
                    (s.cfg.poll_secs.max(1) * 1000) as u32,
                    None,
                );
                tick(s);
            } else {
                let _ = KillTimer(Some(hwnd), TIMER_ID);
                s.current_target = None;
                s.last_seen = None;
                set_tip(s, &format!("dsc_off: {} (auto off)", current_mode_text()));
            }
        }
        ID_GAME => apply_target(s, s.cfg.game_hz, "manual"),
        ID_IDLE => apply_target(s, s.cfg.idle_hz, "manual"),
        ID_HDR => {
            if let Some(cur) = hdr_cur {
                match crate::hdr::hdr_set_verified(!cur) {
                    Ok(_) => tray_log(&format!("hdr -> {}", !cur)),
                    Err(e) => tray_log(&format!("hdr toggle failed: {}", e)),
                }
            }
        }
        ID_EXIT => {
            let _ = DestroyWindow(hwnd);
        }
        _ => {}
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let sp = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TrayState;
    match msg {
        CB_MSG => {
            if !sp.is_null() {
                let m = lparam.0 as u32;
                if m == WM_LBUTTONUP || m == WM_RBUTTONUP || m == WM_CONTEXTMENU {
                    show_menu(&mut *sp, hwnd);
                }
            }
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_ID && !sp.is_null() {
                tick(&mut *sp);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let mut nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                ..Default::default()
            };
            nid.hWnd = hwnd;
            nid.uID = TRAY_ID;
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

pub fn run(cfg: crate::Config) {
    tray_log("applet starting");
    unsafe {
        let icon_idle = make_icon(rgb(70, 190, 70));
        let icon_game = make_icon(rgb(225, 70, 70));
        let hinst = match GetModuleHandleW(None) {
            Ok(h) => h,
            Err(_) => {
                tray_log("GetModuleHandleW failed");
                return;
            }
        };
        let class_name = w!("dsc_off_tray");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            tray_log("RegisterClassW failed");
            return;
        }
        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!("dsc_off applet"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            Some(HMENU::default()),
            Some(HINSTANCE(hinst.0)),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                tray_log(&format!("CreateWindowExW failed: {}", e));
                return;
            }
        };
        let auto_start = cfg.auto_on_start;
        let mut state = Box::new(TrayState {
            cfg,
            hwnd,
            auto: auto_start,
            current_target: None,
            last_seen: None,
            icon_idle,
            icon_game,
        });
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (&mut *state) as *mut TrayState as isize);

        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            ..Default::default()
        };
        nid.hWnd = hwnd;
        nid.uID = TRAY_ID;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = CB_MSG;
        nid.hIcon = icon_idle;
        let tip = "dsc_off applet".encode_utf16().take(127).collect::<Vec<u16>>();
        nid.szTip[..tip.len()].copy_from_slice(&tip);
        nid.szTip[tip.len()] = 0;
        if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            tray_log("Shell_NotifyIconW(NIM_ADD) failed (explorer not running?)");
        }

        if auto_start {
            SetTimer(
                Some(hwnd),
                TIMER_ID,
                (state.cfg.poll_secs.max(1) * 1000) as u32,
                None,
            );
            tray_log("auto enabled at startup; ticking");
            tick(&mut state);
        }

        let raw = Box::into_raw(state);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, Some(hwnd), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        drop(Box::from_raw(raw));
        tray_log("applet exit");
    }
}
