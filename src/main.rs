use std::{
    env,
    fs,
    path::{Path, PathBuf},
    process::exit,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR,
};
use windows::Win32::Graphics::Gdi::{
    ChangeDisplaySettingsExW, EnumDisplayDevicesW, EnumDisplaySettingsExW, CDS_TYPE,
    CDS_UPDATEREGISTRY, DEVMODEW, DISP_CHANGE_SUCCESSFUL, DISPLAY_DEVICEW,
    DISPLAY_DEVICE_ACTIVE, DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, DM_DISPLAYFREQUENCY,
    DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_FLAGS,
    ENUM_DISPLAY_SETTINGS_MODE,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
    TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegEnumKeyW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, REG_BINARY, REG_SAM_FLAGS,
};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, IsIconic,
    IsWindowVisible, GWL_STYLE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VK_CONTROL, VK_LWIN, VK_SHIFT, VIRTUAL_KEY,
};

const BLANKING_FACTOR: f64 = 1.12;

mod hdr;
mod nvapi;
mod tray;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Mode {
    pub(crate) w: u32,
    pub(crate) h: u32,
    pub(crate) freq: u32,
}

pub(crate) struct Output {
    pub(crate) device_name: Vec<u16>,
    pub(crate) adapter: String,
    pub(crate) monitor: String,
    pub(crate) monitor_device_id: String,
    pub(crate) current: Option<Mode>,
    pub(crate) modes: Vec<Mode>,
}

pub(crate) fn wide_to_string(ws: &[u16]) -> String {
    let end = ws.iter().position(|&c| c == 0).unwrap_or(ws.len());
    String::from_utf16_lossy(&ws[..end])
}

fn mode_bitrate_gbps(m: Mode, bpp: u32) -> f64 {
    (m.w as f64 * m.h as f64 * m.freq as f64 * bpp as f64) / 1e9
}

fn link_eff_gbps(link: &str) -> Option<f64> {
    match link {
        "dp-hbr2" => Some(17.28),
        "dp-hbr3" => Some(25.92),
        "dp-uhbr10" => Some(38.69),
        "dp-uhbr13" => Some(51.84),
        "dp-uhbr20" => Some(77.58),
        "hdmi20" => Some(14.4),
        "hdmi21-frl3" => Some(10.67),
        "hdmi21-frl4" => Some(21.33),
        "hdmi21-frl5" => Some(28.44),
        "hdmi21-frl6" => Some(42.67),
        _ => None,
    }
}

fn needs_dsc(m: Mode, bpp: u32, link: &str) -> bool {
    match link_eff_gbps(link) {
        Some(eff) => mode_bitrate_gbps(m, bpp) * BLANKING_FACTOR > eff,
        None => false,
    }
}

pub(crate) fn pcw(v: &[u16]) -> PCWSTR {
    PCWSTR::from_raw(v.as_ptr())
}

pub(crate) fn enumerate_outputs() -> Vec<Output> {
    let mut outs = Vec::new();
    for idx in 0..64u32 {
        let mut dd = DISPLAY_DEVICEW::default();
        dd.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
        unsafe {
            if !EnumDisplayDevicesW(PCWSTR::null(), idx, &mut dd, 0).as_bool() {
                break;
            }
        }
        if (dd.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP) == DISPLAY_DEVICE_ATTACHED_TO_DESKTOP
            && (dd.StateFlags & DISPLAY_DEVICE_ACTIVE) == DISPLAY_DEVICE_ACTIVE
        {
            let mut mon = DISPLAY_DEVICEW::default();
            mon.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
            let (monitor, monitor_device_id) = unsafe {
                if EnumDisplayDevicesW(pcw(&dd.DeviceName), 0, &mut mon, 0).as_bool() {
                    (wide_to_string(&mon.DeviceString), wide_to_string(&mon.DeviceID))
                } else {
                    (String::from("<unknown monitor>"), String::new())
                }
            };
            let mut current = None;
            {
                let mut dm = DEVMODEW::default();
                dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
                unsafe {
                    if EnumDisplaySettingsExW(
                        pcw(&dd.DeviceName),
                        ENUM_CURRENT_SETTINGS,
                        &mut dm,
                        ENUM_DISPLAY_SETTINGS_FLAGS(0),
                    )
                    .as_bool()
                    {
                        current = Some(Mode {
                            w: dm.dmPelsWidth,
                            h: dm.dmPelsHeight,
                            freq: dm.dmDisplayFrequency,
                        });
                    }
                }
            }
            let mut modes = Vec::new();
            let mut i = 0u32;
            loop {
                let mut dm = DEVMODEW::default();
                dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
                unsafe {
                    if !EnumDisplaySettingsExW(
                        pcw(&dd.DeviceName),
                        ENUM_DISPLAY_SETTINGS_MODE(i),
                        &mut dm,
                        ENUM_DISPLAY_SETTINGS_FLAGS(0),
                    )
                    .as_bool()
                    {
                        break;
                    }
                }
                if dm.dmDisplayFrequency >= 24 {
                    let m = Mode {
                        w: dm.dmPelsWidth,
                        h: dm.dmPelsHeight,
                        freq: dm.dmDisplayFrequency,
                    };
                    if !modes.contains(&m) {
                        modes.push(m);
                    }
                }
                if i >= 1024 {
                    break;
                }
                i += 1;
            }
            modes.sort_by(|a, b| b.freq.cmp(&a.freq).then(b.w.cmp(&a.w)));
            outs.push(Output {
                device_name: dd.DeviceName.to_vec(),
                adapter: wide_to_string(&dd.DeviceString),
                monitor,
                monitor_device_id,
                current,
                modes,
            });
        }
    }
    outs
}

pub(crate) fn apply_mode(dev: &[u16], m: Mode, persist: bool) -> Result<(), String> {
    let mut dm = DEVMODEW::default();
    dm.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
    dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;
    dm.dmPelsWidth = m.w;
    dm.dmPelsHeight = m.h;
    dm.dmDisplayFrequency = m.freq;
    let flags = if persist { CDS_UPDATEREGISTRY } else { CDS_TYPE(0) };
    let res = unsafe { ChangeDisplaySettingsExW(pcw(dev), Some(&dm), None, flags, None) };
    if res == DISP_CHANGE_SUCCESSFUL {
        Ok(())
    } else {
        Err(format!(
            "ChangeDisplaySettingsExW failed, DISP_CHANGE code {}",
            res.0
        ))
    }
}

