use std::{
    env,
    fs,
    path::{Path, PathBuf},
    process::exit,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ModeSpec {
    pub(crate) w: u32,
    pub(crate) h: u32,
    pub(crate) hz: u32,
}

pub(crate) fn parse_mode_spec(s: &str) -> Option<ModeSpec> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (res, hz) = match s.split_once('@') {
        Some((r, h)) => (r, h.trim().parse::<u32>().ok()),
        None => (s, None),
    };
    let (w, h) = if let Some((w, h)) = res.split_once(['x', 'X']) {
        (w.trim().parse().ok()?, h.trim().parse().ok()?)
    } else {
        (0, 0)
    };
    let hz = match hz {
        Some(v) => v,
        None => {
            if w == 0 {
                s.parse().ok()?
            } else {
                0
            }
        }
    };
    Some(ModeSpec { w, h, hz })
}

pub(crate) fn mode_spec_label(sp: &ModeSpec) -> String {
    if sp.w == 0 {
        format!("{} Hz", sp.hz)
    } else if sp.hz == 0 {
        format!("{}x{}", sp.w, sp.h)
    } else {
        format!("{}x{} @ {} Hz", sp.w, sp.h, sp.hz)
    }
}

fn parse_mode_specs(v: &str) -> Vec<ModeSpec> {
    v.split(',').filter_map(parse_mode_spec).collect()
}

fn parse_hdr_opt(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "yes" => Some(true),
        "off" | "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

pub(crate) struct Config {
    pub(crate) device: Option<String>,
    pub(crate) poll_secs: u64,
    pub(crate) grace_secs: u64,
    pub(crate) modes: Vec<ModeSpec>,
    pub(crate) auto_game: ModeSpec,
    pub(crate) auto_idle: ModeSpec,
    pub(crate) auto_game_hdr: Option<bool>,
    pub(crate) auto_idle_hdr: Option<bool>,
    pub(crate) auto_enabled_on_start: bool,
    pub(crate) fullscreen_detect: bool,
    pub(crate) gpu_load_detect: bool,
    pub(crate) gpu_threshold: u32,
    pub(crate) games: Vec<String>,
    pub(crate) games_ignore: Vec<String>,
    pub(crate) kgl_min_gpu: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            device: None,
            poll_secs: 2,
            grace_secs: 15,
            modes: vec![
                ModeSpec { w: 0, h: 0, hz: 240 },
                ModeSpec { w: 0, h: 0, hz: 120 },
            ],
            auto_game: ModeSpec { w: 0, h: 0, hz: 240 },
            auto_idle: ModeSpec { w: 0, h: 0, hz: 120 },
            auto_game_hdr: None,
            auto_idle_hdr: None,
            auto_enabled_on_start: true,
            games: Vec::new(),
            games_ignore: Vec::new(),
            fullscreen_detect: false,
            gpu_load_detect: false,
            gpu_threshold: 35,
            kgl_min_gpu: 5,
        }
    }
}

pub(crate) const CONFIG_NAME: &str = "game_mode_switcher.ini";

const CONFIG_TEMPLATE: &str = "# game_mode_switcher configuration (used by `watch` and the tray applet)
# restrict to one output, e.g. DISPLAY1 (comment out = first output)
# device = DISPLAY1
# polling interval for game detection, seconds
poll_secs = 2
# seconds after the last game exits before Auto falls back to idle_mode
grace_secs = 15

# ----- modes -----
# entries shown in the tray menu; each is a refresh rate ('240') or a full
# mode line ('3840x2160@240'). A rate-only entry keeps the current
# resolution; a full line without '@hz' keeps the current refresh rate.
# If an exact mode does not exist, the closest lower-refresh mode at that
# resolution is used instead.
modes = 240, 120

# ----- auto -----
# mode applied when a game is detected / when idle (same syntax as modes)
game_mode = 240
idle_mode = 120
# optionally force HDR when entering game/idle mode (on/off; empty = leave
# HDR as-is - toggle it any time with a left-click on the tray icon)
game_hdr =
idle_hdr =
# run auto on applet / watch launch (syncs mode + HDR immediately)
enabled_on_start = true

# ----- game detection -----
# extra game executables to watch for, comma separated, case-insensitive
# NOTE: Windows' own Known Games List (GameConfigStore, populated by Game
# Bar/Game Mode) is matched automatically - you only need to add exes here
# that Windows has not classified yet
# example: games = cyberpunk2077.exe, eldenring.exe, hfw.exe
games =

# Known Games List entries to ignore even when running with GPU load
# example: games_ignore = robloxplayerbeta.exe, oculus-client.exe
games_ignore =
# minimum 3D GPU utilization (%) a Known Games List process must show to
# count as a running game (background launchers idle near 0%)
kgl_min_gpu = 5

# ALSO treat the foreground window as a game when it covers the whole
# monitor without a title bar (can false-positive on fullscreen video)
fullscreen_detect = false

# ALSO treat high 3D GPU load as gaming (can false-positive on browsers)
gpu_load_detect = false
# minimum 3D engine utilization (%) to count as gaming
gpu_threshold = 35
";

fn split_csv(v: &str) -> Vec<String> {
    v.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

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
    let d = Config::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim();
        match k.as_str() {
            "device" => {
                if !v.is_empty() {
                    cfg.device = Some(v.to_string())
                }
            }
            "poll_secs" => cfg.poll_secs = v.parse().unwrap_or(d.poll_secs),
            "grace_secs" => cfg.grace_secs = v.parse().unwrap_or(d.grace_secs),
            "modes" | "manual_modes" => cfg.modes = parse_mode_specs(v),
            "game_mode" => cfg.auto_game = parse_mode_spec(v).unwrap_or(cfg.auto_game),
            "idle_mode" => cfg.auto_idle = parse_mode_spec(v).unwrap_or(cfg.auto_idle),
            "auto_game_hz" | "game_hz" => cfg.auto_game.hz = v.parse().unwrap_or(d.auto_game.hz),
            "auto_idle_hz" | "idle_hz" => cfg.auto_idle.hz = v.parse().unwrap_or(d.auto_idle.hz),
            "game_hdr" => cfg.auto_game_hdr = parse_hdr_opt(v),
            "idle_hdr" => cfg.auto_idle_hdr = parse_hdr_opt(v),
            "enabled_on_start" | "auto_on_start" => {
                cfg.auto_enabled_on_start = v.parse().unwrap_or(d.auto_enabled_on_start)
            }
            "fullscreen_detect" => cfg.fullscreen_detect = v.parse().unwrap_or(d.fullscreen_detect),
            "gpu_load_detect" => cfg.gpu_load_detect = v.parse().unwrap_or(d.gpu_load_detect),
            "gpu_threshold" => cfg.gpu_threshold = v.parse().unwrap_or(d.gpu_threshold),
            "kgl_min_gpu" => cfg.kgl_min_gpu = v.parse().unwrap_or(d.kgl_min_gpu),
            "games" => cfg.games = split_csv(v),
            "games_ignore" => cfg.games_ignore = split_csv(v),
            _ => {}
        }
    }
    cfg
}
