use std::process::exit;
use windows::core::PCWSTR;
use windows::Win32::Graphics::Gdi::{
    ChangeDisplaySettingsExW, EnumDisplayDevicesW, EnumDisplaySettingsExW, CDS_TYPE,
    CDS_UPDATEREGISTRY, DEVMODEW, DISP_CHANGE_SUCCESSFUL, DISPLAY_DEVICEW,
    DISPLAY_DEVICE_ACTIVE, DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, DM_DISPLAYFREQUENCY,
    DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_FLAGS,
    ENUM_DISPLAY_SETTINGS_MODE,
};

use crate::config::ModeSpec;
use crate::util::{pcw, wide_to_string};

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

fn new_devmode() -> DEVMODEW {
    DEVMODEW {
        dmSize: std::mem::size_of::<DEVMODEW>() as u16,
        ..Default::default()
    }
}

fn new_display_device() -> DISPLAY_DEVICEW {
    DISPLAY_DEVICEW {
        cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
        ..Default::default()
    }
}

fn mode_from_devmode(dm: &DEVMODEW) -> Mode {
    Mode {
        w: dm.dmPelsWidth,
        h: dm.dmPelsHeight,
        freq: dm.dmDisplayFrequency,
    }
}

pub(crate) fn enumerate_outputs() -> Vec<Output> {
    let mut outs = Vec::new();
    for idx in 0..64u32 {
        let mut dd = new_display_device();
        unsafe {
            if !EnumDisplayDevicesW(PCWSTR::null(), idx, &mut dd, 0).as_bool() {
                break;
            }
        }
        if (dd.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP) == DISPLAY_DEVICE_ATTACHED_TO_DESKTOP
            && (dd.StateFlags & DISPLAY_DEVICE_ACTIVE) == DISPLAY_DEVICE_ACTIVE
        {
            let mut mon = new_display_device();
            let monitor = unsafe {
                if EnumDisplayDevicesW(pcw(&dd.DeviceName), 0, &mut mon, 0).as_bool() {
                    wide_to_string(&mon.DeviceString)
                } else {
                    String::from("<unknown monitor>")
                }
            };
            let mut current = None;
            {
                let mut dm = new_devmode();
                unsafe {
                    if EnumDisplaySettingsExW(
                        pcw(&dd.DeviceName),
                        ENUM_CURRENT_SETTINGS,
                        &mut dm,
                        ENUM_DISPLAY_SETTINGS_FLAGS(0),
                    )
                    .as_bool()
                    {
                        current = Some(mode_from_devmode(&dm));
                    }
                }
            }
            let mut modes = Vec::new();
            let mut i = 0u32;
            loop {
                let mut dm = new_devmode();
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
                    let m = mode_from_devmode(&dm);
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
    let mut dm = new_devmode();
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

pub(crate) fn pick_mode<'a>(
    outs: &'a [Output],
    device: &Option<String>,
    spec: &ModeSpec,
) -> Option<(&'a Output, Mode)> {
    let o = find_output(outs, device);
    let cur = o.current?;
    let (tw, th) = if spec.w != 0 {
        (spec.w, spec.h)
    } else {
        (cur.w, cur.h)
    };
    let target_hz = if spec.hz != 0 { spec.hz } else { cur.freq };
    let mut best_le: Option<Mode> = None;
    let mut best_gt: Option<Mode> = None;
    for m in &o.modes {
        if m.w != tw || m.h != th {
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

pub(crate) fn print_output_header(o: &Output) {
    println!(
        "{}  [{} / {}]",
        wide_to_string(&o.device_name),
        o.adapter,
        o.monitor
    );
}

pub(crate) fn print_mode_line(m: Mode, current: &Option<Mode>) {
    let cur = if Some(m) == *current { " [current]" } else { "" };
    println!("    {}x{} @ {:>3} Hz{}", m.w, m.h, m.freq, cur);
}
