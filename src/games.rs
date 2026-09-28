use std::time::{Duration, Instant};
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

use crate::config::Config;
use crate::{pcw, to_widez};
pub(crate) struct ProcessInfo {
    pub(crate) pid: u32,
    pub(crate) name: String,
}

pub(crate) fn running_processes() -> Vec<ProcessInfo> {
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
                    out.push(ProcessInfo {
                        pid: pe.th32ProcessID,
                        name: String::from_utf16_lossy(&pe.szExeFile[..end]),
                    });
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

pub(crate) fn gpu_loads() -> Result<Vec<(u32, f64)>, String> {
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
        if PdhAddCounterW(query, pcw(&path_w), 0, &mut counter) != 0 {
            PdhCloseQuery(query);
            return Err(String::from("PdhAddCounterW failed"));
        }
        let c1 = PdhCollectQueryData(query);
        std::thread::sleep(Duration::from_millis(400));
        let c2 = PdhCollectQueryData(query);
        if c1 != 0 || c2 != 0 {
            PdhCloseQuery(query);
            return Err(String::from("PdhCollectQueryData failed"));
        }
        let mut size = 0u32;
        let mut count = 0u32;
        if PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, None)
            != PDH_MORE_DATA
        {
            PdhCloseQuery(query);
            return Err(String::from("PdhGetFormattedCounterArrayW(size) failed"));
        }
        let mut buf = vec![0u8; size as usize];
        let r = PdhGetFormattedCounterArrayW(
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
        let items =
            std::slice::from_raw_parts(buf.as_ptr() as *const PDH_FMT_COUNTERVALUE_ITEM_W, count as usize);
        let mut per_pid: Vec<(u32, f64)> = Vec::new();
        for item in items {
            let p = item.szName.0 as *const u16;
            let mut l = 0usize;
            while *p.add(l) != 0 {
                l += 1;
            }
            let name = String::from_utf16_lossy(std::slice::from_raw_parts(p, l));
            let lower = name.to_ascii_lowercase();
            if !lower.contains("engtype_3d") {
                continue;
            }
            let pid: u32 = name
                .split("pid_")
                .nth(1)
                .and_then(|rest| rest.split('_').next())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let v = item.FmtValue.Anonymous.doubleValue;
            if !v.is_finite() || v < 0.1 {
                continue;
            }
            if let Some(e) = per_pid.iter_mut().find(|(p, _)| *p == pid) {
                e.1 += v;
            } else {
                per_pid.push((pid, v));
            }
        }
        per_pid.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Ok(per_pid)
    }
}

const KGL_PATH: &str = r"System\GameConfigStore\Children";
const KGL_TTL: Duration = Duration::from_secs(300);

type KglCache = std::sync::Mutex<Option<(Instant, Vec<String>)>>;

fn kgl_cache() -> &'static KglCache {
    static CACHE: std::sync::OnceLock<KglCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(None))
}

