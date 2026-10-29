//! Packed domains: the compact domain encoding that tags values in sets and class properties and describes index keys.
//!
//! Input: OR bytes at a cursor (a carrier word per domain, then optional collation, extended precision and scale,
//! class OID, enum labels, JSON schema, and element domains). Output: the domain chain and the tagged-NULL bit; or
//! the bytes for a chain.
//! Source: engine `src/object/object_representation.c` `or_put_domain`, `unpack_domain`, `unpack_domain_2`
//! (develop @5b1867174 :2696-3005, :3006-3500; release/11.4 @46212adcd :2859, :3513 — 11.4 also packs NCHAR/VARNCHAR
//! with a collation word, and writes scales up to 254 in the carrier where develop stops at 127).

use crate::Oid;
use crate::VersionLine;
use crate::value::db_type::DbType;
use crate::value::domain::{
    CollationFlag, Domain, FLOATING_PRECISION, MAX_VARBIT_PRECISION, MAX_VARCHAR_PRECISION,
    MAX_VARNCHAR_PRECISION_11_4,
};
use crate::value::or_buf::{DecodeResult, OrReader, OrWriter};
use crate::value::string::{read_string, write_string};

const TYPE_MASK: u32 = 0x3F;
const NEXT_FLAG: u32 = 0x80;
/// Tagged NULL on a value tag; DESC on an index key domain (same bit).
const NULL_OR_DESC_FLAG: u32 = 0x40;
/// Class OID (OBJECT/OID), element domains (collections, MIDXKEY), built-in reference (type NULL), labels (ENUM).
const FLAG_100: u32 = 0x100;
const ENUM_COLL_FLAG: u32 = 0x200;
const SCHEMA_FLAG: u32 = 0x400;
const SCALE_SHIFT: u32 = 8;
const SCALE_EXT: u32 = 0xFF;
const CODESET_SHIFT: u32 = 8;
const PRECISION_SHIFT: u32 = 16;
const PRECISION_EXT: u32 = 0xFFFF;
const COLLATION_MASK: u32 = 0xFF;
const COLL_ENFORCE_FLAG: u32 = 0x8000_0000;
const COLL_LEAVE_FLAG: u32 = 0x4000_0000;
/// Engine `LANG_COLL_ISO_BINARY`: the ENUM collation that is not packed.
const COLL_ISO_BINARY: i32 = 0;

fn is_string(version: VersionLine, ty: DbType) -> bool {
    match ty {
        DbType::Char | DbType::Varchar => true,
        DbType::Nchar | DbType::Varnchar => version == VersionLine::V11_4,
        _ => false,
    }
}

fn has_set_domain(ty: DbType) -> bool {
    ty.is_set() || matches!(ty, DbType::Table | DbType::Midxkey)
}

/// The type a built-in reference names: `tp_Domains[index - 1]` (`object_domain.c:329-387`, same slot list in
/// release/11.4 and develop). Slots 21 (`DB_VALUE`), 30 (`TABLE`) and 41-47 (padding) hold the NULL domain; develop
/// keeps slots 26/27 but fills them with `#define tp_NChar_domain tp_Char_domain` and
/// `tp_VarNChar_domain tp_String_domain` (develop :327-362).
fn builtin_type(version: VersionLine, slot: u32) -> Option<DbType> {
    match (version, slot) {
        (_, 21 | 30 | 41..=47) => Some(DbType::Null),
        (VersionLine::Develop, 26) => Some(DbType::Char),
        (VersionLine::Develop, 27) => Some(DbType::Varchar),
        _ => DbType::from_id(version, slot),
    }
}

/// The 1-based `built_in_index` the engine writes for a built-in domain. `tp_domain_init` sets
/// `built_in_index = i + 1` slot by slot, so a domain listed in several slots keeps its last one: NULL 48 (both
/// versions), and in develop CHAR 27 and VARCHAR 28.
fn builtin_index(version: VersionLine, ty: DbType) -> u32 {
    match (version, ty) {
        (_, DbType::Null) => 48,
        (VersionLine::Develop, DbType::Char) => 27,
        (VersionLine::Develop, DbType::Varchar) => 28,
        _ => ty.id() as u32 + 1,
    }
}

/// What the 0x40 bit of a carrier means. The engine caller decides by passing `is_null` to `or_get_domain` or not
/// (`unpack_domain`, develop @5b1867174 :3376-3400).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bit40 {
    /// A value tag (`or_get_value`): the bit marks a NULL value; the tag is never DESC.
    Null,
    /// A domain (index key, set, XASL): the bit marks a DESC key column.
    Desc,
}

/// Reads one packed domain chain whose 0x40 bit means DESC (engine `or_get_domain` with `is_null == NULL`): index
/// key domains, set domains, XASL domains.
pub fn decode_packed_domain(version: VersionLine, r: &mut OrReader) -> DecodeResult<Vec<Domain>> {
    Ok(decode_chain(version, r, Bit40::Desc)?.0)
}