fn parse_args() -> (String, Option<String>, String, u32, Option<u64>) {
    let mut cmd = String::from("status");
    let mut device: Option<String> = None;
    let mut link = String::from("dp-hbr3");
    let mut bpp = 24u32;
    let mut secs: Option<u64> = None;
    let args: Vec<String> = env::args().skip(1).collect();
    let mut it = args.iter();
    if let Some(first) = it.next() {
        match first.as_str() {
            "-h" | "--help" | "help" => {
                print_usage();
                exit(0);
            }
            _ => cmd = first.clone(),
        }
    }
    while let Some(a) = it.next() {
        match a.as_str() {
            "--device" => device = it.next().cloned(),
            "--link" => {
                if let Some(v) = it.next() {
                    link = v.clone();
                }
            }
            "--bpp" => {
                if let Some(v) = it.next() {
                    bpp = v.parse().unwrap_or(24);
                }
            }
            "--secs" => {
                if let Some(v) = it.next() {
                    secs = v.parse().ok();
                }
            }
            _ => {}
        }
    }
    if link_eff_gbps(&link).is_none() {
        eprintln!("unknown link type '{}', see --help", link);
        exit(2);
    }
    (cmd, device, link, bpp, secs)
}

pub(crate) fn find_output<'a>(outs: &'a [Output], device: &Option<String>) -> &'a Output {
    if let Some(want) = device {
        for o in outs {
            let name = wide_to_string(&o.device_name);
            if name.eq_ignore_ascii_case(want)
                || name.trim_start_matches("\\\\.\\").eq_ignore_ascii_case(want)
            {
                return o;
            }
        }
        eprintln!("output '{}' not found; run `dsc_off list`", want);
        exit(2);
    }
    outs.first().expect("no active display outputs found")
}

fn print_mode_line(m: Mode, bpp: u32, link: &str, current: &Option<Mode>) {
    let dsc = if needs_dsc(m, bpp, link) { "DSC" } else { "raw" };
    let cur = if Some(m) == *current { " [current]" } else { "" };
    println!(
        "    {}x{} @ {:>3} Hz   {:>2} Gbps  {}{}",
        m.w,
        m.h,
        m.freq,
        format!("{:.1}", mode_bitrate_gbps(m, bpp)),
        dsc,
        cur
    );
}

pub(crate) fn to_widez(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

const EDID_VALUE: &str = "EDID";
const EDID_OVERRIDE_VALUE: &str = "EDID_Override";
const ENUM_DISPLAY_ROOT: &str = "SYSTEM\\CurrentControlSet\\Enum\\DISPLAY";

struct MonitorReg {
    pnp: String,
    instance: String,
    path: String,
    edid: Option<Vec<u8>>,
    override_data: Option<Vec<u8>>,
    active: bool,
}

fn reg_open(sub: &str, sam: REG_SAM_FLAGS) -> Result<HKEY, WIN32_ERROR> {
    let mut hk = HKEY::default();
    let sub_w = to_widez(sub);
    let err = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, pcw(&sub_w), Some(0), sam, &mut hk) };
    if err == ERROR_SUCCESS {
        Ok(hk)
    } else {
        Err(err)
    }
}

fn reg_close(hk: HKEY) {
    unsafe {
        let _ = RegCloseKey(hk);
    }
}

fn reg_query_binary(hk: HKEY, name: &str) -> Result<Option<Vec<u8>>, WIN32_ERROR> {
    let name_w = to_widez(name);
    let mut size: u32 = 0;
    let e1 = unsafe { RegQueryValueExW(hk, pcw(&name_w), None, None, None, Some(&mut size)) };
    if e1 == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if e1 != ERROR_SUCCESS {
        return Err(e1);
    }
    let mut buf = vec![0u8; size as usize];
    let mut got = size;
    let e2 = unsafe {
        RegQueryValueExW(
            hk,
            pcw(&name_w),
            None,
            None,
            Some(buf.as_mut_ptr()),
            Some(&mut got),
        )
    };
    if e2 != ERROR_SUCCESS {
        return Err(e2);
    }
    buf.truncate(got as usize);
    Ok(Some(buf))
}

fn reg_read_dword(hk: HKEY, name: &str) -> Option<u32> {
    let name_w = to_widez(name);
    let mut v: u32 = 0;
    let mut got = 4u32;
    let e = unsafe {
        RegQueryValueExW(
            hk,
            pcw(&name_w),
            None,
            None,
            Some(&mut v as *mut u32 as *mut u8),
            Some(&mut got),
        )
    };
    if e == ERROR_SUCCESS {
        Some(v)
    } else {
        None
    }
}

fn reg_enum_key(hk: HKEY, idx: u32) -> Option<String> {
    let mut buf = vec![0u16; 256];
    let e = unsafe { RegEnumKeyW(hk, idx, Some(buf.as_mut_slice())) };
    if e != ERROR_SUCCESS {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(0);
    if end == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..end]))
}

fn load_monitor_reg(pnp: &str, instance: &str) -> MonitorReg {
    let dev_params = format!("{}\\{}\\{}\\Device Parameters", ENUM_DISPLAY_ROOT, pnp, instance);
    let parent = format!("{}\\{}\\{}", ENUM_DISPLAY_ROOT, pnp, instance);
    let edid = reg_open(&dev_params, KEY_READ).ok().and_then(|hk| {
        let r = reg_query_binary(hk, EDID_VALUE);
        reg_close(hk);
        r.ok().flatten()
    });
    let override_data = reg_open(&dev_params, KEY_READ).ok().and_then(|hk| {
        let r = reg_query_binary(hk, EDID_OVERRIDE_VALUE);
        reg_close(hk);
        r.ok().flatten()
    });
    let active = reg_open(&parent, KEY_READ)
        .map(|hk| {
            let flags = reg_read_dword(hk, "ConfigFlags");
            reg_close(hk);
            flags.map(|f| f & 0x1 == 0).unwrap_or(true)
        })
        .unwrap_or(true);
    MonitorReg {
        pnp: pnp.to_string(),
        instance: instance.to_string(),
        path: dev_params,
        edid,
        override_data,
        active,
    }
}

fn edid_matches_folder(edid: &[u8], folder: &str) -> bool {
    match parse_edid(edid) {
        Some(info) => folder.ends_with(&format!("{:04X}", info.product)),
        None => false,
    }
}

fn scan_folder_instances(folder: &str, cands: &mut Vec<MonitorReg>) {
    let folder_sub = format!("{}\\{}", ENUM_DISPLAY_ROOT, folder);
    let Ok(fk) = reg_open(&folder_sub, KEY_READ) else {
        return;
    };
    let mut ii = 0u32;
    while let Some(inst) = reg_enum_key(fk, ii) {
        if !cands.iter().any(|c| c.pnp == folder && c.instance == inst) {
            cands.push(load_monitor_reg(&folder, &inst));
        }
        ii += 1;
        if ii >= 64 {
            break;
        }
    }
    reg_close(fk);
}

