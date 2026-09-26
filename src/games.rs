use std::{
    env,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, IsIconic, IsWindowVisible, GWL_STYLE,
};

use crate::{find_output, pcw, to_widez, Mode, Output};
use std::process::exit;
pub(crate) struct Config {
    pub(crate) link: String,
    pub(crate) bpp: u32,
    pub(crate) device: Option<String>,
    pub(crate) poll_secs: u64,
    pub(crate) grace_secs: u64,
    pub(crate) game_hz: u32,
    pub(crate) idle_hz: u32,
    pub(crate) fullscreen_detect: bool,
    pub(crate) gpu_load_detect: bool,
    pub(crate) gpu_threshold: u32,
    pub(crate) auto_on_start: bool,
    pub(crate) games: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            link: String::from("dp-uhbr13"),
            bpp: 24,
            device: None,
            poll_secs: 2,
            grace_secs: 15,
            game_hz: 240,
            idle_hz: 120,
            games: Vec::new(),
            fullscreen_detect: true,
            gpu_load_detect: true,
            gpu_threshold: 35,
            auto_on_start: true,
        }
    }
}

pub(crate) const CONFIG_NAME: &str = "dsc_off.ini";

const CONFIG_TEMPLATE: &str = "# dsc_off configuration (used by `watch` and the tray applet)
# link bandwidth class for DSC verdicts:
#   dp-hbr2 dp-hbr3 dp-uhbr10 dp-uhbr13 dp-uhbr20 hdmi20 hdmi21-frl3..frl6
link = dp-uhbr13
# bits per pixel for bandwidth math (24 = 8bpc RGB 4:4:4)
bpp = 24
# restrict to one output, e.g. DISPLAY1 (comment out = first output)
# device = DISPLAY1
# polling interval for game detection, seconds
poll_secs = 2
# seconds after the last game exits before falling back to idle_hz
grace_secs = 15
# refresh targets in Hz; if a target mode does not exist the closest
# lower-refresh mode at the same resolution is used instead
game_hz = 240
idle_hz = 120
# game executables to watch for, comma separated, case-insensitive
# example: games = cyberpunk2077.exe, eldenring.exe, hfw.exe
games =

# also treat the foreground window as a game when it covers the whole
# monitor without a title bar (exclusive or borderless fullscreen)
fullscreen_detect = true

# treat sustained 3D GPU load as gaming (catches windowed games and any
# launcher, no per-game config needed)
gpu_load_detect = true
# minimum 3D engine utilization (%) to count as gaming
gpu_threshold = 35

# auto-detection runs on applet launch (no need to re-enable after reboot)
auto_on_start = true
";

pub(crate) fn config_path(explicit: Option<&str>) -> PathBuf {
    match explicit {
        Some(p) => PathBuf::from(p),
        None => env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join(CONFIG_NAME)))
            .unwrap_or_else(|| PathBuf::from(CONFIG_NAME)),
    }
}

pub(crate) fn load_or_create_config(path: &Path) -> Config {
    if !path.exists() {
        match fs::write(path, CONFIG_TEMPLATE) {
            Ok(_) => println!("wrote default config to {}", path.display()),
            Err(e) => eprintln!("could not write config {}: {}", path.display(), e),
        }
        return Config::default();
    }
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("could not read config {}: {}", path.display(), e);
            exit(1);
        }
    };
    let mut cfg = Config::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim();
        match k.as_str() {
            "link" => cfg.link = v.to_string(),
            "bpp" => cfg.bpp = v.parse().unwrap_or(24),
            "device" => {
                if !v.is_empty() {
                    cfg.device = Some(v.to_string())
                }
            }
            "poll_secs" => cfg.poll_secs = v.parse().unwrap_or(2),
            "grace_secs" => cfg.grace_secs = v.parse().unwrap_or(15),
            "game_hz" => cfg.game_hz = v.parse().unwrap_or(240),
            "idle_hz" => cfg.idle_hz = v.parse().unwrap_or(120),
            "fullscreen_detect" => cfg.fullscreen_detect = v.parse().unwrap_or(true),
            "gpu_load_detect" => cfg.gpu_load_detect = v.parse().unwrap_or(true),
            "gpu_threshold" => cfg.gpu_threshold = v.parse().unwrap_or(35),
            "auto_on_start" => cfg.auto_on_start = v.parse().unwrap_or(true),
            "games" => {
                cfg.games = v
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            _ => {}
        }
    }
    cfg
}

pub(crate) fn running_processes() -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        if let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            let mut pe = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            if Process32FirstW(snap, &mut pe).is_ok() {
                loop {
                    let end = pe
                        .szExeFile
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(pe.szExeFile.len());
                    out.push(String::from_utf16_lossy(&pe.szExeFile[..end]));
                    if Process32NextW(snap, &mut pe).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snap);
        }
    }
    out
}