/// Reads the packed domain that tags a value (engine `or_get_domain` with `is_null`): the chain, never DESC, and
/// whether the first carrier marks the value NULL.
pub fn decode_value_tag(
    version: VersionLine,
    r: &mut OrReader,
) -> DecodeResult<(Vec<Domain>, bool)> {
    decode_chain(version, r, Bit40::Null)
}

fn decode_chain(
    version: VersionLine,
    r: &mut OrReader,
    bit40: Bit40,
) -> DecodeResult<(Vec<Domain>, bool)> {
    let mut chain = Vec::new();
    let mut is_null = false;
    loop {
        let carrier = r.u32()?;
        let id = carrier & TYPE_MASK;
        if chain.is_empty() && bit40 == Bit40::Null {
            is_null = carrier & NULL_OR_DESC_FLAG != 0;
        }
        if id == 0 && carrier & FLAG_100 != 0 {
            // built-in reference: the 1-based type id sits in the precision field
            let index = (carrier >> PRECISION_SHIFT) & 0xFFFF;
            let ty = index
                .checked_sub(1)
                .and_then(|t| builtin_type(version, t))
                .ok_or_else(|| r.error(format!("bad built-in domain index {index}")))?;
            chain.push(Domain::builtin(version, ty));
            return Ok((chain, is_null));
        }
        let ty = DbType::from_id(version, id)
            .ok_or_else(|| r.error(format!("unknown domain type id {id}")))?;
        let mut d = Domain::of_type(ty);
        d.is_desc = bit40 == Bit40::Desc && carrier & NULL_OR_DESC_FLAG != 0;
        let mut precision = 0u32;
        let mut scale = 0u32;
        let mut has_collation = false;
        let mut string_like = false;
        match ty {
            DbType::Numeric => {
                precision = carrier >> PRECISION_SHIFT;
                scale = (carrier >> SCALE_SHIFT) & 0xFF;
            }
            DbType::Bit | DbType::Varbit => string_like = true,
            t if is_string(version, t) => {
                has_collation = true;
                string_like = true;
            }
            DbType::Midxkey => precision = carrier >> PRECISION_SHIFT,
            DbType::Enumeration => has_collation = carrier & ENUM_COLL_FLAG != 0,
            _ => {}
        }
        if string_like {
            d.codeset = ((carrier >> CODESET_SHIFT) & 0xFF) as u8;
            precision = carrier >> PRECISION_SHIFT;
        }
        if has_collation {
            let c = r.u32()?;
            d.collation_id = (c & COLLATION_MASK) as i32;
            d.collation_flag = if c & COLL_ENFORCE_FLAG != 0 {
                CollationFlag::Enforce
            } else if c & COLL_LEAVE_FLAG != 0 {
                CollationFlag::Leave
            } else {
                CollationFlag::Normal
            };
        } else if ty == DbType::Enumeration {
            d.collation_id = COLL_ISO_BINARY;
        }
        if precision == PRECISION_EXT {
            precision = r.u32()?;
        }
        if scale == SCALE_EXT {
            scale = r.u32()?;
        }
        d.precision = precision as i32;
        d.scale = scale as i32;
        if string_like && d.precision == 0 {
            d.precision = match ty {
                DbType::Varchar => MAX_VARCHAR_PRECISION,
                DbType::Varnchar => MAX_VARNCHAR_PRECISION_11_4,
                DbType::Varbit => MAX_VARBIT_PRECISION,
                _ => FLOATING_PRECISION,
            };
        }
        if matches!(ty, DbType::Object | DbType::Oid) && carrier & FLAG_100 != 0 {
            let pageid = r.i32()?;
            let slotid = r.i16()?;
            let volid = r.i16()?;
            d.class_oid = Some(Oid::new(volid, pageid, slotid));
        }
        if ty == DbType::Enumeration && carrier & FLAG_100 != 0 {
            // `or_put_enumeration`: a set header (type word, count) and one VARCHAR string per label
            let _header = r.u32()?;
            let count = r.i32()?;
            for _ in 0..count.max(0) {
                d.enum_labels.push(read_string(r)?);
            }
        }
        if ty == DbType::Json && carrier & SCHEMA_FLAG != 0 {
            d.json_schema = Some(read_string(r)?);
        }
        if has_set_domain(ty) && carrier & FLAG_100 != 0 {
            d.set_domain = decode_packed_domain(version, r)?;
        }
        chain.push(d);
        if carrier & NEXT_FLAG == 0 {
            return Ok((chain, is_null));
        }
    }
}

