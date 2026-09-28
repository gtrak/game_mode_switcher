use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, DisplayConfigSetDeviceInfo, GetDisplayConfigBufferSizes,
    QueryDisplayConfig, DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE,
    DISPLAYCONFIG_DEVICE_INFO_TYPE, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_0, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE, DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE_0,
    QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Foundation::ERROR_SUCCESS;

const ADV_COLOR_SUPPORTED_BIT: u32 = 1 << 0;
const ADV_COLOR_ENABLED_BIT: u32 = 1 << 1;

fn primary_target() -> Result<(windows::Win32::Foundation::LUID, u32), String> {
    unsafe {
        let mut npath = 0u32;
        let mut nmode = 0u32;
        let err = GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut npath, &mut nmode);
        if err != ERROR_SUCCESS {
            return Err(format!("GetDisplayConfigBufferSizes failed ({})", err.0));
        }
        let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> = Vec::with_capacity(npath as usize);
        let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> = Vec::with_capacity(nmode as usize);
        paths.resize_with(npath as usize, Default::default);
        modes.resize_with(nmode as usize, Default::default);
        let err = QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut npath,
            paths.as_mut_ptr(),
            &mut nmode,
            modes.as_mut_ptr(),
            None,
        );
        if err != ERROR_SUCCESS {
            return Err(format!("QueryDisplayConfig failed ({})", err.0));
        }
        paths.truncate(npath as usize);
        let p = paths.first().ok_or_else(|| "no active display paths".to_string())?;
        Ok((p.targetInfo.adapterId, p.targetInfo.id))
    }
}

pub(crate) struct HdrInfo {
    pub enabled: bool,
    pub wide_color: bool,
    pub force_disabled: bool,
    pub bits_per_channel: u32,
    pub color_encoding_rgb: bool,
}

pub(crate) fn hdr_state() -> Result<Option<HdrInfo>, String> {
    let (adapter, id) = primary_target()?;
    unsafe {
        let mut get = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                size: std::mem::size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32,
                adapterId: adapter,
                id,
            },
            Anonymous: DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_0 { value: 0 },
            bitsPerColorChannel: 0,
            colorEncoding: Default::default(),
        };
        let err = DisplayConfigGetDeviceInfo(&mut get.header);
        if err != 0 {
            return Err(format!("DisplayConfigGetDeviceInfo failed ({})", err));
        }
        let bits = get.Anonymous.value;
        if bits & ADV_COLOR_SUPPORTED_BIT == 0 {
            return Ok(None);
        }
        Ok(Some(HdrInfo {
            enabled: bits & ADV_COLOR_ENABLED_BIT != 0,
            wide_color: bits & (1 << 2) != 0,
            force_disabled: bits & (1 << 3) != 0,
            bits_per_channel: get.bitsPerColorChannel,
            color_encoding_rgb: get.colorEncoding.0 == 1,
        }))
    }
}

pub(crate) fn hdr_set(on: bool) -> Result<(), String> {
    let value = if on { 1u32 } else { 0u32 };
    set_type15(value, 24)
}

fn wait_for(tries: u32, delay_ms: u64, mut probe: impl FnMut() -> bool) -> bool {
    for _ in 0..tries {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        if probe() {
            return true;
        }
    }
    false
}

fn wait_type15_state(state: u32, tries: u32, delay_ms: u64) -> bool {
    wait_for(tries, delay_ms, || type15_state() == Ok(state))
}

pub(crate) enum HdrMethod {
    DisplayConfig,
    Legacy,
    Nvapi,
}

