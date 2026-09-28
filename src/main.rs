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

mod games;
mod hdr;
mod nvapi;
mod tray;

pub(crate) use games::{
    config_path, detect_fullscreen_game, game_active, gpu_loads, load_or_create_config,
    mode_spec_label, pick_mode, running_processes, ModeSpec, CONFIG_NAME, Config,
};

pub(crate) const PRIMARY_DISPLAY: &str = "\\\\.\\DISPLAY1";

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
    pub(crate) current: Option<Mode>,
    pub(crate) modes: Vec<Mode>,
}

pub(crate) fn wide_to_string(ws: &[u16]) -> String {
    let end = ws.iter().position(|&c| c == 0).unwrap_or(ws.len());
    String::from_utf16_lossy(&ws[..end])
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
            let monitor = unsafe {
                if EnumDisplayDevicesW(pcw(&dd.DeviceName), 0, &mut mon, 0).as_bool() {
                    wide_to_string(&mon.DeviceString)
                } else {
                    String::from("<unknown monitor>")
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

fn parse_args() -> (String, Option<String>) {
    let mut cmd = String::from("status");
    let mut device: Option<String> = None;
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
        if a.as_str() == "--device" {
            device = it.next().cloned();
        }
    }
    (cmd, device)
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

fn print_mode_line(m: Mode, current: &Option<Mode>) {
    let cur = if Some(m) == *current { " [current]" } else { "" };
    println!("    {}x{} @ {:>3} Hz{}", m.w, m.h, m.freq, cur);
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

fn hdr_fail(e: String) -> ! {
    eprintln!("{}", e);
    exit(1);
}

fn print_usage() {
    println!(
        "game_mode_switcher {} - tray applet + CLI for display mode and HDR switching

Automatically switches your display between game mode (high refresh) and
desktop mode (lower refresh) based on running games, with optional HDR
control. The tray applet is the primary interface; the CLI is scriptable.

USAGE:
  game_mode_switcher <command> [options]

COMMANDS:
  status            Show current display mode (default)
  list              Enumerate outputs and all supported modes
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

OPTIONS:
  --device NAME     Output to act on, e.g. DISPLAY1 (default: first output)",
        env!("CARGO_PKG_VERSION")
    );
}

fn main() {
    let raw: Vec<String> = env::args().skip(1).collect();
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
                    match nvapi::nvapi_hdr_mode(PRIMARY_DISPLAY) {
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
                    match nvapi::nvapi_hdr_capabilities(PRIMARY_DISPLAY) {
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
                Err(e) => hdr_fail(e),
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
                    Err(e) => hdr_fail(e),
                }
            }
            "toggle" => match hdr::hdr_enabled() {
                Some(cur) => {
                    let on = !cur;
                    match hdr::hdr_set_verified(on) {
                        Ok(_) => println!("HDR {}", if on { "on" } else { "off" }),
                        Err(e) => hdr_fail(e),
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
    let (cmd, _device) = parse_args();
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
                    Some(c) => println!(
                        "  current: {}x{} @ {} Hz",
                        c.w, c.h, c.freq
                    ),
                    None => println!("  current mode unavailable"),
                }
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
                    print_mode_line(*m, &o.current);
                }
            }
        }
        other => {
            eprintln!("unknown command '{}'\n", other);
            print_usage();
            exit(2);
        }
    }
}
