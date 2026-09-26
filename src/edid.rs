use std::{fs, path::PathBuf, process::exit};
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VK_CONTROL, VK_LWIN, VK_SHIFT, VIRTUAL_KEY,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegEnumKeyW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, REG_BINARY, REG_SAM_FLAGS,
};

use crate::{find_output, link_eff_gbps, needs_dsc, pcw, to_widez, wide_to_string, Mode, Output};
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
            cands.push(load_monitor_reg(folder, &inst));
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
    if edid.len() < 128 || !edid.len().is_multiple_of(128) {
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
    if edid.len() < 128 || !edid.len().is_multiple_of(128) {
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

pub(crate) fn edid_main(args: &[String], outs: &[Output]) {
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