fn collect_monitor_candidates(o: &Output) -> Vec<MonitorReg> {
    let mut cands: Vec<MonitorReg> = Vec::new();
    let did = o
        .monitor_device_id
        .strip_prefix("MONITOR\\")
        .unwrap_or(&o.monitor_device_id);
    let mut parts = did.split('\\');
    let folder_hint = parts
        .next()
        .filter(|p| !p.is_empty() && !p.contains('{'))
        .map(|p| p.to_string());
    match folder_hint {
        Some(f) => scan_folder_instances(&f, &mut cands),
        None => {
            if let Ok(root) = reg_open(ENUM_DISPLAY_ROOT, KEY_READ) {
                let mut pi = 0u32;
                while let Some(folder) = reg_enum_key(root, pi) {
                    let folder_sub = format!("{}\\{}", ENUM_DISPLAY_ROOT, folder);
                    if let Ok(fk) = reg_open(&folder_sub, KEY_READ) {
                        let mut ii = 0u32;
                        while let Some(inst) = reg_enum_key(fk, ii) {
                            let inst_sub = format!(
                                "{}\\{}\\{}\\Device Parameters",
                                ENUM_DISPLAY_ROOT, folder, inst
                            );
                            let matches = reg_open(&inst_sub, KEY_READ)
                                .ok()
                                .and_then(|hk| {
                                    let r = reg_query_binary(hk, EDID_VALUE);
                                    reg_close(hk);
                                    r.ok().flatten()
                                })
                                .map(|e| edid_matches_folder(&e, &folder))
                                .unwrap_or(false);
                            if matches
                                && !cands.iter().any(|c| c.pnp == folder && c.instance == inst)
                            {
                                cands.push(load_monitor_reg(&folder, &inst));
                            }
                            ii += 1;
                            if ii >= 64 {
                                break;
                            }
                        }
                        reg_close(fk);
                    }
                    pi += 1;
                    if pi >= 256 {
                        break;
                    }
                }
                reg_close(root);
            }
        }
    }
    cands
}

fn parse_detailed_timing(d: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let pclk = ((d[1] as u32) << 8) | d[0] as u32;
    if pclk == 0 {
        return None;
    }
    let w = d[2] as u32 | ((d[4] as u32 >> 4) << 8);
    let h = d[5] as u32 | ((d[7] as u32 >> 4) << 8);
    let hblank = d[3] as u32 | ((d[4] as u32 & 0x0F) << 8);
    let vblank = d[6] as u32 | ((d[7] as u32 & 0x0F) << 8);
    let htot = w + hblank;
    let vtot = h + vblank;
    let freq = if htot > 0 && vtot > 0 {
        (pclk as u64 * 10_000 / (htot as u64 * vtot as u64)) as u32
    } else {
        0
    };
    Some((w, h, freq, pclk * 10))
}

struct DetailedTiming {
    slot: usize,
    w: u32,
    h: u32,
    freq_hz: u32,
    pclk_khz: u32,
}

struct EdidInfo {
    manuf: String,
    product: u16,
    serial: u32,
    week: u8,
    year: u16,
    name: Option<String>,
    detailed: Vec<DetailedTiming>,
    ext_tags: Vec<u8>,
    base_checksum_ok: bool,
}

fn edid_name_desc(edid: &[u8]) -> Option<String> {
    if edid.len() < 128 {
        return None;
    }
    for off in [54usize, 72, 90, 108] {
        let d = &edid[off..off + 18];
        if d[0] == 0 && d[1] == 0 && d[2] == 0xFC {
            let s: String = d[5..18]
                .iter()
                .take_while(|&&c| c != 0x0A && c != 0)
                .map(|&c| c as char)
                .collect();
            let t = s.trim().to_string();
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    None
}

fn parse_edid(edid: &[u8]) -> Option<EdidInfo> {
    if edid.len() < 128 || edid.len() % 128 != 0 {
        return None;
    }
    if edid[0] != 0x00 || edid[1] != 0xFF {
        return None;
    }
    let ext_count = edid[126] as usize;
    let mut ext_tags = Vec::new();
    for i in 0..ext_count {
        let off = 128 * (i + 1);
        if off + 128 <= edid.len() {
            ext_tags.push(edid[off]);
        }
    }
    let manuf_v = ((edid[8] as u16) << 8) | edid[9] as u16;
    let cc = |x: u16| ((x & 0x1F) as u8 + b'A') as char;
    let mut detailed = Vec::new();
    for (si, off) in [54usize, 72, 90, 108].into_iter().enumerate() {
        if let Some((w, h, freq, pclk_khz)) = parse_detailed_timing(&edid[off..off + 18]) {
            detailed.push(DetailedTiming {
                slot: si + 1,
                w,
                h,
                freq_hz: freq,
                pclk_khz,
            });
        }
    }
    Some(EdidInfo {
        manuf: format!("{}{}{}", cc(manuf_v >> 10), cc(manuf_v >> 5), cc(manuf_v)),
        product: ((edid[11] as u16) << 8) | edid[10] as u16,
        serial: u32::from_le_bytes([edid[12], edid[13], edid[14], edid[15]]),
        week: edid[16],
        year: 1980 + edid[17] as u16,
        name: edid_name_desc(edid),
        detailed,
        ext_tags,
        base_checksum_ok: edid[..128].iter().map(|&b| b as u32).sum::<u32>() % 256 == 0,
    })
}

fn ext_tag_name(tag: u8) -> &'static str {
    match tag {
        0x02 => "CTA-861",
        0x10 => "Video Timing Block (VTG)",
        0x12 => "Display Information (DI-EXT)",
        0x40 => "Localized String (LS-EXT)",
        0x70 => "DisplayID",
        0x7F => "YCbCr 4:2:0 video",
        0xF0 => "extension block map",
        _ => "unknown",
    }
}

fn edid_checksum(bytes: &[u8]) -> u8 {
    ((256 - bytes.iter().map(|&b| b as u32).sum::<u32>() % 256) % 256) as u8
}

fn build_no_dsc_override(
    edid: &[u8],
    link: &str,
    bpp: u32,
) -> Result<(Vec<u8>, Vec<String>), String> {
    if edid.len() < 128 || edid.len() % 128 != 0 {
        return Err(String::from("EDID is not a whole number of 128-byte blocks"));
    }
    let ext_count = edid[126] as usize;
    if 128 * (1 + ext_count) > edid.len() {
        return Err(String::from("EDID extension count exceeds data length"));
    }
    let mut out: Vec<u8> = edid[..128].to_vec();
    let mut log: Vec<String> = Vec::new();
    for (si, off) in [54usize, 72, 90, 108].into_iter().enumerate() {
        if let Some((w, h, freq, _)) = parse_detailed_timing(&edid[off..off + 18]) {
            if needs_dsc(Mode { w, h, freq }, bpp, link)
                && out[off..off + 18].iter().any(|&b| b != 0)
            {
                for b in &mut out[off..off + 18] {
                    *b = 0;
                }
                log.push(format!(
                    "removed detailed timing #{}: {}x{} @ {} Hz (requires DSC)",
                    si + 1,
                    w,
                    h,
                    freq
                ));
            }
        }
    }
    let mut kept: Vec<&[u8]> = Vec::new();
    for i in 0..ext_count {
        let b = &edid[128 * (i + 1)..128 * (i + 2)];
        if b[0] == 0x70 || b[0] == 0xF0 {
            log.push(format!(
                "removed extension: 0x{:02X} ({})",
                b[0],
                ext_tag_name(b[0])
            ));
        } else {
            kept.push(b);
            log.push(format!(
                "kept extension: 0x{:02X} ({})",
                b[0],
                ext_tag_name(b[0])
            ));
        }
    }
    let k = kept.len();
    let mut exts: Vec<u8> = Vec::new();
    if k > 2 {
        if k > 126 {
            return Err(String::from(
                "too many remaining extension blocks for a single block map",
            ));
        }
        let mut map = vec![0u8; 128];
        map[0] = 0xF0;
        for (i, b) in kept.iter().enumerate() {
            map[i + 1] = b[0];
        }
        map[127] = edid_checksum(&map[..127]);
        exts.extend(map);
        log.push(String::from("added extension block map"));
    }
    for b in kept {
        exts.extend_from_slice(b);
    }
    out[126] = (k + if k > 2 { 1 } else { 0 }) as u8;
    out[127] = edid_checksum(&out[..127]);
    out.extend(exts);
    Ok((out, log))
}

fn write_edid_override(m: &MonitorReg, data: &[u8]) -> Result<(), String> {
    let hk = reg_open(&m.path, KEY_SET_VALUE).map_err(|e| {
        if e == ERROR_ACCESS_DENIED {
            format!(
                "access denied writing {} - run this command from an elevated terminal",
                m.path
            )
        } else {
            format!("failed to open {} (Win32 error {})", m.path, e.0)
        }
    })?;
    let name_w = to_widez(EDID_OVERRIDE_VALUE);
    let e = unsafe { RegSetValueExW(hk, pcw(&name_w), Some(0), REG_BINARY, Some(data)) };
    reg_close(hk);
    if e == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("RegSetValueExW failed (Win32 error {})", e.0))
    }
}

