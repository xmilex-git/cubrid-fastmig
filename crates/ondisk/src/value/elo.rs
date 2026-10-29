//! BLOB/CLOB values (engine `DB_ELO`): the size, locator and meta data a record stores for an external LOB file.
//!
//! Input: OR bytes at a cursor. Output: [`Elo`], and its locator split into scheme and path for the LOB manifest and
//! the verifier's LOB check; or the bytes of an [`Elo`] (same in both versions).
//! Source: engine `src/object/object_primitive.c` `mr_data_writemem_elo`, `peekmem_elo` (develop @5b1867174
//! :5847, :5885; release/11.4 the same), `or_packed_string_length` (`object_representation.c:2216`); locator forms
//! `src/storage/es_posix.c` `es_make_abs_path`, `es_common.c` `es_get_type`; G6 workspace#488 (11.4 stores
//! `file:` paths relative to lob-base-path; 10.2–11.3 absolute).

use crate::value::db_type::DbType;
use crate::value::or_buf::{DecodeResult, OrReader, OrWriter};

/// One BLOB or CLOB value.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Elo {
    /// [`DbType::Blob`] or [`DbType::Clob`] (from the domain; the bytes do not say).
    pub ty: DbType,
    /// File size in bytes (engine `DB_ELO.size`).
    pub size: i64,
    /// Locator text without its NUL, e.g. `file:ces_123/t.00001672..._0001`.
    pub locator: Vec<u8>,
    /// Meta data text without its NUL; `None` when the record stores length 0.
    pub meta_data: Option<Vec<u8>>,
    /// Engine `DB_ELO_TYPE` (1 = `ELO_FBO`).
    pub elo_type: i32,
}

/// Where a locator points, by the engine's scheme prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LobPath<'a> {
    /// `file:` with a path that does not start with `/`: relative to the database's lob-base-path.
    Relative(&'a [u8]),
    /// `file:/...`: an absolute path.
    Absolute(&'a [u8]),
    /// Any other scheme (`owfs:`, `local:`) or no scheme; the whole locator.
    Unsupported(&'a [u8]),
}

impl Elo {
    /// Splits the locator (engine `es_get_type` + `es_make_abs_path`).
    pub fn lob_path(&self) -> LobPath<'_> {
        match self.locator.strip_prefix(b"file:") {
            Some(p) if p.starts_with(b"/") => LobPath::Absolute(p),
            Some(p) => LobPath::Relative(p),
            None => LobPath::Unsupported(&self.locator),
        }
    }
}

/// `or_put_string_aligned` text: bytes until the first NUL of a field of `len` bytes.
fn c_string(field: &[u8]) -> Vec<u8> {
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    field[..end].to_vec()
}

/// Reads one ELO: `size` BE i64, locator length BE i32 + padded C string, meta length BE i32 + padded C string,
/// `type` BE i32.
pub fn read_elo(r: &mut OrReader, ty: DbType) -> DecodeResult<Elo> {
    let size = r.i64()?;
    let loc_len = r.i32()?;
    if loc_len <= 0 {
        return Err(r.error(format!("ELO locator length {loc_len}")));
    }
    let locator = c_string(r.bytes(loc_len as usize)?);
    let meta_len = r.i32()?;
    let meta_data = if meta_len > 0 {
        Some(c_string(r.bytes(meta_len as usize)?))
    } else {
        None
    };
    let elo_type = r.i32()?;
    Ok(Elo {
        ty,
        size,
        locator,
        meta_data,
        elo_type,
    })
}

fn write_c_string(w: &mut OrWriter, s: Option<&[u8]>) {
    match s {
        // `or_packed_string_length - OR_INT_SIZE`: text + NUL rounded up to 4
        Some(s) => {
            let len = (s.len() + 1).div_ceil(4) * 4;
            w.i32(len as i32);
            w.bytes(s);
            w.zeros(len - s.len());
        }
        None => w.i32(0),
    }
}

/// Writes one ELO in the format [`read_elo`] reads (engine `mr_data_writemem_elo`).
pub fn write_elo(w: &mut OrWriter, e: &Elo) {
    w.i64(e.size);
    write_c_string(w, Some(&e.locator));
    write_c_string(w, e.meta_data.as_deref());
    w.i32(e.elo_type);
}
