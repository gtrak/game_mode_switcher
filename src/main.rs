use std::{
    env,
    path::PathBuf,
    process::exit,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows::core::PCWSTR;
use windows::Win32::Graphics::Gdi::{
    ChangeDisplaySettingsExW, EnumDisplayDevicesW, EnumDisplaySettingsExW, CDS_TYPE,
    CDS_UPDATEREGISTRY, DEVMODEW, DISP_CHANGE_SUCCESSFUL, DISPLAY_DEVICEW,
    DISPLAY_DEVICE_ACTIVE, DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, DM_DISPLAYFREQUENCY,
    DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_FLAGS,
    ENUM_DISPLAY_SETTINGS_MODE,
};

const BLANKING_FACTOR: f64 = 1.12;

mod edid;
mod games;
mod hdr;
mod nvapi;
mod tray;

use edid::edid_main;
pub(crate) use games::{
    config_path, detect_fullscreen_game, game_active, gpu_loads, load_or_create_config,
    mode_spec_label, pick_mode, running_processes, ModeSpec, CONFIG_NAME, Config,
};

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

pub(crate) fn link_eff_gbps(link: &str) -> Option<f64> {
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
        let mut dd = DISPLAY_DEVICEW {
            cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        unsafe {
            if !EnumDisplayDevicesW(PCWSTR::null(), idx, &mut dd, 0).as_bool() {
                break;
            }
        }
        if (dd.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP) == DISPLAY_DEVICE_ATTACHED_TO_DESKTOP
            && (dd.StateFlags & DISPLAY_DEVICE_ACTIVE) == DISPLAY_DEVICE_ACTIVE
        {
            let mut mon = DISPLAY_DEVICEW {
                cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
                ..Default::default()
            };
            let (monitor, monitor_device_id) = unsafe {
                if EnumDisplayDevicesW(pcw(&dd.DeviceName), 0, &mut mon, 0).as_bool() {
                    (wide_to_string(&mon.DeviceString), wide_to_string(&mon.DeviceID))
                } else {
                    (String::from("<unknown monitor>"), String::new())
                }
            };
            let mut current = None;
            {
                let mut dm = DEVMODEW {
                    dmSize: std::mem::size_of::<DEVMODEW>() as u16,
                    ..Default::default()
                };
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
                let mut dm = DEVMODEW {
                    dmSize: std::mem::size_of::<DEVMODEW>() as u16,
                    ..Default::default()
                };
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
    let mut dm = DEVMODEW {
        dmSize: std::mem::size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
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
        eprintln!("output '{}' not found; run `game_mode_switcher list`", want);
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
    let mut applied: Option<ModeSpec> = None;
    let mut last_seen: Option<Instant> = None;
    let mut last_active: Option<bool> = None;
    loop {
        thread::sleep(Duration::from_secs(cfg.poll_secs.max(1)));
        let procs = running_processes();
        let active = game_active(cfg, &procs);
        if active {
            last_seen = Some(Instant::now());
        }
        let (spec, is_active) = if active {
            (cfg.auto_game, true)
        } else if last_seen
            .map(|t| t.elapsed().as_secs() >= cfg.grace_secs)
            .unwrap_or(false)
        {
            last_seen = None;
            (cfg.auto_idle, false)
        } else {
            continue;
        };
        let state_changed = last_active != Some(is_active);
        if applied == Some(spec) && !state_changed {
            continue;
        }
        let outs = enumerate_outputs();
        let Some((o, m)) = pick_mode(&outs, &cfg.device, &spec) else {
            println!(
                "[{}] no mode for {} at this resolution",
                unix_ts(),
                mode_spec_label(&spec)
            );
            applied = Some(spec);
            continue;
        };
        if spec.hz != 0 && m.freq != spec.hz {
            println!(
                "[{}] {} unavailable; closest is {}x{} @ {} Hz",
                unix_ts(),
                mode_spec_label(&spec),
                m.w,
                m.h,
                m.freq
            );
        }
        let reason = if is_active {
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
            } else {
                let want = if is_active {
                    cfg.auto_game_hdr
                } else {
                    cfg.auto_idle_hdr
                };
                if let Some(want) = want {
                    match hdr::hdr_set_verified(want) {
                        Ok(_) => println!("[{}] hdr -> {}", unix_ts(), want),
                        Err(e) => eprintln!("[{}] hdr set failed: {}", unix_ts(), e),
                    }
                }
            }
        }
        applied = Some(spec);
        last_active = Some(is_active);
    }
}

fn print_usage() {
    println!(
        "game_mode_switcher {} - programmatic DSC toggle via display mode switching

DSC (Display Stream Compression) is engaged by the GPU driver per-mode whenever
the uncompressed pixel rate exceeds the link bandwidth. Switching to a mode
that fits the link raw (uncompressed) forces a link retrain without DSC;
switching back re-enables it. This tool automates that.

USAGE:
  game_mode_switcher <command> [options]

COMMANDS:
  status            Show current mode + whether it likely requires DSC (default)
  list              Enumerate outputs and all supported modes
  off               Switch to the highest-refresh mode that does NOT need DSC
  on                Restore the highest-refresh mode (DSC resumes if needed)
  test [--secs N]   Apply DSC-off mode for N seconds, then restore (default 10)
  watch [--dry-run] [--config FILE]
                    Auto-switch by game detection: while a game runs apply
                    game_mode (and game_hdr if set); grace_secs after the
                    last game exits, fall back to idle_mode/idle_hdr. Reads
                    game_mode_switcher.ini next to the exe (created on first run).
  detect            Show the live game-detection signals: Known Games List
                    processes (with per-process 3D GPU load), configured
                    games, and fullscreen state. Useful for tuning the ini.
  config            Create/print the config file location
  applet            Launch the system tray applet (detached; left-click
                    toggles HDR, right-click menu: modes / Auto / Exit;
                    same game_mode_switcher.ini)
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
        let kgl0 = games::known_game_exes();
        println!(
            "detect: {} known-game exe(s) from GameConfigStore, {} configured",
            kgl0.len(),
            cfg.games.len()
        );
        loop {
            let procs = running_processes();
            let named = cfg
                .games
                .iter()
                .any(|g| procs.iter().any(|p| p.name.eq_ignore_ascii_case(g)));
            let kgl = games::known_game_exes();
            let loads = gpu_loads().unwrap_or_default();
            let name_of = |pid: u32| {
                procs
                    .iter()
                    .find(|p| p.pid == pid)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| format!("pid {}", pid))
            };
            let kgl_hit = !kgl.is_empty()
                && procs
                    .iter()
                    .any(|p| {
                        kgl.iter().any(|k| p.name.eq_ignore_ascii_case(k))
                            && loads
                                .iter()
                                .any(|(lpid, v)| *lpid == p.pid && *v >= cfg.kgl_min_gpu as f64)
                    });
            let fs = detect_fullscreen_game();
            let top: Vec<String> = loads
                .iter()
                .take(3)
                .map(|(pid, v)| format!("{} {:.1}%", name_of(*pid), v))
                .collect();
            let gpu_hit = loads.first().map(|(_, v)| *v).unwrap_or(0.0)
                >= cfg.gpu_threshold as f64;
            println!(
                "[{}] named={} kgl={} fullscreen={} gpu_hit={} top3=[{}]",
                unix_ts(),
                named,
                kgl_hit,
                fs,
                gpu_hit,
                top.join(", ")
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
            let exe = env::current_exe().unwrap_or_else(|_| PathBuf::from("game_mode_switcher.exe"));
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            match std::process::Command::new(&exe)
                .args(["applet", "--bg"])
                .creation_flags(DETACHED_PROCESS)
                .spawn()
            {
                Ok(_) => println!("applet launched in the system tray (log: exe dir/game_mode_switcher_tray.log)"),
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
                    "  game_mode_switcher candidate: {} -> {}x{} @ {} Hz (raw link, no DSC)",
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
