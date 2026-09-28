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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_to_string_stops_at_nul() {
        let v: Vec<u16> = "hi\0".encode_utf16().collect();
        assert_eq!(wide_to_string(&v), "hi");
    }

    #[test]
    fn wide_to_string_empty() {
        assert_eq!(wide_to_string(&[]), "");
    }

    #[test]
    fn wide_to_string_no_nul_reads_full_slice() {
        let v: Vec<u16> = "hello".encode_utf16().collect();
        assert_eq!(wide_to_string(&v), "hello");
    }

    #[test]
    fn wide_to_string_lone_surrogate_is_lossy() {
        let v = vec![0xD800u16, 0];
        assert!(wide_to_string(&v).contains('\u{FFFD}'));
    }

    #[test]
    fn to_widez_round_trip() {
        for s in ["", "abc", "héllo wörld", "日本語"] {
            assert_eq!(wide_to_string(&to_widez(s)), s);
        }
    }

    #[test]
    fn wide_cstr_terminated_and_unterminated() {
        let v = to_widez("abc");
        assert_eq!(wide_cstr(v.as_ptr()), "abc");
        let v: Vec<u16> = "xyz".encode_utf16().collect();
        assert_eq!(wide_cstr(v.as_ptr()), "xyz");
    }
}