fn delete_edid_override(m: &MonitorReg) -> Result<bool, String> {
    let hk = reg_open(&m.path, KEY_SET_VALUE).map_err(|e| {
        if e == ERROR_ACCESS_DENIED {
            String::from("access denied - run this command from an elevated terminal")
        } else {
            format!("failed to open {} (Win32 error {})", m.path, e.0)
        }
    })?;
    let name_w = to_widez(EDID_OVERRIDE_VALUE);
    let e = unsafe { RegDeleteValueW(hk, pcw(&name_w)) };
    reg_close(hk);
    match e {
        ERROR_SUCCESS => Ok(true),
        ERROR_FILE_NOT_FOUND => Ok(false),
        other => Err(format!("RegDeleteValueW failed (Win32 error {})", other.0)),
    }
}

fn restart_graphics_driver() {
    let seq_down = [VK_LWIN, VK_CONTROL, VK_SHIFT, VIRTUAL_KEY(0x42)];
    let seq_up = [VIRTUAL_KEY(0x42), VK_SHIFT, VK_CONTROL, VK_LWIN];
    let mut inputs: Vec<INPUT> = Vec::new();
    for vk in seq_down {
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: KEYBD_EVENT_FLAGS(0),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
    }
    for vk in seq_up {
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
    }
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent == inputs.len() as u32 {
        println!("sent Win+Ctrl+Shift+B: graphics driver restarting (screen blanks ~5 s)");
    } else {
        eprintln!("SendInput only delivered {} of {} events", sent, inputs.len());
    }
}

fn edid_main(args: &[String], outs: &[Output]) {
    let mut link = String::from("dp-hbr3");
    let mut bpp = 24u32;
    let mut device: Option<String> = None;
    let mut pos: Vec<String> = Vec::new();
    let mut it = args.iter();
    let sub = it.next().map(|s| s.as_str()).unwrap_or("status");
    while let Some(a) = it.next() {
        match a.as_str() {
            "--link" => {
                if let Some(v) = it.next() {
                    link = v.clone();
                }
            }
            "--bpp" => {
                if let Some(v) = it.next() {
                    bpp = v.parse().unwrap_or(24);
                }
            }
            "--device" => {
                if let Some(v) = it.next() {
                    device = Some(v.clone());
                }
            }
            other => pos.push(other.to_string()),
        }
    }
    if link_eff_gbps(&link).is_none() {
        eprintln!("unknown link type '{}', see --help", link);
        exit(2);
    }
    let o = find_output(outs, &device);
    match sub {
        "status" => edid_status(o, &link, bpp),
        "backup" => edid_backup(o, pos.first().map(|s| s.as_str())),
        "no-dsc" => edid_no_dsc(o, &link, bpp),
        "restore" => edid_restore(o),
        "restart-driver" => restart_graphics_driver(),
        other => {
            eprintln!("unknown edid subcommand '{}'", other);
            exit(2);
        }
    }
}