pub(crate) fn pick_by_freq<'a>(
    outs: &'a [Output],
    device: &Option<String>,
    target_hz: u32,
) -> Option<(&'a Output, Mode)> {
    let o = find_output(outs, device);
    let cur = o.current?;
    let mut best_le: Option<Mode> = None;
    let mut best_gt: Option<Mode> = None;
    for m in &o.modes {
        if m.w != cur.w || m.h != cur.h {
            continue;
        }
        if m.freq <= target_hz {
            if best_le.map(|b| m.freq > b.freq).unwrap_or(true) {
                best_le = Some(*m);
            }
        } else if best_gt.map(|b| m.freq < b.freq).unwrap_or(true) {
            best_gt = Some(*m);
        }
    }
    best_le
        .or(best_gt)
        .map(|m| (o, m))
}

pub(crate) fn detect_fullscreen_game() -> bool {
    const TOL: i32 = 2;
    const WS_CAPTION: u32 = 0x00C0_0000;
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return false;
        }
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return false;
        }
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        if style & WS_CAPTION != 0 {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return false;
        }
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if hmon.is_invalid() {
            return false;
        }
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(hmon, &mut mi).as_bool() {
            return false;
        }
        let m = mi.rcMonitor;
        (r.left - m.left).abs() <= TOL
            && (r.top - m.top).abs() <= TOL
            && (r.right - m.right).abs() <= TOL
            && (r.bottom - m.bottom).abs() <= TOL
    }
}

pub(crate) fn gpu_game_load() -> Result<Option<(u32, f64)>, String> {
    let _ = 0; // placeholder removed below
    use windows::Win32::System::Performance::{
        PdhAddCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
        PdhOpenQueryW, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
        PDH_MORE_DATA,
    };
    unsafe {
        let mut query = PDH_HQUERY::default();
        if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
            return Err(String::from("PdhOpenQueryW failed"));
        }
        let mut counter = PDH_HCOUNTER::default();
        let path_w = to_widez("\\GPU Engine(*)\\Utilization Percentage");
        let add = PdhAddCounterW(query, pcw(&path_w), 0, &mut counter);
        if add != 0 {
            PdhCloseQuery(query);
            return Err(format!("PdhAddCounterW failed ({})", add));
        }
        let c1 = PdhCollectQueryData(query);
        std::thread::sleep(Duration::from_millis(300));
        let c2 = PdhCollectQueryData(query);
        if c1 != 0 || c2 != 0 {
            PdhCloseQuery(query);
            return Err(String::from("PdhCollectQueryData failed"));
        }
        let mut size = 0u32;
        let mut count = 0u32;
        let mut r = PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut size,
            &mut count,
            None,
        );
        if r != PDH_MORE_DATA {
            PdhCloseQuery(query);
            return Err(format!("PdhGetFormattedCounterArrayW(size) failed ({})", r));
        }
        let mut buf = vec![0u8; size as usize];
        r = PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_DOUBLE,
            &mut size,
            &mut count,
            Some(buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W),
        );
        if r != 0 {
            PdhCloseQuery(query);
            return Err(format!("PdhGetFormattedCounterArrayW failed ({})", r));
        }
        PdhCloseQuery(query);
        let items = std::slice::from_raw_parts(buf.as_ptr() as *const PDH_FMT_COUNTERVALUE_ITEM_W, count as usize);
        let mut top: Option<(u32, f64)> = None;
        let mut n3d = 0usize;
        for item in items {
            let name_end = item.szName.0 as *const u16;
            let mut len = 0usize;
            {
                while *name_end.add(len) != 0 {
                    len += 1;
                }
            }
            let name = String::from_utf16_lossy(std::slice::from_raw_parts(name_end, len));
            if !name.to_ascii_lowercase().contains("engtype_3d") {
                continue;
            }
            n3d += 1;
            let pid: u32 = name
                .split('_')
                .find_map(|t| t.strip_prefix("pid_").and_then(|v| v.parse().ok()))
                .unwrap_or(0);
            let v = item.FmtValue.Anonymous.doubleValue;
            if v.is_finite() && v > top.map(|t| t.1).unwrap_or(0.0) {
                top = Some((pid, v));
            }
        }
        if n3d == 0 {
            let mut names = String::new();
            let mut shown = 0;
            for item in items {
                let p = item.szName.0 as *const u16;
                let mut l = 0usize;
                while *p.add(l) != 0 {
                    l += 1;
                }
                let n = String::from_utf16_lossy(std::slice::from_raw_parts(p, l));
                if shown < 8 {
                    names.push_str(&n);
                    names.push_str(" | ");
                    shown += 1;
                }
            }
            return Err(format!("no 3d among {} items; samples: {}", count, names));
        }
        Ok(top)
    }
}

pub(crate) fn game_active(cfg: &Config, procs: &[String]) -> bool {
    if cfg
        .games
        .iter()
        .any(|g| procs.iter().any(|p| p.eq_ignore_ascii_case(g)))
    {
        return true;
    }
    if cfg.fullscreen_detect && detect_fullscreen_game() {
        return true;
    }
    if cfg.gpu_load_detect {
        if let Ok(Some((pid, load))) = gpu_game_load() {
            if load >= cfg.gpu_threshold as f64 {
                return true;
            }
            let _ = pid;
        }
    }


    false
}
