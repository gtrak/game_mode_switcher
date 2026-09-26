use windows::core::{PCSTR, PCWSTR};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

const ID_INITIALIZE: u32 = 0x0150_e828;
const ID_GET_DISPLAY_ID_BY_NAME: u32 = 0xae45_7190;
const ID_HDR_COLOR_CONTROL: u32 = 0x351d_a224;
const ID_GET_HDR_CAPABILITIES: u32 = 0x84f2_a8df;

const HDR_CMD_GET: u32 = 0;
const HDR_CMD_SET: u32 = 1;
const HDR_MODE_OFF: u32 = 0;
const HDR_MODE_UHDA: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct MasteringData {
    pub v: [u16; 12],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct HdrColorDataV2 {
    version: u32,
    cmd: u32,
    hdr_mode: u32,
    static_metadata_descriptor_id: u32,
    mastering_display_data: MasteringData,
    hdr_color_format: u32,
    hdr_dynamic_range: u32,
    hdr_bpc: u32,
}

impl HdrColorDataV2 {
    fn new() -> Self {
        HdrColorDataV2 {
            version: (std::mem::size_of::<HdrColorDataV2>() as u32) | (2u32 << 16),
            cmd: HDR_CMD_GET,
            hdr_mode: HDR_MODE_OFF,
            static_metadata_descriptor_id: 0,
            mastering_display_data: MasteringData::default(),
            hdr_color_format: 0,
            hdr_dynamic_range: 0,
            hdr_bpc: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct HdrCapabilitiesV1 {
    version: u32,
    flags: u32,
    static_metadata_descriptor_id: u32,
    display_data: MasteringData,
}

impl HdrCapabilitiesV1 {
    fn new() -> Self {
        HdrCapabilitiesV1 {
            version: (std::mem::size_of::<HdrCapabilitiesV1>() as u32) | (1u32 << 16),
            flags: 0,
            static_metadata_descriptor_id: 0,
            display_data: MasteringData::default(),
        }
    }
}

type QueryInterfaceFn = unsafe extern "system" fn(id: u32) -> *mut core::ffi::c_void;
type InitializeFn = unsafe extern "system" fn() -> i32;
type GetDisplayIdFn = unsafe extern "system" fn(name: *const u8, display_id: *mut u32) -> i32;
type HdrColorControlFn =
    unsafe extern "system" fn(display_id: u32, data: *mut HdrColorDataV2) -> i32;
type HdrCapabilitiesFn =
    unsafe extern "system" fn(display_id: u32, caps: *mut HdrCapabilitiesV1) -> i32;

struct NvapiCtx {
    display_id: u32,
    hdr_control: HdrColorControlFn,
    hdr_caps: HdrCapabilitiesFn,
}

fn nvapi_ctx(display_name: &str) -> Result<NvapiCtx, String> {
    unsafe {
        let name_w: Vec<u16> = "nvapi64.dll\0".encode_utf16().collect();
        let lib = LoadLibraryW(PCWSTR::from_raw(name_w.as_ptr()))
            .map_err(|e| format!("nvapi64.dll not loadable: {}", e))?;
        let sym = b"nvapi_QueryInterface\0";
        let qif_addr = GetProcAddress(lib, PCSTR::from_raw(sym.as_ptr()));
        let Some(qif_addr) = qif_addr else {
            return Err(String::from("nvapi_QueryInterface not found in nvapi64.dll"));
        };
        let qif: QueryInterfaceFn = std::mem::transmute(qif_addr);

        let init: InitializeFn =
            query(qif, ID_INITIALIZE).ok_or_else(|| String::from("NvAPI_Initialize missing"))?;
        let st = init();
        if st != 0 {
            return Err(format!("NvAPI_Initialize failed ({})", st));
        }

        let get_display_id: GetDisplayIdFn = query(qif, ID_GET_DISPLAY_ID_BY_NAME)
            .ok_or_else(|| String::from("NvAPI_DISP_GetDisplayIdByDisplayName missing"))?;
        let mut name_b = display_name.as_bytes().to_vec();
        name_b.push(0);
        let mut display_id = 0u32;
        let st = get_display_id(name_b.as_ptr(), &mut display_id);
        if st != 0 || display_id == 0 {
            return Err(format!(
                "NvAPI_DISP_GetDisplayIdByDisplayName('{}') failed ({})",
                display_name, st
            ));
        }

        let hdr_control: HdrColorControlFn = query(qif, ID_HDR_COLOR_CONTROL)
            .ok_or_else(|| String::from("NvAPI_Disp_HdrColorControl missing"))?;
        let hdr_caps: HdrCapabilitiesFn = query(qif, ID_GET_HDR_CAPABILITIES)
            .ok_or_else(|| String::from("NvAPI_Disp_GetHdrCapabilities missing"))?;

        Ok(NvapiCtx {
            display_id,
            hdr_control,
            hdr_caps,
        })
    }
}

unsafe fn query<T>(qif: QueryInterfaceFn, id: u32) -> Option<T> {
    let p = qif(id);
    if p.is_null() {
        None
    } else {
        Some(std::mem::transmute_copy::<*mut core::ffi::c_void, T>(&p))
    }
}

fn default_mastering() -> MasteringData {
    MasteringData {
        v: [
            0x8A48, 0x3908, 0x2134, 0x9BAA, 0x1996, 0x08FC, 0x3D13, 0x4042, 0x2710, 0x00FA,
            0x03E8, 0x0190,
        ],
    }
}

pub(crate) struct NvapiHdrCaps {
    pub st2084_supported: bool,
    pub traditional_hdr_supported: bool,
    pub edr_supported: bool,
    pub driver_expand: bool,
    pub metadata: [u16; 12],
}

pub(crate) fn nvapi_hdr_capabilities(display_name: &str) -> Result<NvapiHdrCaps, String> {
    let ctx = nvapi_ctx(display_name)?;
    let mut caps = HdrCapabilitiesV1::new();
    let st = unsafe { (ctx.hdr_caps)(ctx.display_id, &mut caps) };
    if st != 0 {
        return Err(format!("NvAPI_Disp_GetHdrCapabilities failed ({})", st));
    }
    Ok(NvapiHdrCaps {
        st2084_supported: caps.flags & 1 != 0,
        traditional_hdr_supported: caps.flags & (1 << 1) != 0,
        edr_supported: caps.flags & (1 << 2) != 0,
        driver_expand: caps.flags & (1 << 3) != 0,
        metadata: caps.display_data.v,
    })
}

pub(crate) fn nvapi_hdr_mode(display_name: &str) -> Result<u32, String> {
    let ctx = nvapi_ctx(display_name)?;
    let mut data = HdrColorDataV2::new();
    let st = unsafe { (ctx.hdr_control)(ctx.display_id, &mut data) };
    if st != 0 {
        return Err(format!("NvAPI_Disp_HdrColorControl(GET) failed ({})", st));
    }
    Ok(data.hdr_mode)
}

pub(crate) fn nvapi_hdr_set(display_name: &str, on: bool) -> Result<(), String> {
    let ctx = nvapi_ctx(display_name)?;
    unsafe {
        let mut data = HdrColorDataV2::new();
        let st = (ctx.hdr_control)(ctx.display_id, &mut data);
        if st != 0 {
            return Err(format!("NvAPI_Disp_HdrColorControl(GET) failed ({})", st));
        }
        let want = if on { HDR_MODE_UHDA } else { HDR_MODE_OFF };
        if data.hdr_mode == want {
            return Ok(());
        }
        data.cmd = HDR_CMD_SET;
        data.hdr_mode = want;
        if on {
            let mut caps = HdrCapabilitiesV1::new();
            let st = (ctx.hdr_caps)(ctx.display_id, &mut caps);
            let caps_meta = if st == 0 && caps.display_data.v[..11].iter().any(|&x| x != 0) {
                let mut m = caps.display_data;
                m.v[10] = caps.display_data.v[8];
                m.v[11] = caps.display_data.v[10];
                m
            } else {
                default_mastering()
            };
            data.mastering_display_data = caps_meta;
        }
        let st = (ctx.hdr_control)(ctx.display_id, &mut data);
        if st != 0 {
            return Err(format!(
                "NvAPI_Disp_HdrColorControl(SET {}) failed ({})",
                if on { "on" } else { "off" },
                st
            ));
        }
    }
    Ok(())
}
