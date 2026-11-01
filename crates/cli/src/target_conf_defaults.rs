//! The defaults migrate and plan take from the target server's `cubrid.conf` (the command line always wins).
//!
//! Input: the target conf (default `$CUBRID/conf/cubrid.conf`) and the target DB name. Output: the values the target
//! server would run with for `db_volume_size` (→ `--db-volume-size`), `data_buffer_size` (→ `--temp-mem-size`: the
//! engine's data buffer holds the temp file pages too) and `temp_file_max_size_in_pages` (→ `--temp-disk-size`),
//! each none when the conf does not set it.
//! Sizes are read like the engine: a number, optionally with a fraction, and a unit B, K, M, G, T or P (with or
//! without a trailing B, any case), bytes when there is none.
//! Source: engine `util_common.c:util_size_string_to_byte`, `util_size_to_byte`; `system_parameter.c`
//! (`PRM_NAME_DB_VOLUME_SIZE`, `PRM_NAME_PAGE_BUFFER_SIZE`, `PRM_NAME_BOSR_MAXTMP_PAGES`), develop 5b1867174; decision
//! G13 workspace#604.

use std::io;
use std::path::Path;

use fastmig_plan::cubrid_conf::parameter;

use crate::args::CliError;
use crate::limits::TempDiskChoice;

#[derive(Clone, Copy, Debug, Default)]
pub struct TargetConfDefaults {
    pub db_volume_size: Option<u64>,
    pub data_buffer_size: Option<u64>,
    pub temp_file_max_size_in_pages: Option<i64>,
}

/// An engine size string in bytes.
pub fn engine_size(v: &str) -> Option<u64> {
    let v = v.trim();
    let split = v
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(v.len());
    let (num, unit) = v.split_at(split);
    let num: f64 = num.parse().ok().filter(|n: &f64| *n >= 0.0)?;
    let unit = unit.to_ascii_lowercase();
    let shift = match unit.strip_suffix('b').unwrap_or(&unit) {
        "" => 0,
        "k" => 10,
        "m" => 20,
        "g" => 30,
        "t" => 40,
        "p" => 50,
        _ => return None,
    };
    Some((num * (1u64 << shift) as f64) as u64)
}

impl TargetConfDefaults {
    pub fn read(conf: Option<&Path>, db: &str) -> Result<Self, CliError> {
        let Some(conf) = conf else {
            return Ok(Self::default());
        };
        let get = |name: &str| -> io::Result<Option<String>> { parameter(conf, db, name) };
        let bad =
            |name: &str, v: &str| CliError::Usage(format!("{}: bad {name}={v}", conf.display()));
        let size = |name: &str| -> Result<Option<u64>, CliError> {
            get(name)?
                .map(|v| engine_size(&v).ok_or_else(|| bad(name, &v)))
                .transpose()
        };
        Ok(TargetConfDefaults {
            db_volume_size: size("db_volume_size")?,
            data_buffer_size: size("data_buffer_size")?,
            temp_file_max_size_in_pages: get("temp_file_max_size_in_pages")?
                .map(|v| {
                    v.parse::<i64>()
                        .map_err(|_| bad("temp_file_max_size_in_pages", &v))
                })
                .transpose()?,
        })
    }

    /// The `--temp-disk-size` the conf gives: > 0 pages → that many pages of `page_size`, 0 → min, -1 or unset → none
    /// (asked on a terminal, else max).
    pub fn temp_disk(&self, page_size: u64) -> Option<TempDiskChoice> {
        match self.temp_file_max_size_in_pages {
            Some(n) if n > 0 => Some(TempDiskChoice::Bytes(n as u64 * page_size)),
            Some(0) => Some(TempDiskChoice::Min),
            _ => None,
        }
    }
}