pub(crate) fn known_game_exes() -> Vec<String> {
    if let Ok(guard) = kgl_cache().lock() {
        if let Some((t, list)) = guard.as_ref() {
            if t.elapsed() < KGL_TTL {
                return list.clone();
            }
        }
    }
    let mut list = Vec::new();
    unsafe {
        use windows::Win32::Foundation::ERROR_SUCCESS;
        use windows::Win32::System::Registry::{
            RegCloseKey, RegEnumKeyW, RegOpenKeyExW, RegQueryValueExW, HKEY_CURRENT_USER,
            HKEY, KEY_READ, REG_VALUE_TYPE,
        };
        let sub = to_widez(KGL_PATH);
        let mut hk = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, pcw(&sub), Some(0), KEY_READ, &mut hk)
            != ERROR_SUCCESS
        {
            let _ = kgl_cache().lock().map(|mut c| *c = None);
            return list;
        }
        let mut idx = 0u32;
        let mut name_buf = [0u16; 256];
        loop {
            if RegEnumKeyW(hk, idx, Some(&mut name_buf)) != ERROR_SUCCESS {
                break;
            }
            let end = name_buf.iter().position(|&c| c == 0).unwrap_or(0);
            if end > 0 {
                let child = String::from_utf16_lossy(&name_buf[..end]);
                let child_sub = to_widez(&format!("{}\\{}", KGL_PATH, child));
                let mut ck = HKEY::default();
                if RegOpenKeyExW(HKEY_CURRENT_USER, pcw(&child_sub), Some(0), KEY_READ, &mut ck)
                    == ERROR_SUCCESS
                {
                    let val = to_widez("MatchedExeFullPath");
                    let mut size = 0u32;
                    let mut vtype = REG_VALUE_TYPE::default();
                    if RegQueryValueExW(
                        ck,
                        pcw(&val),
                        None,
                        Some(&mut vtype),
                        None,
                        Some(&mut size),
                    ) == ERROR_SUCCESS
                        && size > 2
                    {
                        let mut buf = vec![0u8; size as usize];
                        let mut got = size;
                        if RegQueryValueExW(
                            ck,
                            pcw(&val),
                            None,
                            None,
                            Some(buf.as_mut_ptr()),
                            Some(&mut got),
                        ) == ERROR_SUCCESS
                        {
                            let words: Vec<u16> = buf[..(got as usize & !1)]
                                .chunks_exact(2)
                                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                                .collect();
                            let wend =
                                words.iter().position(|&c| c == 0).unwrap_or(words.len());
                            let full = String::from_utf16_lossy(&words[..wend]);
                            if let Some(name) = full.rsplit('\\').next() {
                                let name = name.to_ascii_lowercase();
                                if name.ends_with(".exe") && !list.contains(&name) {
                                    list.push(name);
                                }
                            }
                        }
                    }
                    let _ = RegCloseKey(ck);
                }
            }
            idx += 1;
            if idx >= 4096 {
                break;
            }
        }
        let _ = RegCloseKey(hk);
    }
    let _ = kgl_cache().lock().map(|mut c| {
        *c = Some((Instant::now(), list.clone()));
    });
    list
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


pub(crate) struct DetectSignals {
    pub(crate) named: bool,
    pub(crate) kgl: bool,
    pub(crate) fullscreen: bool,
    pub(crate) gpu: bool,
}

pub(crate) fn detect_signals(cfg: &Config, procs: &[ProcessInfo]) -> DetectSignals {
    let named = cfg
        .games
        .iter()
        .any(|g| procs.iter().any(|p| p.name.eq_ignore_ascii_case(g)));
    let mut kgl = false;
    // early-exit flag: when the KGL branch resolves (ignored match or no
    // match), game_active returns before the gpu_load_detect check
    let mut kgl_early_exit = false;
    let kgl_list = known_game_exes();
    if !kgl_list.is_empty() && cfg.kgl_min_gpu > 0 {
        let loads = gpu_loads().unwrap_or_default();
        if let Some((matched_pid, matched_name)) = procs
            .iter()
            .find(|p| kgl_list.iter().any(|k| p.name.eq_ignore_ascii_case(k)))
            .map(|p| (p.pid, p.name.clone()))
        {
            if cfg.games_ignore.iter().any(|g| matched_name.eq_ignore_ascii_case(g)) {
                kgl_early_exit = true;
            } else {
                let load = loads
                    .iter()
                    .find(|(pid, _)| *pid == matched_pid)
                    .map(|(_, v)| *v)
                    .unwrap_or(0.0);
                if load >= cfg.kgl_min_gpu as f64 {
                    kgl = true;
                }
            }
        } else {
            kgl_early_exit = true;
        }
    }
    let fullscreen = cfg.fullscreen_detect && detect_fullscreen_game();
    let gpu = if !kgl_early_exit {
        if cfg.gpu_load_detect {
            if let Ok(loads) = gpu_loads() {
                loads.first().map(|(_, v)| *v).unwrap_or(0.0) >= cfg.gpu_threshold as f64
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
    };
    DetectSignals {
        named,
        kgl,
        fullscreen,
        gpu,
    }
}

pub(crate) fn game_active(cfg: &Config, procs: &[ProcessInfo]) -> bool {
    let s = detect_signals(cfg, procs);
    s.named || s.kgl || s.fullscreen || s.gpu
}