pub(crate) fn hdr_set_verified(on: bool) -> Result<HdrMethod, String> {
    if hdr_enabled() == Some(on) {
        return Ok(HdrMethod::DisplayConfig);
    }
    let want_state = if on { 2u32 } else { 1u32 };
    if hdr_set(on).is_ok() && wait_type15_state(want_state, 12, 150) {
        return Ok(HdrMethod::DisplayConfig);
    }
    legacy_set(on)?;
    if wait_legacy(on, 10, 150) {
        return Ok(HdrMethod::Legacy);
    }
    if crate::nvapi::nvapi_hdr_set(crate::PRIMARY_DISPLAY, on).is_ok() {
        let want = if on { 2u32 } else { 0u32 };
        if wait_for(10, 100, || crate::nvapi::nvapi_hdr_mode(crate::PRIMARY_DISPLAY) == Ok(want)
            && type15_state() == Ok(want_state)
        ) {
            return Ok(HdrMethod::Nvapi);
        }
    }
    Err(format!(
        "HDR did not reach {} via type16 SET, legacy DisplayConfig, or NVAPI",
        if on { "on" } else { "off" }
    ))
}

fn wait_legacy(on: bool, tries: u32, delay_ms: u64) -> bool {
    wait_for(
        tries,
        delay_ms,
        || hdr_state().ok().flatten().map(|i| i.enabled) == Some(on),
    )
}

fn legacy_set(on: bool) -> Result<(), String> {
    let (adapter, id) = primary_target()?;
    unsafe {
        let mut set = DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE,
                size: std::mem::size_of::<DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE>() as u32,
                adapterId: adapter,
                id,
            },
            Anonymous: DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE_0 {
                Anonymous: Default::default(),
            },
        };
        set.Anonymous.value = if on { 1 } else { 0 };
        let err = DisplayConfigSetDeviceInfo(&set.header);
        if err != 0 {
            return Err(format!("legacy DisplayConfigSetDeviceInfo failed ({})", err));
        }
    }
    Ok(())
}

pub(crate) fn hdr_enabled() -> Option<bool> {
    match type15_state() {
        Ok(2) => Some(true),
        Ok(1) => Some(false),
        _ => hdr_state().ok().flatten().map(|i| i.enabled),
    }
}

pub(crate) fn dump_type15() -> Result<Vec<u8>, String> {
    let target = primary_target()?;
    unsafe {
        let mut buf = [0u8; 36];
        let header = buf.as_mut_ptr() as *mut DISPLAYCONFIG_DEVICE_INFO_HEADER;
        (*header).r#type = DISPLAYCONFIG_DEVICE_INFO_TYPE(15);
        (*header).size = 36;
        (*header).adapterId = target.0;
        (*header).id = target.1;
        let err = DisplayConfigGetDeviceInfo(header);
        if err != 0 {
            return Err(format!("type15 GET failed ({})", err));
        }
        Ok(buf.to_vec())
    }
}

pub(crate) fn type15_state() -> Result<u32, String> {
    let b = dump_type15()?;
    Ok(u32::from_le_bytes([b[32], b[33], b[34], b[35]]))
}

pub(crate) fn set_type15(value: u32, size: u32) -> Result<(), String> {
    let target = primary_target()?;
    unsafe {
        let mut buf = [0u8; 36];
        buf[0..4].copy_from_slice(&16u32.to_le_bytes());
        buf[4..8].copy_from_slice(&size.to_le_bytes());
        let mut luid_le = [0u8; 8];
        luid_le[0..4].copy_from_slice(&target.0.LowPart.to_le_bytes());
        luid_le[4..8].copy_from_slice(&target.0.HighPart.to_le_bytes());
        buf[8..16].copy_from_slice(&luid_le);
        buf[16..20].copy_from_slice(&target.1.to_le_bytes());
        if size >= 24 {
            buf[20..24].copy_from_slice(&value.to_le_bytes());
        }
        if size >= 36 {
            buf[32..36].copy_from_slice(&value.to_le_bytes());
        }
        let header = buf.as_mut_ptr() as *const DISPLAYCONFIG_DEVICE_INFO_HEADER;
        let err = DisplayConfigSetDeviceInfo(header);
        if err != 0 {
            return Err(format!("type16 SET failed ({})", err));
        }
    }
    Ok(())
}