# game_mode_switcher

A Windows system-tray applet + CLI that switches your display between
**game mode** (high refresh, HDR on) and **desktop mode** (lower refresh,
HDR as you like it), automatically, based on what you're running.

It grew out of a workaround for an NVIDIA driver bug that only manifests
while DSC is active: switching to a display mode whose uncompressed pixel
rate fits the link bandwidth forces the GPU to retrain the link without
DSC, and switching back re-engages it. That mechanism — plus Windows'
undocumented advanced-color API for HDR — is what this tool automates.

Tested on: RTX 4090 + Acer Predator X32 X (4K 240 Hz, DP 2.1 UHBR13) on
Windows 11 (build 26200). Should work on any Windows 10/11 setup.

## What it does

- **Tray applet** — a text icon showing the live HDR state:
  - **single left-click** toggles HDR ↔ SDR (catches external changes too,
    e.g. Win+Alt+B or in-game toggles)
  - **right-click** opens a menu: your configured modes, **Auto**, Exit
  - **Auto** applies `game_mode` while a game is detected and `idle_mode`
    after a grace period; optionally flips HDR at the same time
- **Game detection** — matches running processes against Windows' own
  *Known Games List* (Game Bar / GameConfigStore), gated on that process's
  own 3D GPU utilization so idle launchers don't trigger it
- **CLI** — everything is scriptable: mode switching, HDR on/off/toggle,
  EDID inspection/backup, DSC bandwidth verdicts per link class
- **HDR control that works** — uses the undocumented DisplayConfig
  advanced-color SET (type 16), with legacy DisplayConfig and NVAPI as
  fallbacks, because the documented `SET_ADVANCED_COLOR_STATE` silently
  no-ops on recent Windows 11 builds

## Build

```
cargo build --release
```

Produces `target\release\game_mode_switcher.exe`. Windows only; no admin
rights needed (except `edid no-dsc`, which writes to HKLM).

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
| `status` | Current mode + whether it likely requires DSC (default) |
| `list` | Enumerate outputs and all supported modes |
| `off` | Highest-refresh mode that does **not** need DSC |
| `on` | Highest-refresh mode (DSC resumes if needed) |
| `test [--secs N]` | Apply DSC-off mode for N seconds, then restore |
| `watch [--dry-run]` | Headless auto-switch loop (same config as the applet) |
| `detect` | Live view of the game-detection signals (for tuning the ini) |
| `config` | Create/print the config file location |
| `applet` | Launch the tray applet (detached) |
| `hdr status\|on\|off\|toggle` | Show or set Windows HDR |
| `edid status\|backup\|no-dsc\|restore\|restart-driver` | EDID tools (diagnostics) |

Options: `--device NAME` (output, e.g. `DISPLAY1`), `--link TYPE`
(link bandwidth class, e.g. `dp-uhbr13`), `--bpp N`, `--secs N`.

`hdr` also has diagnostics subcommands (`probe`, `probe2`, `dump15`,
`set15 V SIZE`, `types`) used to reverse-engineer the undocumented API.

## Configuration

`game_mode_switcher.ini` next to the exe (auto-created on first run,
or via `game_mode_switcher config`):

```ini
# link bandwidth class for DSC verdicts:
#   dp-hbr2 dp-hbr3 dp-uhbr10 dp-uhbr13 dp-uhbr20 hdmi20 hdmi21-frl3..frl6
link = dp-uhbr13
# bits per pixel for bandwidth math (24 = 8bpc RGB 4:4:4)
bpp = 24
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
   Mode). The tool reads `MatchedExeFullPath` entries (cached 5 min).
2. A listed process only counts as a *running game* if **its own**
   3D engine GPU utilization (from `\GPU Engine(*)\Utilization Percentage`)
   is ≥ `kgl_min_gpu` percent — so background launchers don't trigger it.
3. Optional extra signals (off by default): borderless-fullscreen
   foreground window, or system-wide high 3D load.
4. While a game runs, `game_mode` is applied; `grace_secs` after the last
   one exits, `idle_mode` is restored.

Run `game_mode_switcher detect` to see all three signals live.

## HDR control (technical notes)

Windows 11 24H2+ ignores the documented `DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE`
call on some setups. This tool instead uses the undocumented
`DisplayConfigSetDeviceInfo` payload types:

- **GET** advanced-color info, type `15`, size 36 — state enum at offset 32
  (`1` = SDR, `2` = HDR), plus bits-per-pixel and luminance fields
- **SET** advanced-color state, type `16`, size 24 — payload u32 at
  offset 20 (`0` = SDR, `1` = HDR) — note the *different* encoding vs GET

Every SET is verified with a follow-up GET; legacy DisplayConfig and
NVAPI (`NvAPI_Disp_HdrColorControl`) are kept as fallbacks. See
`src/hdr.rs` and `src/nvapi.rs`.

## DSC notes

`status`/`list` compute whether a mode fits the configured link bandwidth
raw (`needs_dsc` heuristic, bpp × pixels × refresh × blanking vs link
rate). On NVIDIA + DisplayPort the DSC capability is decided by live
DPCD data — EDID overrides do **not** remove DSC-dependent modes; the only
reliable lever is mode switching itself (a mode that fits the link raw
trains without DSC). `edid no-dsc` remains as an experiment/diagnostic.

## Source layout

```
src/main.rs   CLI, mode enumeration/apply, DSC bandwidth math, watch loop
src/tray.rs   tray applet (icon rendering, menu, auto tick, HDR polling)
src/games.rs  config, Known Games List, GPU load, detection, mode picking
src/hdr.rs    DisplayConfig advanced-color GET/SET + diagnostics
src/nvapi.rs  NVAPI HDR fallback (dynamic nvapi64.dll loading)
src/edid.rs   EDID inspection/backup/override, driver restart
```

## Limitations

- Windows 10/11 only; undocumented APIs may change in future builds
- Assumes a single display setup for the auto logic (the `--device` /
  `device =` option selects which output is switched)
- Mode switching causes a brief blank on most monitors
- The DSC verdict is a bandwidth heuristic — Windows exposes no public
  API to query the sink's DSC state

## License

Not yet chosen — add a LICENSE before distributing.
