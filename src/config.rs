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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HdrPref {
    /// no preference - leave HDR as-is
    Default,
    On,
    Off,
    /// idle_hdr only: restore the pre-game HDR state when the game exits
    Restore,
}

fn parse_hdr_opt(v: &str) -> HdrPref {
    match v.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "1" | "yes" => HdrPref::On,
        "off" | "false" | "0" | "no" => HdrPref::Off,
        "restore" => HdrPref::Restore,
        _ => HdrPref::Default,
    }
}

pub(crate) struct Config {
    pub(crate) device: Option<String>,
    pub(crate) poll_secs: u64,
    pub(crate) grace_secs: u64,
    pub(crate) modes: Vec<ModeSpec>,
    pub(crate) auto_game: ModeSpec,
    pub(crate) auto_idle: ModeSpec,
    pub(crate) auto_game_hdr: HdrPref,
    pub(crate) auto_idle_hdr: HdrPref,
    pub(crate) auto_enabled_on_start: bool,
    pub(crate) fullscreen_detect: bool,
    pub(crate) gpu_load_detect: bool,
    pub(crate) gpu_threshold: u32,
    pub(crate) games: Vec<String>,
    pub(crate) games_ignore: Vec<String>,
    pub(crate) kgl_min_gpu: u32,
}

impl Config {
    pub(crate) fn spec_for(&self, active: bool) -> ModeSpec {
        if active {
            self.auto_game
        } else {
            self.auto_idle
        }
    }

