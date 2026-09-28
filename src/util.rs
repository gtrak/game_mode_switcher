use std::time::{SystemTime, UNIX_EPOCH};
use windows::core::PCWSTR;

pub(crate) fn wide_to_string(ws: &[u16]) -> String {
    let end = ws.iter().position(|&c| c == 0).unwrap_or(ws.len());
    String::from_utf16_lossy(&ws[..end])
}

pub(crate) fn wide_cstr(p: *const u16) -> String {
    unsafe {
        let mut l = 0usize;
        while *p.add(l) != 0 {
            l += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, l))
    }
}

pub(crate) fn pcw(v: &[u16]) -> PCWSTR {
    PCWSTR::from_raw(v.as_ptr())
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
