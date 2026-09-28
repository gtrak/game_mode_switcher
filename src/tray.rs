use std::time::Instant;

use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateSolidBrush, DeleteDC,
    DeleteObject, DrawTextW, FillRect, GetDC, ReleaseDC, SelectObject, SetBkMode, SetTextColor,
    BITMAPINFO, BITMAPINFOHEADER, CLEARTYPE_QUALITY, DEFAULT_CHARSET, DIB_RGB_COLORS,
    DRAW_TEXT_FORMAT, FONT_CLIP_PRECISION, FONT_OUTPUT_PRECISION,
    TRANSPARENT,
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
const ID_MANUAL0: usize = 2100;
const ID_EXIT: usize = 2999;
const TIMER_ID: usize = 1;

const ICON_SIZE: i32 = 32;

struct TrayState {
    cfg: crate::Config,
    hwnd: HWND,
    auto: bool,
    manual: Option<crate::ModeSpec>,
    applied: Option<crate::ModeSpec>,
    last_active: Option<bool>,
    last_seen: Option<Instant>,
    hdr_on: bool,
    icon_hdr: HICON,
    icon_sdr: HICON,
}

fn tray_log(msg: &str) {
    let path = crate::config_path(None)
        .parent()
        .map(|d| d.join("game_mode_switcher_tray.log"));
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

unsafe fn make_text_icon(text: &str, bg: COLORREF) -> HICON {
    let size = ICON_SIZE;
    let hdc_screen = GetDC(None);
    let hdc = CreateCompatibleDC(Some(hdc_screen));
    let mut bmi = BITMAPINFO::default();
    bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = size;
    bmi.bmiHeader.biHeight = -size;
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let hbmp = CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits, Some(HANDLE::default()), 0)
        .unwrap_or_else(|_| CreateBitmap(size, size, 1, 32, None));
    let _old_bmp = SelectObject(hdc, hbmp.into());
    let brush = CreateSolidBrush(bg);
    let _old_brush = SelectObject(hdc, brush.into());
    FillRect(hdc, &RECT { left: 0, top: 0, right: size, bottom: size }, brush);
    let hfont = CreateFontW(
        -(size * 15 / 32),
        0,
        0,
        0,
        600,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        FONT_OUTPUT_PRECISION(0),
        FONT_CLIP_PRECISION(0),
        CLEARTYPE_QUALITY,
        0,
        w!("Segoe UI"),
    );
    let _old_font = SelectObject(hdc, hfont.into());
    SetTextColor(hdc, COLORREF(0x00FF_FFFF));
    SetBkMode(hdc, TRANSPARENT);
    let mut rc = RECT { left: 0, top: 0, right: size, bottom: size };
    let mut text_w: Vec<u16> = text.encode_utf16().collect();
    DrawTextW(hdc, &mut text_w, &mut rc, DRAW_TEXT_FORMAT(1 | 4 | 0x20));
    if !bits.is_null() {
        let p = bits as *mut u8;
        let r = 6i32;
        for y in 0..size {
            for x in 0..size {
                let mut alpha = 0xFFu8;
                let dx = if x < r { r - x } else { 0 };
                let dy = if y < r { r - y } else { 0 };
                if dx > 0 && dy > 0 && dx * dx + dy * dy > r * r {
                    alpha = 0;
                }
                let dx = if x >= size - r { x - (size - r - 1) } else { 0 };
                let dy = if y >= size - r { y - (size - r - 1) } else { 0 };
                if dx > 0 && dy > 0 && dx * dx + dy * dy > r * r {
                    alpha = 0;
                }
                *p.add(((y * size + x) * 4 + 3) as usize) = alpha;
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
    let _ = DeleteObject(hfont.into());
    let _ = DeleteObject(brush.into());
    let _ = DeleteObject(mask.into());
    let _ = DeleteObject(hbmp.into());
    let _ = DeleteDC(hdc);
    ReleaseDC(None, hdc_screen);
    icon
}

fn hdr_hz_text(s: &TrayState) -> String {
    let target = match s.applied {
        Some(sp) => crate::mode_spec_label(&sp),
        None => "?".into(),
    };
    let mode = if s.manual.is_some() {
        "manual"
    } else if s.auto {
        "auto"
    } else {
        "off"
    };
    format!(
        "game_mode_switcher: {} | {} ({})",
        if s.hdr_on { "HDR" } else { "SDR" },
        target,
        mode
    )
}

fn update_icon(s: &mut TrayState) {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        ..Default::default()
    };
    nid.hWnd = s.hwnd;
    nid.uID = TRAY_ID;
    nid.uFlags = NIF_ICON | NIF_TIP;
    nid.hIcon = if s.hdr_on { s.icon_hdr } else { s.icon_sdr };
    let tip = hdr_hz_text(s);
    let tip = tip.encode_utf16().take(127).collect::<Vec<u16>>();
    nid.szTip[..tip.len()].copy_from_slice(&tip);
    nid.szTip[tip.len()] = 0;
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn apply_target(s: &mut TrayState, spec: crate::ModeSpec, reason: &str) {
    let outs = crate::enumerate_outputs();
    match crate::pick_mode(&outs, &s.cfg.device, &spec) {
        Some((o, m)) => match crate::apply_mode(&o.device_name, m, true) {
            Ok(()) => {
                tray_log(&format!(
                    "applied {}x{} @ {} Hz ({})",
                    m.w, m.h, m.freq, reason
                ));
            }
            Err(e) => tray_log(&format!("apply failed: {}", e)),
        },
        None => tray_log(&format!(
            "no mode for {} ({})",
            crate::mode_spec_label(&spec),
            reason
        )),
    }
    s.applied = Some(spec);
    update_icon(s);
}

fn apply_auto_hdr(s: &mut TrayState, active: bool) {
    let want = s.cfg.hdr_for(active);
    if let Some(want) = want {
        if want != s.hdr_on {
            match crate::hdr::hdr_set_verified(want) {
                Ok(_) => {
                    tray_log(&format!("auto hdr -> {}", want));
                    s.hdr_on = want;
                }
                Err(e) => tray_log(&format!("auto hdr failed: {}", e)),
            }
        }
    }
    update_icon(s);
}

fn poll_hdr(s: &mut TrayState) {
    if let Some(on) = crate::hdr::hdr_enabled() {
        if on != s.hdr_on {
            tray_log(&format!("hdr state -> {} (external or applied)", on));
            s.hdr_on = on;
            update_icon(s);
        }
    }
}

fn toggle_hdr(s: &mut TrayState) {
    let want = !s.hdr_on;
    match crate::hdr::hdr_set_verified(want) {
        Ok(_) => {
            tray_log(&format!("hdr toggled -> {}", want));
            s.hdr_on = want;
        }
        Err(e) => tray_log(&format!("hdr toggle failed: {}", e)),
    }
    poll_hdr(s);
    update_icon(s);
}

fn tick(s: &mut TrayState) {
    poll_hdr(s);
    if let Some(spec) = s.manual {
        if s.applied != Some(spec) {
            apply_target(s, spec, "manual override");
        }
        return;
    }
    if !s.auto {
        return;
    }
    let procs = crate::running_processes();
    let active = crate::game_active(&s.cfg, &procs);
    if active {
        s.last_seen = Some(Instant::now());
    }
    let (is_active, reason) = if active {
        (true, "game running")
    } else if let Some(t) = s.last_seen {
        if t.elapsed().as_secs() >= s.cfg.grace_secs {
            s.last_seen = None;
            (false, "idle")
        } else {
            return;
        }
    } else {
        return;
    };
    let spec = s.cfg.spec_for(is_active);
    let state_changed = s.last_active != Some(is_active);
    if s.applied != Some(spec) {
        apply_target(s, spec, reason);
    }
    if state_changed {
        apply_auto_hdr(s, is_active);
        s.last_active = Some(is_active);
    }
}

unsafe fn show_menu(s: &mut TrayState, hwnd: HWND) {
    let menu = match CreatePopupMenu() {
        Ok(m) => m,
        Err(_) => return,
    };
    let status_w = crate::to_widez(&hdr_hz_text(s));
    let _ = AppendMenuW(
        menu,
        MF_STRING | MF_DISABLED,
        0,
        PCWSTR::from_raw(status_w.as_ptr()),
    );
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    for (i, sp) in s.cfg.modes.iter().enumerate() {
        let label = crate::to_widez(&crate::mode_spec_label(sp));
        let checked = s.manual == Some(*sp);
        let flags = if checked { MF_STRING | MF_CHECKED } else { MF_STRING };
        let _ = AppendMenuW(
            menu,
            flags,
            ID_MANUAL0 + i,
            PCWSTR::from_raw(label.as_ptr()),
        );
    }
    let auto_label = crate::to_widez("Auto");
    let auto_checked = s.auto && s.manual.is_none();
    let auto_flags = if auto_checked { MF_STRING | MF_CHECKED } else { MF_STRING };
    let _ = AppendMenuW(
        menu,
        auto_flags,
        ID_AUTO,
        PCWSTR::from_raw(auto_label.as_ptr()),
    );
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let exit_w = crate::to_widez("Exit");
    let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, PCWSTR::from_raw(exit_w.as_ptr()));

    let _ = SetForegroundWindow(hwnd);
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let flags = (TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_RETURNCMD).0;
    let id = TrackPopupMenuEx(menu, flags, pt.x, pt.y, hwnd, None).0 as u32 as usize;
    let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);
    if id >= ID_MANUAL0 && id < ID_MANUAL0 + s.cfg.modes.len() {
        let spec = s.cfg.modes[id - ID_MANUAL0];
        s.manual = Some(spec);
        s.auto = false;
        tray_log(&format!(
            "manual = {} (auto off)",
            crate::mode_spec_label(&spec)
        ));
        apply_target(s, spec, "manual");
        return;
    }
    match id {
        ID_AUTO => {
            if s.auto && s.manual.is_none() {
                s.auto = false;
                s.last_seen = None;
                tray_log("auto = false");
                update_icon(s);
            } else {
                s.auto = true;
                s.manual = None;
                tray_log("auto = true");
                let procs = crate::running_processes();
                let active = crate::game_active(&s.cfg, &procs);
                let spec = s.cfg.spec_for(active);
                apply_target(s, spec, "auto resume");
                apply_auto_hdr(s, active);
                s.last_active = Some(active);
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
                if m == WM_LBUTTONUP {
                    toggle_hdr(&mut *sp);
                } else if m == WM_RBUTTONUP || m == WM_CONTEXTMENU {
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
            let nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: TRAY_ID,
                ..Default::default()
            };
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
        let icon_hdr = make_text_icon("HDR", rgb(196, 98, 0));
        let icon_sdr = make_text_icon("SDR", rgb(58, 58, 64));
        let hinst = match GetModuleHandleW(None) {
            Ok(h) => h,
            Err(_) => {
                tray_log("GetModuleHandleW failed");
                return;
            }
        };
        let class_name = w!("game_mode_switcher_tray");
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
            w!("game_mode_switcher applet"),
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
        let auto_start = cfg.auto_enabled_on_start;
        let hdr0 = crate::hdr::hdr_enabled().unwrap_or(false);
        let mut state = Box::new(TrayState {
            cfg,
            hwnd,
            auto: auto_start,
            manual: None,
            applied: None,
            last_active: None,
            last_seen: None,
            hdr_on: hdr0,
            icon_hdr,
            icon_sdr,
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
        nid.hIcon = if hdr0 { icon_hdr } else { icon_sdr };
        if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            tray_log("Shell_NotifyIconW(NIM_ADD) failed (explorer not running?)");
        }

        SetTimer(
            Some(hwnd),
            TIMER_ID,
            (state.cfg.poll_secs.max(1) * 1000) as u32,
            None,
        );

        if auto_start {
            tray_log("auto enabled at startup; syncing");
            let procs = crate::running_processes();
            let active = crate::game_active(&state.cfg, &procs);
            let spec = state.cfg.spec_for(active);
            apply_target(&mut state, spec, "startup sync");
            apply_auto_hdr(&mut state, active);
            state.last_active = Some(active);
        } else {
            update_icon(&mut state);
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