    pub(crate) fn hdr_for(&self, active: bool) -> HdrPref {
        if active {
            self.auto_game_hdr
        } else {
            self.auto_idle_hdr
        }
    }
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
            auto_game_hdr: HdrPref::Default,
            auto_idle_hdr: HdrPref::Default,
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
# force HDR when entering game/idle mode: on/off; 'restore' on idle_hdr
# puts the pre-game HDR state back when the game exits; empty = leave as-is
# (toggle HDR any time with a left-click on the tray icon)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_spec(w: u32, h: u32, hz: u32) -> ModeSpec {
        ModeSpec { w, h, hz }
    }

    fn assert_spec_eq(got: Option<ModeSpec>, w: u32, h: u32, hz: u32) {
        match got {
            Some(s) => {
                assert_eq!(s.w, w, "w");
                assert_eq!(s.h, h, "h");
                assert_eq!(s.hz, hz, "hz");
            }
            None => panic!("expected ModeSpec {}x{} @ {} Hz, got None", w, h, hz),
        }
    }

    fn assert_spec_same(g: &ModeSpec, e: &ModeSpec) {
        assert_eq!(g.w, e.w, "w");
        assert_eq!(g.h, e.h, "h");
        assert_eq!(g.hz, e.hz, "hz");
    }

    #[test]
    fn parse_mode_spec_basic() {
        assert_spec_eq(parse_mode_spec("240"), 0, 0, 240);
        assert_spec_eq(parse_mode_spec("3840x2160@240"), 3840, 2160, 240);
        assert_spec_eq(parse_mode_spec("3840x2160"), 3840, 2160, 0);
    }

    #[test]
    fn parse_mode_spec_trims_and_accepts_uppercase_x() {
        assert_spec_eq(parse_mode_spec("  240  "), 0, 0, 240);
        assert_spec_eq(parse_mode_spec("3840X2160@240 "), 3840, 2160, 240);
    }

    #[test]
    fn parse_mode_spec_rejects_garbage() {
        for bad in ["", "   ", "abc", "3840x", "x2160"] {
            assert!(parse_mode_spec(bad).is_none(), "expected None for {bad:?}");
        }
    }

    #[test]
    fn parse_mode_spec_bad_hz_after_at_keeps_zero_hz() {
        // A dangling '@' or an unparseable hz after '@' falls back to hz = 0
        // (keep current refresh) when a resolution is present.
        assert_spec_eq(parse_mode_spec("3840x2160@"), 3840, 2160, 0);
        assert_spec_eq(parse_mode_spec("3840x2160@abc"), 3840, 2160, 0);
    }

    #[test]
    fn parse_mode_spec_zero_hz_is_valid_sentinel() {
        assert_spec_eq(parse_mode_spec("0"), 0, 0, 0);
    }

    #[test]
    fn mode_spec_label_all_branches() {
        assert_eq!(mode_spec_label(&mk_spec(0, 0, 240)), "240 Hz");
        assert_eq!(mode_spec_label(&mk_spec(3840, 2160, 0)), "3840x2160");
        assert_eq!(
            mode_spec_label(&mk_spec(3840, 2160, 240)),
            "3840x2160 @ 240 Hz"
        );
    }

    #[test]
    fn parse_mode_specs_drops_empty_and_bad_entries() {
        assert!(parse_mode_specs("").is_empty());
        let v = parse_mode_specs("240,, 120 ,");
        assert_eq!(v.len(), 2);
        assert_spec_eq(Some(v[0]), 0, 0, 240);
        assert_spec_eq(Some(v[1]), 0, 0, 120);
        let v = parse_mode_specs("240, junk ,3840x2160@60");
        assert_eq!(v.len(), 2);
        assert_spec_eq(Some(v[0]), 0, 0, 240);
        assert_spec_eq(Some(v[1]), 3840, 2160, 60);
    }

    #[test]
    fn parse_hdr_opt_all_values() {
        for t in ["on", "true", "1", "yes", "ON", "True", "YeS"] {
            assert_eq!(parse_hdr_opt(t), HdrPref::On, "expected On for {t:?}");
        }
        for f in ["off", "false", "0", "no", "OFF", "No", "FALSE"] {
            assert_eq!(parse_hdr_opt(f), HdrPref::Off, "expected Off for {f:?}");
        }
    }

    #[test]
    fn parse_hdr_opt_restore_and_default_fallbacks() {
        for t in ["restore", "RESTORE", "Restore", "  restore  "] {
            assert_eq!(parse_hdr_opt(t), HdrPref::Restore, "expected Restore for {t:?}");
        }
        for d in ["", "   ", "maybe", "banana"] {
            assert_eq!(parse_hdr_opt(d), HdrPref::Default, "expected Default for {d:?}");
        }
    }

    #[test]
    fn split_csv_trims_and_drops_empty() {
        assert!(split_csv("").is_empty());
        assert_eq!(split_csv("a, b ,c"), vec!["a", "b", "c"]);
    }

    #[test]
    fn spec_for_and_hdr_for_select_by_active() {
        let c = Config {
            auto_game_hdr: HdrPref::On,
            auto_idle_hdr: HdrPref::Off,
            ..Default::default()
        };
        assert_spec_same(&c.spec_for(true), &c.auto_game);
        assert_spec_same(&c.spec_for(false), &c.auto_idle);
        assert!(c.spec_for(true).hz != c.spec_for(false).hz);
        assert_eq!(c.hdr_for(true), HdrPref::On);
        assert_eq!(c.hdr_for(false), HdrPref::Off);
    }

    fn tmp_path(name: &str) -> PathBuf {
        env::temp_dir().join(format!("gms_test_{name}.ini"))
    }

    #[test]
    fn load_template_round_trips_to_default() {
        let path = tmp_path("template");
        fs::write(&path, CONFIG_TEMPLATE).unwrap();
        let cfg = load_or_create_config(&path);
        let d = Config::default();
        assert_eq!(cfg.device, d.device);
        assert_eq!(cfg.poll_secs, d.poll_secs);
        assert_eq!(cfg.grace_secs, d.grace_secs);
        assert_eq!(cfg.modes.len(), d.modes.len());
        for (g, e) in cfg.modes.iter().zip(&d.modes) {
            assert_spec_same(g, e);
        }
        assert_spec_same(&cfg.auto_game, &d.auto_game);
        assert_spec_same(&cfg.auto_idle, &d.auto_idle);
        assert_eq!(cfg.auto_game_hdr, d.auto_game_hdr);
        assert_eq!(cfg.auto_idle_hdr, d.auto_idle_hdr);
        assert_eq!(cfg.auto_enabled_on_start, d.auto_enabled_on_start);
        assert_eq!(cfg.fullscreen_detect, d.fullscreen_detect);
        assert_eq!(cfg.gpu_load_detect, d.gpu_load_detect);
        assert_eq!(cfg.gpu_threshold, d.gpu_threshold);
        assert_eq!(cfg.games, d.games);
        assert_eq!(cfg.games_ignore, d.games_ignore);
        assert_eq!(cfg.kgl_min_gpu, d.kgl_min_gpu);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn load_malformed_values_fall_back_to_defaults() {
        let path = tmp_path("malformed");
        fs::write(
            &path,
            "fullscreen_detect = banana\ngpu_load_detect = banana\npoll_secs = x\ngrace_secs = nope\ngpu_threshold = zz\nkgl_min_gpu = 12x\n",
        )
        .unwrap();
        let cfg = load_or_create_config(&path);
        let d = Config::default();
        assert!(!cfg.fullscreen_detect);
        assert!(!cfg.gpu_load_detect);
        assert_eq!(cfg.poll_secs, d.poll_secs);
        assert_eq!(cfg.grace_secs, d.grace_secs);
        assert_eq!(cfg.gpu_threshold, d.gpu_threshold);
        assert_eq!(cfg.kgl_min_gpu, d.kgl_min_gpu);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn load_valid_overrides_are_honored() {
        let path = tmp_path("overrides");
        fs::write(
            &path,
            "poll_secs = 99\nmodes = 240, 3840x2160@120\ngame_hdr = on\nidle_hdr = off\n",
        )
        .unwrap();
        let cfg = load_or_create_config(&path);
        assert_eq!(cfg.poll_secs, 99);
        assert_eq!(cfg.modes.len(), 2);
        assert_spec_same(&cfg.modes[0], &mk_spec(0, 0, 240));
        assert_spec_same(&cfg.modes[1], &mk_spec(3840, 2160, 120));
        assert_eq!(cfg.auto_game_hdr, HdrPref::On);
        assert_eq!(cfg.auto_idle_hdr, HdrPref::Off);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn load_idle_hdr_restore() {
        let path = tmp_path("restore");
        fs::write(&path, "idle_hdr = restore\n").unwrap();
        let cfg = load_or_create_config(&path);
        let d = Config::default();
        assert_eq!(cfg.auto_idle_hdr, HdrPref::Restore);
        assert_eq!(cfg.auto_game_hdr, d.auto_game_hdr);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn load_unknown_keys_are_ignored() {
        let path = tmp_path("unknown");
        fs::write(&path, "bogus_key = 1\nbpp = garbage\n[weird]\n").unwrap();
        let cfg = load_or_create_config(&path);
        let d = Config::default();
        assert_eq!(cfg.poll_secs, d.poll_secs);
        assert_eq!(cfg.grace_secs, d.grace_secs);
        assert_eq!(cfg.modes.len(), d.modes.len());
        assert_spec_same(&cfg.auto_game, &d.auto_game);
        assert_spec_same(&cfg.auto_idle, &d.auto_idle);
        assert_eq!(cfg.auto_enabled_on_start, d.auto_enabled_on_start);
        assert_eq!(cfg.gpu_threshold, d.gpu_threshold);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn load_legacy_aliases() {
        let path = tmp_path("legacy");
        fs::write(&path, "game_hz = 200\nidle_hz = 100\nauto_on_start = false\n").unwrap();
        let cfg = load_or_create_config(&path);
        assert_eq!(cfg.auto_game.hz, 200);
        assert_eq!(cfg.auto_idle.hz, 100);
        assert!(!cfg.auto_enabled_on_start);
        let _ = fs::remove_file(&path);
    }
}
