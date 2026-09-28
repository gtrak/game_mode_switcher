# game_mode_switcher

A Windows system-tray applet + CLI that switches your display mode and HDR
between **game mode** (high refresh) and **desktop mode** (lower refresh),
automatically, based on which games are running.

It grew out of a workaround for an NVIDIA driver bug that only manifests
while DSC is active: switching to a display mode whose uncompressed pixel
rate fits the link forces the GPU to retrain the link without DSC. That
mechanism — plus Windows' undocumented advanced-color API for HDR — is what
this tool automates. Tested on an RTX 4090 + Acer X32 X (4K240, DP 2.1) on
Windows 11.

## What it does

- **Tray applet** — a text icon showing the live HDR/SDR state:
  - **left-click** toggles HDR ↔ SDR (catches external changes too, e.g.
    in-game toggles)
  - **right-click** opens a menu: your configured modes, **Auto**, Exit
  - **Auto** applies `game_mode` while a game is detected and `idle_mode`
    after a grace period; optionally flips HDR (`game_hdr` / `idle_hdr`)
- **Game detection** — matches running processes against Windows' own
  *Known Games List* (Game Bar / GameConfigStore), gated on that process's
  own 3D GPU utilization so idle launchers don't trigger it
- **Scriptable CLI** — mode switching, auto-switch loop, live detection,
  and HDR on/off/toggle, all headless
- **HDR control that works** — uses the undocumented DisplayConfig
  advanced-color type-16 SET (verified against the type-15 GET, with legacy
  DisplayConfig and NVAPI as fallbacks), because the documented
  `SET_ADVANCED_COLOR_STATE` silently no-ops on recent Windows 11 builds

## Build

```
cargo build --release
```

Produces `target\release\game_mode_switcher.exe`. Windows only; no admin
rights needed. `cargo test` runs 33 unit tests.

## Tray applet

```
game_mode_switcher applet
```

| Action | Result |
|---|---|
| Left-click icon | Toggle HDR ↔ SDR |
| Right-click → mode entry | Switch to that mode, Auto is unchecked (manual) |
| Right-click → Auto | Re-enable auto detection and sync immediately |
| Right-click → Auto (again) | Turn Auto off entirely |
| Right-click → Exit | Quit the applet |

The icon reads **HDR** (amber) or **SDR** (gray) and tracks the real
Windows HDR state every poll cycle — even if HDR was changed elsewhere.

The applet logs to `game_mode_switcher_tray.log` next to the exe.

## CLI

```
game_mode_switcher <command> [options]
```

| Command | Purpose |
|---|---|
| `status` | Show current display mode (default) |
| `list` | Enumerate outputs and all supported modes |
| `watch [--dry-run] [--config FILE]` | Auto-switch by game detection: while a game runs apply `game_mode` (and `game_hdr` if set); `grace_secs` after the last game exits, fall back to `idle_mode` / `idle_hdr` |
| `detect` | Show the live game-detection signals: Known Games List processes (with per-process 3D GPU load), configured games, and fullscreen state — useful for tuning the ini |
| `config` | Create/print the config file location |
| `applet` | Launch the system tray applet (detached; left-click toggles HDR, right-click menu: modes / Auto / Exit) |
| `hdr [status\|on\|off\|toggle]` | Show or set Windows HDR on the primary display |

Options: `--config FILE` (config file for `watch`/`config`, default
`game_mode_switcher.ini` next to the exe), `--dry-run` (`watch`: log actions
without applying them), `--bg` (`applet`: run in-process, used by the
detached launcher). `watch`/`config` read `game_mode_switcher.ini` next to
the exe (auto-created with defaults on first run).

## Configuration

`game_mode_switcher.ini` next to the exe (auto-created on first run, or via
`game_mode_switcher config`):

```ini
# game_mode_switcher configuration (used by `watch` and the tray applet)
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
```

Example: play games at 4K/240/HDR, drop to 4K/120/SDR when done, plus a
low-latency esports entry in the menu:

```ini
modes = 3840x2160@240, 3840x2160@120, 2560x1440@360
game_mode = 3840x2160@240
idle_mode = 3840x2160@120
game_hdr = on
idle_hdr = off
```

## How game detection works

1. Windows maintains a *Known Games List* under
   `HKCU\System\GameConfigStore\Children` (populated by Game Bar / Game
   Mode). The tool reads its `MatchedExeFullPath` entries (cached 5 min).
2. A listed process only counts as a *running game* if **its own** 3D
   engine GPU utilization (from `\GPU Engine(*)\Utilization Percentage`) is
   ≥ `kgl_min_gpu` percent — so background launchers that sit near 0% don't
   trigger it.
3. Optional extra signals (off by default): a borderless-fullscreen
   foreground window, or system-wide high 3D load.
4. While a game runs, `game_mode` is applied; `grace_secs` after the last
   one exits, `idle_mode` is restored.

Run `game_mode_switcher detect` to see all three signals live and tune the
ini.

## HDR control (technical notes)

Recent Windows 11 builds no-op the documented
`DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE` call, so this tool instead uses
undocumented `DisplayConfigSetDeviceInfo` payload types: a type-15 GET
(size 36) whose state enum at offset 32 reads `1` = SDR / `2` = HDR, and a
type-16 SET (size 24) whose payload u32 at offset 20 writes `0` = SDR /
`1` = HDR — note the *different* encoding between GET and SET. Every SET is
verified with a follow-up GET; legacy DisplayConfig and NVAPI
(`NvAPI_Disp_HdrColorControl`) are kept as fallbacks. See `src/hdr.rs` and
`src/nvapi.rs`.

## Source layout

```
src/main.rs     CLI dispatch and command implementations
src/display.rs  mode enumeration, mode apply, and mode picking
src/config.rs   Config / ModeSpec types and the ini template + parser
src/games.rs    process enumeration, GPU load, Known Games List, detection
src/auto.rs     AutoSwitcher state machine shared by `watch` and the tray
src/tray.rs     system-tray applet (icon, menu, auto tick, HDR polling)
src/hdr.rs      DisplayConfig advanced-color GET/SET
src/nvapi.rs    NVAPI HDR fallback (dynamic nvapi64.dll loading)
src/util.rs     string and time helpers
```

## Limitations

- Windows 10/11 only; the undocumented DisplayConfig APIs may change in
  future builds
- The auto logic assumes a single display (the `device =` option selects
  which output is switched)
- Mode switching causes a brief blank on most monitors
- HDR control targets the primary display

## License

MIT — see [LICENSE](LICENSE).
