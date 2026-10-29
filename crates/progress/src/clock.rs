//! Time as the display prints it: durations (`0:42`, `1:05:09`) and the local wall-clock time of day (`22:41`).
//!
//! Input: seconds, or a moment `secs_from_now` ahead. Output: short strings.
//! Source: POSIX `localtime_r(3)`, glibc `struct tm` (Linux x86-64/aarch64 layout); std has no local time zone.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// `m:ss` under an hour, `h:mm:ss` above.
pub fn duration(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Local `HH:MM` of now + `secs_from_now`; `??:??` if the C library cannot convert it.
pub fn local_hhmm(secs_from_now: f64) -> String {
    let at = SystemTime::now() + Duration::from_secs_f64(secs_from_now.clamp(0.0, 1e9));
    let t = at
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let mut tm = Tm::default();
    // SAFETY: both pointers are valid for the call; localtime_r writes only into `tm`.
    let ok = !unsafe { localtime_r(&t, &mut tm) }.is_null();
    if ok {
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    } else {
        "??:??".into()
    }
}

/// glibc `struct tm`.
#[repr(C)]
struct Tm {
    tm_sec: i32,
    tm_min: i32,
    tm_hour: i32,
    tm_mday: i32,
    tm_mon: i32,
    tm_year: i32,
    tm_wday: i32,
    tm_yday: i32,
    tm_isdst: i32,
    tm_gmtoff: i64,
    tm_zone: *const u8,
}

impl Default for Tm {
    fn default() -> Self {
        Tm {
            tm_sec: 0,
            tm_min: 0,
            tm_hour: 0,
            tm_mday: 0,
            tm_mon: 0,
            tm_year: 0,
            tm_wday: 0,
            tm_yday: 0,
            tm_isdst: 0,
            tm_gmtoff: 0,
            tm_zone: std::ptr::null(),
        }
    }
}

unsafe extern "C" {
    fn localtime_r(t: *const i64, tm: *mut Tm) -> *mut Tm;
}