fn edid_status(o: &Output, link: &str, bpp: u32) {
    println!(
        "monitor '{}' on {} (Win32 device id: '{}')",
        o.monitor,
        wide_to_string(&o.device_name),
        o.monitor_device_id
    );
    let cands = collect_monitor_candidates(o);
    if cands.is_empty() {
        eprintln!(
            "no instances matching this monitor found under {}",
            ENUM_DISPLAY_ROOT
        );
        exit(1);
    }
    println!(
        "{} matching instance(s) of {} (one per connector):",
        cands.len(),
        cands.first().map(|c| c.pnp.clone()).unwrap_or_default()
    );
    for (i, c) in cands.iter().enumerate() {
        let tag = match &c.edid {
            Some(e) => {
                let n = e.len() / 128;
                let tags: Vec<String> = (0..e[126] as usize)
                    .filter(|i| 128 * (i + 2) <= e.len())
                    .map(|i| format!("0x{:02X}", e[128 * (i + 1)]))
                    .collect();
                format!(
                    "EDID {} blocks [{}]",
                    n,
                    if tags.is_empty() {
                        String::from("no exts")
                    } else {
                        tags.join(",")
                    }
                )
            }
            None => String::from("EDID not cached"),
        };
        let ov = match &c.override_data {
            Some(v) => format!("override PRESENT ({} bytes)", v.len()),
            None => String::from("no override"),
        };
        println!(
            "  [{}] {}{}  {}  {}",
            i,
            c.instance,
            if c.active { "" } else { " (disabled)" },
            tag,
            ov
        );
    }
    let c = match cands.iter().find(|c| c.edid.is_some()) {
        Some(c) => c,
        None => {
            println!("no cached EDID to parse");
            return;
        }
    };
    let (which, data) = match (c.edid.as_ref(), c.override_data.as_ref()) {
        (Some(_e), Some(v)) => ("EDID_Override (takes precedence over live EDID)", v),
        (Some(e), None) => ("cached EDID", e),
        (None, Some(v)) => ("EDID_Override (no base EDID cached)", v),
        (None, None) => {
            println!("no EDID data available to parse");
            return;
        }
    };
    let info = match parse_edid(data) {
        Some(i) => i,
        None => {
            println!("could not parse EDID data");
            return;
        }
    };
    let ext_n = data.len() / 128 - 1;
    println!(
        "parsing {} of instance {} ({}):",
        which,
        c.instance,
        if ext_n > 0 {
            format!("{} ext blocks", ext_n)
        } else {
            String::from("no ext blocks")
        }
    );
    println!(
        "  manufacturer {}  product 0x{:04X}  serial {}  (week {} of {}, base checksum {})",
        info.manuf,
        info.product,
        info.serial,
        info.week,
        info.year,
        if info.base_checksum_ok { "OK" } else { "BAD" }
    );
    println!("  monitor name: '{}'", info.name.as_deref().unwrap_or("?"));
    for d in &info.detailed {
        let m = Mode {
            w: d.w,
            h: d.h,
            freq: d.freq_hz,
        };
        println!(
            "  detailed timing #{}: {}x{} @ {} Hz ({} MHz) -> {}",
            d.slot,
            d.w,
            d.h,
            d.freq_hz,
            d.pclk_khz / 1000,
            if needs_dsc(m, bpp, link) {
                "needs DSC (removed by `edid no-dsc`)"
            } else {
                "fits link raw (kept)"
            }
        );
    }
    for t in &info.ext_tags {
        println!("  extension: 0x{:02X} ({})", t, ext_tag_name(*t));
    }
    if info.ext_tags.contains(&0x70) {
        println!("  note: DisplayID ext carries the DSC-capable timing claims; `edid no-dsc` drops it");
    }
}

fn edid_backup(o: &Output, path: Option<&str>) {
    let cands = collect_monitor_candidates(o);
    let c = match cands.iter().find(|c| c.edid.is_some()) {
        Some(c) => c,
        None => {
            eprintln!("no instance with a cached EDID value");
            exit(1);
        }
    };
    let edid = c.edid.as_deref().unwrap();
    let path = path
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("dsc_off_edid_backup_{}.bin", c.pnp)));
    match fs::write(&path, edid) {
        Ok(_) => println!(
            "wrote {} bytes of EDID (instance {}) to {}",
            edid.len(),
            c.instance,
            path.display()
        ),
        Err(e) => {
            eprintln!("failed to write {}: {}", path.display(), e);
            exit(1);
        }
    }
    println!("note: the original EDID registry value is never modified; `edid restore` reverts any override");
}

fn edid_no_dsc(o: &Output, link: &str, bpp: u32) {
    let cands = collect_monitor_candidates(o);
    let targets: Vec<&MonitorReg> = cands.iter().filter(|c| c.edid.is_some()).collect();
    if targets.is_empty() {
        eprintln!("no instance with a cached EDID value");
        exit(1);
    }
    let mut wrote = 0;
    let mut access_denied = false;
    for c in targets {
        let edid = c.edid.as_deref().unwrap();
        let (data, log) = match build_no_dsc_override(edid, link, bpp) {
            Ok(x) => x,
            Err(e) => {
                println!("instance {}: skipped ({})", c.instance, e);
                continue;
            }
        };
        println!("instance {}:", c.instance);
        for l in &log {
            println!("  {}", l);
        }
        match write_edid_override(c, &data) {
            Ok(()) => {
                println!("  wrote EDID_Override ({} bytes)", data.len());
                wrote += 1;
            }
            Err(e) => {
                if e.contains("access denied") {
                    access_denied = true;
                } else {
                    println!("  {}", e);
                }
            }
        }
    }
    if access_denied && wrote == 0 {
        eprintln!("HKLM write failed - run this command from an elevated terminal (Start, search Terminal, right-click, Run as administrator), then rerun: dsc_off edid no-dsc");
        exit(1);
    }
    if wrote > 0 {
        println!("wrote override to {} instance(s); next: `dsc_off edid restart-driver` (or press Win+Ctrl+Shift+B) to reload EDIDs", wrote);
        println!("then: `dsc_off list` - DSC-dependent modes should be gone");
        println!("revert anytime: `dsc_off edid restore` + another driver restart");
        println!("note: if DSC modes still appear, nvlddmkm is reading DSC caps from live DPCD registers, which user space cannot override; use the mode-switch commands instead");
    }
}

fn edid_restore(o: &Output) {
    let cands = collect_monitor_candidates(o);
    let mut restored = 0;
    for c in cands.iter().filter(|c| c.override_data.is_some()) {
        match delete_edid_override(c) {
            Ok(true) => {
                println!("deleted EDID_Override at {}", c.path);
                restored += 1;
            }
            Ok(false) => {}
            Err(e) => {
                eprintln!("{}", e);
                exit(1);
            }
        }
    }
    if restored == 0 {
        println!("no EDID_Override present anywhere; nothing to restore");
    } else {
        println!("now run `dsc_off edid restart-driver` (or Win+Ctrl+Shift+B) to go back to the live sink EDID");
    }
}

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
            let mut pe = PROCESSENTRY32W::default();
            pe.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
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
        let mut mi = MONITORINFO::default();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
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

