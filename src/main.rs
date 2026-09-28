use std::{
    env,
    path::PathBuf,
    process::exit,
    thread,
    time::Duration,
};

mod auto;
mod config;
mod display;
mod games;
mod hdr;
mod nvapi;
mod tray;
mod util;

pub(crate) use display::{
    apply_mode, enumerate_outputs, find_output, pick_mode, print_mode_line, print_output_header,
};
pub(crate) use util::{pcw, to_widez, unix_ts, wide_to_string};

pub(crate) use config::{
    config_path, load_or_create_config, mode_spec_label, ModeSpec, CONFIG_NAME, Config,
};

pub(crate) use games::{game_active, gpu_loads, running_processes};

pub(crate) const PRIMARY_DISPLAY: &str = "\\\\.\\DISPLAY1";

struct Args {
    cmd: String,
    config: Option<String>,
    dry_run: bool,
    bg: bool,
}

fn parse_args(args: &[String]) -> Args {
    if matches!(
        args.first().map(|s| s.as_str()),
        Some("-h" | "--help" | "help")
    ) {
        print_usage();
        exit(0);
    }
    let mut cmd = String::new();
    let mut config: Option<String> = None;
    let mut dry_run = false;
    let mut bg = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--config" => config = it.next().cloned(),
            "--dry-run" => dry_run = true,
            "--bg" => bg = true,
            other => {
                // first non-flag token is the command; unknown flags are
                // silently ignored
                if !other.starts_with('-') && cmd.is_empty() {
                    cmd = other.to_string();
                }
            }
        }
    }
    if cmd.is_empty() {
        cmd = String::from("status");
    }
    Args {
        cmd,
        config,
        dry_run,
        bg,
    }
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
    let mut sw = crate::auto::AutoSwitcher::new();
    loop {
        thread::sleep(Duration::from_secs(cfg.poll_secs.max(1)));
        let procs = running_processes();
        let active = game_active(cfg, &procs);
        // watch keeps its own reason phrasing below ("game detected" /
        // "idle grace elapsed"); the reason returned by step is ignored.
        let Some((spec, is_active, state_changed, _reason)) = sw.step(cfg, active) else {
            continue;
        };
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
                let want = cfg.hdr_for(is_active);
                if let Some(want) = want {
                    match hdr::hdr_set_verified(want) {
                        Ok(_) => println!("[{}] hdr -> {}", unix_ts(), want),
                        Err(e) => eprintln!("[{}] hdr set failed: {}", unix_ts(), e),
                    }
                }
            }
        }
        applied = Some(spec);
        sw.last_active = Some(is_active);
    }
}

fn cmd_status() {
    let outs = enumerate_outputs();
    if outs.is_empty() {
        eprintln!("no active display outputs found");
        exit(1);
    }
    for o in &outs {
        print_output_header(o);
        match o.current {
            Some(c) => println!(
                "  current: {}x{} @ {} Hz",
                c.w, c.h, c.freq
            ),
            None => println!("  current mode unavailable"),
        }
    }
}

fn cmd_list() {
    let outs = enumerate_outputs();
    if outs.is_empty() {
        eprintln!("no active display outputs found");
        exit(1);
    }
    for o in &outs {
        print_output_header(o);
        for m in &o.modes {
            print_mode_line(*m, &o.current);
        }
    }
}

fn cmd_hdr(sub: &str) {
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
}

fn cmd_detect() {
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
        let sig = games::detect_signals(&cfg, &procs);
        let loads = gpu_loads().unwrap_or_default();
        let name_of = |pid: u32| {
            procs
                .iter()
                .find(|p| p.pid == pid)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| format!("pid {}", pid))
        };
        let top: Vec<String> = loads
            .iter()
            .take(3)
            .map(|(pid, v)| format!("{} {:.1}%", name_of(*pid), v))
            .collect();
        println!(
            "[{}] named={} kgl={} fullscreen={} gpu_hit={} top3=[{}]",
            unix_ts(),
            sig.named,
            sig.kgl,
            sig.fullscreen,
            sig.gpu,
            top.join(", ")
        );
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn cmd_config(path: Option<&str>) {
    let path = config_path(path);
    load_or_create_config(&path);
    println!("config at {}", path.display());
}

fn cmd_applet(background: bool) {
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
  --config FILE   Config file for watch/config (default: game_mode_switcher.ini next to the exe)
  --dry-run       watch: log actions without applying them
  --bg            applet: run in-process (used by the detached launcher)",
        env!("CARGO_PKG_VERSION")
    );
}

fn main() {
    let raw: Vec<String> = env::args().skip(1).collect();
    let args = parse_args(&raw);
    match args.cmd.as_str() {
        "status" => cmd_status(),
        "list" => cmd_list(),
        "hdr" => cmd_hdr(raw.get(1).map(|s| s.as_str()).unwrap_or("status")),
        "detect" => cmd_detect(),
        "watch" => {
            let path = config_path(args.config.as_deref());
            let cfg = load_or_create_config(&path);
            cmd_watch(&cfg, args.dry_run);
        }
        "config" => cmd_config(args.config.as_deref()),
        "applet" => cmd_applet(args.bg),
        other => {
            eprintln!("unknown command '{}'\n", other);
            print_usage();
            exit(2);
        }
    }
}
