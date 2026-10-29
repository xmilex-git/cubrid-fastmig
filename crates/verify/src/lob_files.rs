//! Check 5: each LOB value of a live row names its referenced LOB file, which exists with the value's size.
//!
//! Input: the LOB values the heap passes collected and the database's lob-base-path. Output: check 5 defects —
//! `lob-missing`, `lob-size`, `lob-unsupported` (a scheme other than `file:`), `lob-no-base` (a relative locator and
//! no lob-base-path).
//! The locators come from the heap pass, which decodes every live record anyway (check 1 (iv)); so the check needs
//! no `<dest>/fastmig/lob-manifest` and covers a source the same way as a target (G6 asked for the manifest only to
//! avoid a second record scan).
//! Source: G6 workspace#488 (relative locators under the lob-base-path, absolute ones as they are); ondisk
//! `Elo::lob_path` (engine `es_get_type`, `es_make_abs_path`).

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use fastmig_ondisk::value::LobPath;

use crate::defect::{Check, Defect, OidText};
use crate::object_refs::LobRef;

pub fn check_lobs(lobs: &[LobRef], base: Option<&Path>, class: &str) -> Vec<Defect> {
    let mut out = Vec::new();
    for l in lobs {
        let d = |code| {
            Defect::new(Check::Lob, code)
                .class(class)
                .object(format!("{} attr {}", OidText(l.from), l.attr))
                .at(String::from_utf8_lossy(&l.elo.locator).into_owned())
        };
        let path: PathBuf = match l.elo.lob_path() {
            LobPath::Absolute(p) => Path::new(std::ffi::OsStr::from_bytes(p)).to_path_buf(),
            LobPath::Relative(p) => match base {
                Some(b) => b.join(std::ffi::OsStr::from_bytes(p)),
                None => {
                    out.push(d("lob-no-base"));
                    continue;
                }
            },
            LobPath::Unsupported(_) => {
                out.push(d("lob-unsupported"));
                continue;
            }
        };
        match std::fs::metadata(&path) {
            Ok(m) if m.len() as i64 == l.elo.size => {}
            Ok(m) => out.push(d("lob-size").expected(l.elo.size).actual(m.len())),
            Err(e) => out.push(d("lob-missing").expected(path.display()).actual(e)),
        }
    }
    out
}