pub(crate) fn unix_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cmd_watch(cfg: &Config, dry_run: bool) {
    let outs = enumerate_outputs();
    let o = find_output(&outs, &cfg.device);
    let cur = match o.current {
        Some(c) => c,
        None => {
            eprintln!("current mode unavailable on {}", wide_to_string(&o.device_name));
            exit(1);
        }
    };
    println!(
        "watching for {} game process(es) on {} (current {}x{} @ {} Hz, poll {}s, grace {}s){}",
        cfg.games.len(),
        wide_to_string(&o.device_name),
        cur.w,
        cur.h,
        cur.freq,
        cfg.poll_secs.max(1),
        cfg.grace_secs,
        if dry_run { " [dry-run]" } else { "" }
    );
    if cfg.games.is_empty() {
        println!(
            "note: no games configured; edit {} and set e.g. games = cyberpunk2077.exe",
            CONFIG_NAME
        );
    }
    for g in &cfg.games {
        println!("  watching: {}", g);
    }
    let mut applied: Option<u32> = None;
    let mut last_seen: Option<Instant> = None;
    loop {
        thread::sleep(Duration::from_secs(cfg.poll_secs.max(1)));
        let procs = running_processes();
        let active = game_active(cfg, &procs);
        if active {
            last_seen = Some(Instant::now());
        }
        let target_hz = if active {
            Some(cfg.game_hz)
        } else if last_seen
            .map(|t| t.elapsed().as_secs() >= cfg.grace_secs)
            .unwrap_or(false)
        {
            last_seen = None;
            Some(cfg.idle_hz)
        } else {
            None
        };
        let Some(hz) = target_hz else {
            continue;
        };
        if applied == Some(hz) {
            continue;
        }
        let outs = enumerate_outputs();
        let Some((o, m)) = pick_by_freq(&outs, &cfg.device, hz) else {
            println!("[{}] no mode available at current resolution", unix_ts());
            applied = Some(hz);
            continue;
        };
        if m.freq != hz {
            println!(
                "[{}] {} Hz unavailable at {}x{}; closest is {} Hz",
                unix_ts(),
                hz,
                m.w,
                m.h,
                m.freq
            );
        }
        let reason = if active {
            "game detected"
        } else {
            "idle grace elapsed"
        };
        println!(
            "[{}] {}: applying {}x{} @ {} Hz",
            unix_ts(),
            reason,
            m.w,
            m.h,
            m.freq
        );
        if !dry_run {
            if let Err(e) = apply_mode(&o.device_name, m, true) {
                eprintln!("[{}] apply failed: {}", unix_ts(), e);
            }
        }
        applied = Some(hz);
    }
}

fn print_usage() {
    println!(
        "dsc_off {} - programmatic DSC toggle via display mode switching

DSC (Display Stream Compression) is engaged by the GPU driver per-mode whenever
the uncompressed pixel rate exceeds the link bandwidth. Switching to a mode
that fits the link raw (uncompressed) forces a link retrain without DSC;
switching back re-enables it. This tool automates that.

USAGE:
  dsc_off <command> [options]

COMMANDS:
  status            Show current mode + whether it likely requires DSC (default)
  list              Enumerate outputs and all supported modes
  off               Switch to the highest-refresh mode that does NOT need DSC
  on                Restore the highest-refresh mode (DSC resumes if needed)
  test [--secs N]   Apply DSC-off mode for N seconds, then restore (default 10)
  watch [--dry-run] [--config FILE]
                    Auto-switch by game detection: while a configured game
                    process runs, apply game_hz; grace_secs after the last
                    game exits, fall back to idle_hz. Reads dsc_off.ini
                    next to the exe (created on first run).
  config            Create/print the config file location
  applet            Launch the system tray applet (detached; icon menu:
                    Auto / game Hz / idle Hz / HDR / Exit; same dsc_off.ini)
  hdr [status|on|off|toggle]
                    Show or set Windows HDR on the primary display.
                    Uses the undocumented DisplayConfig type 16 advanced-color
                    SET (verified against type 15), with legacy DisplayConfig
                    and NVAPI as fallbacks. Diagnostics: hdr probe|probe2|
                    dump15|set15 V SIZE|types

EDID MECHANISM (overrides the sink EDID the driver reads):
  edid status       Inspect cached EDID + any active override
  edid backup [f]   Save the current EDID binary to a file
  edid no-dsc       Write an EDID_Override with DSC-dependent data removed
                    (needs an elevated terminal; revert with `edid restore`)
  edid restore      Delete the EDID_Override (back to live sink EDID)
  edid restart-driver
                    Restart the graphics driver via Win+Ctrl+Shift+B keystroke
                    (required after no-dsc/restore for changes to take effect)

OPTIONS:
  --device NAME     Output to act on, e.g. DISPLAY1 (default: first output)
  --link TYPE       Link bandwidth assumption:
                      dp-hbr2 dp-hbr3 dp-uhbr10 dp-uhbr13 dp-uhbr20
                      hdmi20 hdmi21-frl3 hdmi21-frl4 hdmi21-frl5 hdmi21-frl6
                    (default: dp-hbr3; use dp-uhbr13 for RTX 40-series +
                     DP 2.1 displays; `status` shows all link classes)
  --bpp N           Bits per pixel for bandwidth math (default 24 = 8b/RGB444)
  --secs N          Duration for `test` (default 10)

NOTES:
  `off`/`on` persist via the registry; `test` is temporary.
  The DSC verdict is a bandwidth heuristic, not a driver query: Windows exposes
  no public API to read the sink's DSC state.
  `edid no-dsc` needs admin (HKLM write) and only applies after a driver restart.
  edid subcommands accept --device/--link/--bpp like the rest of the tool.",
        env!("CARGO_PKG_VERSION")
    );
}