/// Writes a packed domain chain (engine `or_put_domain`). `include_class_oids` packs OBJECT class OIDs;
/// `is_null` sets the tagged-NULL bit on each carrier, as the engine does for a NULL tag.
/// An 11.4-only type (NCHAR/VARNCHAR) written as develop is the caller's error and panics.
pub fn encode_packed_domain(
    version: VersionLine,
    w: &mut OrWriter,
    chain: &[Domain],
    include_class_oids: bool,
    is_null: bool,
) {
    if let [d] = chain
        && d.builtin
    {
        let mut carrier = FLAG_100 | (builtin_index(version, d.ty) << PRECISION_SHIFT);
        if is_null {
            carrier |= NULL_OR_DESC_FLAG;
        }
        w.u32(carrier);
        return;
    }
    for (i, d) in chain.iter().enumerate() {
        assert!(
            DbType::from_id(version, d.ty.id() as u32).is_some(),
            "{:?} has no {version:?} encoding",
            d.ty
        );
        let mut carrier = d.ty.id() as u32 & TYPE_MASK;
        if i + 1 < chain.len() {
            carrier |= NEXT_FLAG;
        }
        if d.is_desc || is_null {
            carrier |= NULL_OR_DESC_FLAG;
        }
        let mut precision: u32 = 0;
        let mut ext_scale: Option<i32> = None;
        let mut has_collation = false;
        let mut has_oid = false;
        let mut has_sub = false;
        let mut has_enum = false;
        let mut has_schema = false;
        match d.ty {
            DbType::Numeric => {
                let scale = d.scale.max(0);
                let direct_max = match version {
                    VersionLine::V11_4 => 255,
                    VersionLine::Develop => 128,
                };
                if d.scale < 0 || scale >= direct_max {
                    carrier |= SCALE_EXT << SCALE_SHIFT;
                    ext_scale = Some(d.scale);
                } else {
                    carrier |= (scale as u32) << SCALE_SHIFT;
                }
                precision = if d.precision <= FLOATING_PRECISION {
                    match version {
                        VersionLine::V11_4 => 38,
                        VersionLine::Develop => 40,
                    }
                } else {
                    d.precision as u32
                };
            }
            t if is_string(version, t) || matches!(t, DbType::Bit | DbType::Varbit) => {
                has_collation = is_string(version, t);
                carrier |= (d.codeset as u32) << CODESET_SHIFT;
                let max = match t {
                    DbType::Varchar => MAX_VARCHAR_PRECISION,
                    DbType::Varnchar => MAX_VARNCHAR_PRECISION_11_4,
                    DbType::Varbit => MAX_VARBIT_PRECISION,
                    _ => i32::MIN,
                };
                if d.precision != FLOATING_PRECISION && d.precision != max {
                    precision = d.precision as u32;
                }
            }
            DbType::Object | DbType::Oid => {
                if include_class_oids {
                    carrier |= FLAG_100;
                    has_oid = true;
                }
            }
            t if has_set_domain(t) => {
                if !d.set_domain.is_empty() {
                    carrier |= FLAG_100;
                    has_sub = true;
                }
                if t == DbType::Midxkey {
                    precision = d.precision as u32;
                }
            }
            DbType::Enumeration => {
                if d.collation_id != COLL_ISO_BINARY {
                    has_collation = true;
                    carrier |= ENUM_COLL_FLAG;
                }
                if !d.enum_labels.is_empty() {
                    carrier |= FLAG_100;
                    has_enum = true;
                }
            }
            DbType::Json if d.json_schema.is_some() => {
                carrier |= SCHEMA_FLAG;
                has_schema = true;
            }
            _ => {}
        }
        let mut ext_precision = None;
        if precision != 0 {
            if precision < PRECISION_EXT {
                carrier |= precision << PRECISION_SHIFT;
            } else {
                carrier |= PRECISION_EXT << PRECISION_SHIFT;
                ext_precision = Some(precision);
            }
        }
        w.u32(carrier);
        if has_collation {
            let mut c = d.collation_id as u32;
            match d.collation_flag {
                CollationFlag::Enforce => c |= COLL_ENFORCE_FLAG,
                CollationFlag::Leave => c |= COLL_LEAVE_FLAG,
                CollationFlag::Normal => {}
            }
            w.u32(c);
        }
        if let Some(p) = ext_precision {
            w.u32(p);
        }
        // the engine writes the extended scale only when it is non-zero
        if let Some(s) = ext_scale.filter(|s| *s != 0) {
            w.i32(s);
        }
        if has_oid {
            let o = d.class_oid.unwrap_or(Oid::NULL);
            w.i32(o.pageid);
            w.i16(o.slotid);
            w.i16(o.volid);
        }
        if has_enum {
            // set header word: SEQUENCE with no flags, then the count
            w.u32(DbType::Sequence.id() as u32);
            w.i32(d.enum_labels.len() as i32);
            for l in &d.enum_labels {
                write_string(w, l);
            }
        }
        if has_schema {
            write_string(w, d.json_schema.as_deref().unwrap_or_default());
        }
        if has_sub {
            encode_packed_domain(version, w, &d.set_domain, include_class_oids, false);
        }
    }
}
