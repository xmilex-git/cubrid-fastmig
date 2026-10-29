//! The migrate start time as the engine writes `created_time` / `updated_time`: this process's local wall-clock time
//! (`TZ`, else `/etc/localtime`), milliseconds kept.
//!
//! Input: the system clock and the process time zone. Output: a DATETIME [`Value`].
//! Source: develop `db_sys_datetime` (string_opfunc.c@682b080b2: `localtime_r` of the epoch second, then
//! `db_datetime_encode` with the milliseconds), which `db_set_otmpl_timestamps` and `au_set_new_timestamps` write;
//! workspace#482 decision 8; workspace#543 (the UTC time was written before). glibc x86_64 `struct tm`: nine ints,
//! then `long tm_gmtoff` and `const char *tm_zone`.

use std::ffi::{c_char, c_long};
use std::time::{SystemTime, UNIX_EPOCH};

use fastmig_ondisk::value::Value;

#[repr(C)]
struct Tm {
    fields: [i32; 9],
    tm_gmtoff: c_long,
    tm_zone: *const c_char,
}

unsafe extern "C" {
    fn tzset();
    fn localtime_r(t: *const i64, tm: *mut Tm) -> *mut Tm;
}

/// Seconds east of UTC that `localtime_r` applies at `sec`; 0 when it fails.
fn utc_offset(sec: i64) -> i64 {
    let mut tm = std::mem::MaybeUninit::<Tm>::uninit();
    // SAFETY: tzset only reads the environment; localtime_r reads `sec` and fills the whole `tm` when it returns non-null.
    let filled = unsafe {
        tzset();
        !localtime_r(&sec, tm.as_mut_ptr()).is_null()
    };
    if !filled {
        return 0;
    }
    // SAFETY: localtime_r returned non-null, so it wrote the struct.
    unsafe { tm.assume_init() }.tm_gmtoff
}

pub fn now_datetime() -> Value {
    let utc_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let ms = utc_ms + utc_offset(utc_ms.div_euclid(1000)) * 1000;
    // Julian day of 1970-01-01 is 2440588
    Value::Datetime {
        date: (2_440_588 + ms.div_euclid(86_400_000)) as u32,
        time: ms.rem_euclid(86_400_000) as u32,
    }
}