fn main() {
    let raw: Vec<String> = env::args().skip(1).collect();
    if raw.first().map(|s| s.as_str()) == Some("edid") {
        let outs = enumerate_outputs();
        if outs.is_empty() {
            eprintln!("no active display outputs found");
            exit(1);
        }
        edid_main(&raw[1..], &outs);
        return;
    }
    if raw.first().map(|s| s.as_str()) == Some("hdr") {
        let sub = raw.get(1).map(|s| s.as_str()).unwrap_or("status");
        match sub {
            "status" => match hdr::hdr_state() {
                Ok(Some(i)) => {
                    println!(
                        "HDR: {}  (supported: yes, wide-color: {}, force-disabled: {})",
                        if i.enabled { "on" } else { "off" },
                        i.wide_color,
                        i.force_disabled
                    );
                    println!(
                        "  bits-per-color-channel: {}  color-encoding: {}",
                        i.bits_per_channel,
                        if i.color_encoding_rgb { "RGB" } else { "other" }
                    );
                    match nvapi::nvapi_hdr_mode("\\\\.\\DISPLAY1") {
                        Ok(m) => println!(
                            "  nvapi hdrMode: {} ({})",
                            m,
                            match m {
                                0 => "OFF",
                                2 => "UHDA/HDR10",
                                _ => "other",
                            }
                        ),
                        Err(e) => println!("  nvapi hdrMode: unavailable ({})", e),
                    }
                    match nvapi::nvapi_hdr_capabilities("\\\\.\\DISPLAY1") {
                        Ok(c) => {
                            println!(
                                "  nvapi caps: st2084={} traditionalHdr={} edr={} driverExpand={}",
                                c.st2084_supported,
                                c.traditional_hdr_supported,
                                c.edr_supported,
                                c.driver_expand
                            );
                            println!(
                                "  nvapi caps metadata: {:02X?}",
                                c.metadata
                            );
                        }
                        Err(e) => println!("  nvapi caps: unavailable ({})", e),
                    }
                }
                Ok(None) => println!("HDR: not supported on the primary display"),
                Err(e) => {
                    eprintln!("{}", e);
                    exit(1);
                }
            },
            "on" | "off" => {
                let on = sub == "on";
                match hdr::hdr_set_verified(on) {
                    Ok(m) => println!(
                        "HDR {} ({})",
                        if on { "on" } else { "off" },
                        match m {
                            hdr::HdrMethod::DisplayConfig => "type16 DisplayConfig SET",
                            hdr::HdrMethod::Legacy => "legacy DisplayConfig SET",
                            hdr::HdrMethod::Nvapi => "NVAPI",
                        }
                    ),
                    Err(e) => {
                        eprintln!("{}", e);
                        exit(1);
                    }
                }
            }
            "set15" => {
                let value: u32 = raw.get(2).and_then(|v| v.parse().ok()).unwrap_or(2);
                let size: u32 = raw.get(3).and_then(|v| v.parse().ok()).unwrap_or(36);
                match hdr::set_type15(value, size) {
                    Ok(()) => {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        println!(
                            "set15({} bytes -> value {}) applied; state now {:?}, nvapi {:?}",
                            size,
                            value,
                            hdr::type15_state(),
                            nvapi::nvapi_hdr_mode("\\\\.\\DISPLAY1")
                        );
                    }
                    Err(e) => {
                        eprintln!("{}", e);
                        exit(1);
                    }
                }
            }
            "dump15" => match hdr::dump_type15() {
                Ok(b) => println!(
                    "type15 (36 bytes): {}",
                    b.iter().map(|x| format!("{:02X}", x)).collect::<String>()
                ),
                Err(e) => {
                    eprintln!("{}", e);
                    exit(1);
                }
            },
            "types" => {
                for (ty, sz, _err) in hdr::probe_device_info_types() {
                    if sz > 0 {
                        println!("type {:>3}: OK at size {}", ty, sz);
                    }
                }
            }
            "probe" => {
                let cur = match hdr::hdr_enabled() {
                    Some(v) => v,
                    None => {
                        eprintln!("HDR not supported");
                        exit(1);
                    }
                };
                let want = !cur;
                println!(
                    "probe: nvapi SET({}) from mode {}, 10s dual-oracle timeline:",
                    want,
                    if cur { 2 } else { 0 }
                );
                if let Err(e) = nvapi::nvapi_hdr_set("\\\\.\\DISPLAY1", want) {
                    println!("  set error: {}", e);
                }
                for i in 0..20 {
                    let m = nvapi::nvapi_hdr_mode("\\\\.\\DISPLAY1");
                    let os = hdr::hdr_state().ok().flatten().map(|s| s.enabled);
                    println!(
                        "  +{:>5} ms: nvapi={:?} os_get={:?}",
                        500 * (i + 1),
                        m,
                        os
                    );
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                println!("  --- watching 90s for delayed reversion (changes only) ---");
                let mut last = nvapi::nvapi_hdr_mode("\\\\.\\DISPLAY1");
                for i in 0..90 {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    let m = nvapi::nvapi_hdr_mode("\\\\.\\DISPLAY1");
                    if m != last {
                        println!("  +{} s: nvapi {} -> {:?}", 10 + i + 1, last.unwrap_or(99), m);
                        last = m;
                    }
                }
                println!("  watch done, final nvapi={:?}", last);
            }
            "probe2" => {
                let cur = match hdr::hdr_state() {
                    Ok(Some(i)) => i.enabled,
                    Ok(None) => {
                        eprintln!("HDR not supported");
                        exit(1);
                    }
                    Err(e) => {
                        eprintln!("{}", e);
                        exit(1);
                    }
                };
                let want = !cur;
                println!(
                    "probe2: attempting HDR -> {} (current {}), GET timeline after SET:",
                    want, cur
                );
                for (ms, state) in hdr::hdr_probe_timeline(want) {
                    println!(
                        "  +{:>4} ms: {}",
                        ms,
                        match state {
                            Some(true) => "on",
                            Some(false) => "off",
                            None => "?",
                        }
                    );
                }
                println!("final: {:?}", hdr::hdr_state().map(|r| r.map(|i| i.enabled)));
            }
            "toggle" => match hdr::hdr_enabled() {
                Some(cur) => {
                    let on = !cur;
                    match hdr::hdr_set_verified(on) {
                        Ok(_) => println!("HDR {}", if on { "on" } else { "off" }),
                        Err(e) => {
                            eprintln!("{}", e);
                            exit(1);
                        }
                    }
                }
                None => println!("HDR: not supported on the primary display"),
            },
            other => {
                eprintln!("unknown hdr subcommand '{}' (use status|on|off|toggle)", other);
                exit(2);
            }
        }
        return;
    }
    if raw.first().map(|s| s.as_str()) == Some("detect") {
        let path = config_path(None);
        let cfg = load_or_create_config(&path);
        loop {
            let procs = running_processes();
            let named = cfg
                .games
                .iter()
                .any(|g| procs.iter().any(|p| p.eq_ignore_ascii_case(g)));
            let fs = detect_fullscreen_game();
            let gpu = match gpu_game_load() {
                    Ok(v) => { if v.is_none() { println!("gpu: no engtype_3d instances above 0 (raw path OK)"); } v },
                Err(e) => {
                    println!("[{}] gpu error: {}", unix_ts(), e);
                    None
                }
            };
            let gpu_hit = gpu.map(|(_, v)| v >= cfg.gpu_threshold as f64).unwrap_or(false);
            println!(
                "[{}] named={} fullscreen={} gpu={}",
                unix_ts(),
                named,
                fs,
                match (&gpu, gpu_hit) {
                    (Some((pid, v)), _) =>
                        format!("{:.1}% (pid {})", v, pid),
                    (None, _) => String::from("n/a"),
                }
            );
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    if raw.first().map(|s| s.as_str()) == Some("watch") || raw.first().map(|s| s.as_str()) == Some("config") {        let mut cfg_path: Option<String> = None;
        let mut dry_run = false;
        let mut it = raw[1..].iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--config" => cfg_path = it.next().cloned(),
                "--dry-run" => dry_run = true,
                _ => {}
            }
        }
        let path = config_path(cfg_path.as_deref());
        let cfg = load_or_create_config(&path);
        if raw[0] == "config" {
            println!("config at {}", path.display());
            return;
        }
        cmd_watch(&cfg, dry_run);
        return;
    }
    if raw.first().map(|s| s.as_str()) == Some("applet") {
        let background = raw.iter().any(|a| a == "--bg");
        if background {
            let path = config_path(None);
            let cfg = load_or_create_config(&path);
            tray::run(cfg);
        } else {
            let exe = env::current_exe().unwrap_or_else(|_| PathBuf::from("dsc_off.exe"));
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            match std::process::Command::new(&exe)
                .args(["applet", "--bg"])
                .creation_flags(DETACHED_PROCESS)
                .spawn()
            {
                Ok(_) => println!("applet launched in the system tray (log: exe dir/dsc_off_tray.log)"),
                Err(e) => {
                    eprintln!("failed to spawn applet: {}", e);
                    exit(1);
                }
            }
        }
        return;
    }
    let (cmd, device, link, bpp, secs) = parse_args();
    let outs = enumerate_outputs();
    if outs.is_empty() {
        eprintln!("no active display outputs found");
        exit(1);
    }
    match cmd.as_str() {
        "status" => {
            for o in &outs {
                println!(
                    "{}  [{} / {}]",
                    wide_to_string(&o.device_name),
                    o.adapter,
                    o.monitor
                );
                match o.current {
                    Some(c) => {
                        let need = mode_bitrate_gbps(c, bpp) * BLANKING_FACTOR;
                        println!(
                            "  current: {}x{} @ {} Hz  ({} bpp, incl. blanking: {:.1} Gbps)",
                            c.w, c.h, c.freq, bpp, need
                        );
                        println!("  DSC verdict per possible link:");
                        for l in [
                            "dp-hbr2",
                            "dp-hbr3",
                            "dp-uhbr10",
                            "dp-uhbr13",
                            "dp-uhbr20",
                            "hdmi20",
                            "hdmi21-frl6",
                        ] {
                            let eff = link_eff_gbps(l).unwrap_or(0.0);
                            let d = needs_dsc(c, bpp, l);
                            println!(
                                "    {:<12} {:>6.2} Gbps eff -> {}",
                                l,
                                eff,
                                if d {
                                    "needs DSC"
                                } else {
                                    "fits uncompressed"
                                }
                            );
                        }
                    }
                    None => println!("  current mode unavailable"),
                }
            }
            let target = pick_dsc_free(&outs, &device, bpp, &link);
            match target {
                Some((o, m)) => println!(
                    "  dsc_off candidate: {} -> {}x{} @ {} Hz (raw link, no DSC)",
                    wide_to_string(&o.device_name),
                    m.w,
                    m.h,
                    m.freq
                ),
                None => println!("  no DSC-free mode available at current resolution"),
            }
        }
        "list" => {
            for o in &outs {
                println!(
                    "{}  [{} / {}]",
                    wide_to_string(&o.device_name),
                    o.adapter,
                    o.monitor
                );
                for m in &o.modes {
                    print_mode_line(*m, bpp, &link, &o.current);
                }
            }
        }
        "off" => {
            let o = find_output(&outs, &device);
            let cur = o.current.expect("no current mode");
            match pick_dsc_free(std::slice::from_ref(o), &None, bpp, &link) {
                Some((_, m)) => {
                    if m.freq == cur.freq {
                        println!(
                            "current mode {}x{} @ {} Hz already fits the {} link raw; nothing to do",
                            cur.w, cur.h, cur.freq, link
                        );
                        return;
                    }
                    apply_mode(&o.device_name, m, true)
                        .unwrap_or_else(|e| exit_with(&e));
                    println!(
                        "DSC OFF: {}x{} @ {} Hz (was {}x{} @ {} Hz)",
                        m.w, m.h, m.freq, cur.w, cur.h, cur.freq
                    );
                }
                None => {
                    eprintln!(
                        "no mode at {}x{} fits {} raw; lower --bpp or pick another link",
                        cur.w, cur.h, link
                    );
                    exit(1);
                }
            }
        }
        "on" => {
            let o = find_output(&outs, &device);
            let cur = o.current.expect("no current mode");
            let best = o
                .modes
                .iter()
                .filter(|m| m.w == cur.w && m.h == cur.h)
                .max_by_key(|m| m.freq)
                .copied();
            match best {
                Some(m) => {
                    if m.freq == cur.freq {
                        println!("already at max refresh {} Hz; nothing to do", m.freq);
                        return;
                    }
                    apply_mode(&o.device_name, m, true).unwrap_or_else(|e| exit_with(&e));
                    println!(
                        "restored: {}x{} @ {} Hz{}",
                        m.w,
                        m.h,
                        m.freq,
                        if needs_dsc(m, bpp, &link) { " (DSC engaged)" } else { "" }
                    );
                }
                None => {
                    eprintln!("no modes at {}x{}", cur.w, cur.h);
                    exit(1);
                }
            }
        }
        "test" => {
            let secs = secs.unwrap_or(10);
            let o = find_output(&outs, &device);
            let cur = o.current.expect("no current mode");
            let target = pick_dsc_free(std::slice::from_ref(o), &None, bpp, &link);
            let m = match target {
                Some((_, m)) => m,
                None => {
                    eprintln!("no DSC-free mode available at current resolution");
                    exit(1);
                }
            };
            if m.freq == cur.freq {
                println!("current mode already DSC-free; nothing to test");
                return;
            }
            apply_mode(&o.device_name, m, false).unwrap_or_else(|e| exit_with(&e));
            println!("DSC OFF for {}s ({}x{} @ {} Hz)...", secs, m.w, m.h, m.freq);
            thread::sleep(Duration::from_secs(secs));
            apply_mode(&o.device_name, cur, false).unwrap_or_else(|e| exit_with(&e));
            println!("restored {}x{} @ {} Hz", cur.w, cur.h, cur.freq);
        }
        other => {
            eprintln!("unknown command '{}'\n", other);
            print_usage();
            exit(2);
        }
    }
}

fn exit_with(msg: &str) -> ! {
    eprintln!("{}", msg);
    exit(1);
}

fn pick_dsc_free<'a>(
    outs: &'a [Output],
    device: &Option<String>,
    bpp: u32,
    link: &str,
) -> Option<(&'a Output, Mode)> {
    let o = find_output(outs, device);
    let cur = o.current?;
    o.modes
        .iter()
        .filter(|m| m.w == cur.w && m.h == cur.h && m.freq <= cur.freq)
        .filter(|m| !needs_dsc(**m, bpp, link))
        .max_by_key(|m| m.freq)
        .map(|m| (o, *m))
}
