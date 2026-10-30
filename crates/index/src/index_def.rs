//! What the tool needs to build one plain index of the target: the classes whose rows it holds, its shape (unique,
//! primary key, top class, packed key domain) and its key domain, all from the target class objects in the target line
//! (develop, or 11.4 for a target of the second stage, workspace#530).
//!
//! Input: the constraint in each owner class (`ClassObject` + `Constraint` in the target line; the classes that share one BTID
//! are its owners, as the verifier's `IndexSpec`). Output: [`IndexDef`], or a refusal for an index the tool does
//! not build (function or filter index: deferred, workspace#486; prefix index; a key attribute missing).
//! The key domain is built the way the engine builds the one it stores in the root header, so a tree that never
//! existed (a new catalog table's index, workspace#504) gets the same one as a rebuilt tree.
//! Source: engine `schema_manager.c` `allocate_index` (unique family = `*U`/`*RU`/`*P`, `*P` NOT NULL) and
//! `construct_index_key_domain` (develop 682b080b2 :10356-10501): one column → the attribute's domain, a copy with
//! `is_desc` when DESC; several → MIDXKEY of copies (type, precision, scale, codeset, collation, ENUM labels,
//! `is_desc`; no class OID); `btree.c` root header `or_pack_domain (key_type, 0, 0)` (no class OIDs).
//! An 11.4 DEDUPLICATE index keeps its hidden SHORT column last in the MIDXKEY (also with one real column) and stores
//! the column count at root @70 (release/11.4 `deduplicate_key.c:dk_create_index_level_adjust`,
//! `btree_load.h:SET_DECOMPRESS_IDX_HEADER`; R16 workspace#532, T14 workspace#537 measured on engine trees).

use fastmig_ondisk::class_object::{
    CLASSFLAG_SYSTEM, ClassObject, Constraint, ConstraintKind, DeduplicateKey,
};
use fastmig_ondisk::record::layout::schema_domain;
use fastmig_ondisk::value::string::codeset;
use fastmig_ondisk::value::{
    DbType, DecodeError, DecodeResult, Domain, KeyDomain, OrWriter, encode_packed_domain,
};
use fastmig_ondisk::{Btid, Hfid, Oid, VersionLine};

use crate::index_shape::{DEFAULT_UNFILL_FACTOR, IndexShape};

/// One class whose live rows go into the index, with the index's constraint as that class names it.
#[derive(Clone, Copy)]
pub struct IndexOwner<'c> {
    pub class_oid: Oid,
    pub class: &'c ClassObject,
    pub hfid: Hfid,
    pub constraint: &'c Constraint,
}

/// One plain index to fill: its BTID (from `btree_file::create_btree_file`) and what the build needs.
pub struct IndexDef<'c> {
    pub btid: Btid,
    pub owners: Vec<IndexOwner<'c>>,
    pub shape: IndexShape,
    pub key_domain: KeyDomain,
}

fn refuse(what: String) -> DecodeError {
    DecodeError { at: 0, what }
}

/// The shape and key domain of the index `owner.constraint` names, in the `target` line's format. `db_charset`:
/// the volume header `db_charset` (the engine's built-in string domain follows it).
pub fn index_shape(
    owner: &IndexOwner,
    io_pagesize: usize,
    db_charset: u8,
    target: VersionLine,
) -> DecodeResult<(IndexShape, KeyDomain)> {
    let c = owner.constraint;
    let name = String::from_utf8_lossy(&c.name);
    if c.has_function || c.has_filter {
        return Err(refuse(format!(
            "{name}: function and filter indexes are deferred (workspace#486)"
        )));
    }
    if has_prefix_lengths(c) {
        // shortcut: prefix indexes (*PLID*) are refused; 11.4 no longer creates them, upgrade if a source has one
        return Err(refuse(format!("{name}: prefix index is not supported")));
    }
    let deduplicate_key = c.deduplicate_key.filter(|_| target == VersionLine::V11_4);
    let domain = key_domain(owner.class, c, db_charset, target, deduplicate_key)?;
    let kd = KeyDomain::new(target, &domain)?;
    // 11.4 `rev_level:16 | deduplicate_key_idx + 1:16`: the hidden column is the last of the K columns
    let root_off68 = match deduplicate_key {
        Some(_) => (kd.n_columns() as i32) << 16,
        None => 0,
    };
    let mut w = OrWriter::new();
    encode_packed_domain(target, &mut w, &[domain], false, false);
    let unique = matches!(
        c.kind,
        ConstraintKind::Unique | ConstraintKind::ReverseUnique | ConstraintKind::PrimaryKey
    );
    Ok((
        IndexShape {
            unique,
            primary_key: c.kind == ConstraintKind::PrimaryKey,
            topclass: owner.class_oid,
            packed_key_domain: w.buf,
            root_off68,
            target,
            io_pagesize,
            unfill_factor: DEFAULT_UNFILL_FACTOR,
        },
        kd,
    ))
}

/// The first key attribute: the B-tree file descriptor's `attr_id` (engine `attr_ids[0]`).
pub fn first_key_attr(c: &Constraint) -> i32 {
    c.keys.first().map_or(-1, |k| k.0)
}

/// Engine `construct_index_key_domain` without the function-index part; with `deduplicate_key` (11.4 only) the
/// hidden SHORT column goes last, DESC as the constraint lists it.
fn key_domain(
    class: &ClassObject,
    c: &Constraint,
    db_charset: u8,
    line: VersionLine,
    deduplicate_key: Option<DeduplicateKey>,
) -> DecodeResult<Domain> {
    let cols = c
        .keys
        .iter()
        .map(|&(id, asc_desc)| {
            let a = class
                .attributes
                .iter()
                .find(|a| a.id == id)
                .ok_or_else(|| refuse(format!("key attribute {id} is not in the class")))?;
            Ok((schema_domain(line, &a.domain)?, asc_desc == 1))
        })
        .collect::<DecodeResult<Vec<(Domain, bool)>>>()?;
    let hidden = deduplicate_key.map(|h| (Domain::of_type(DbType::Short), h.desc));
    let cols: Vec<(Domain, bool)> = cols.into_iter().chain(hidden).collect();
    match cols.as_slice() {
        [] => Err(refuse("index without key attributes".into())),
        [(d, desc)] => {
            let mut d = d.clone();
            d.is_desc = *desc;
            // the attribute's domain may be the cached built-in one; a DESC copy is not. 11.4 createdb indexes its
            // system classes' VARCHAR keys with an explicit domain (11.4.6 roots of `db_user.name` and
            // `db_serial.unique_name` on T1 and a fresh SA createdb; a user table's same key is built-in), workspace#530
            let system_varchar = line == VersionLine::V11_4
                && d.ty == DbType::Varchar
                && class.flags & CLASSFLAG_SYSTEM != 0;
            d.builtin =
                !desc && !system_varchar && is_builtin_attribute_domain(&d, db_charset, line);
            Ok(d)
        }
        _ => {
            let set_domain = cols
                .into_iter()
                .map(|(d, desc)| Domain {
                    is_desc: desc,
                    class_oid: None,
                    set_domain: Vec::new(),
                    json_schema: None,
                    builtin: false,
                    ..d
                })
                .collect::<Vec<_>>();
            Ok(Domain {
                precision: set_domain.len() as i32,
                set_domain,
                ..Domain::of_type(DbType::Midxkey)
            })
        }
    }
}

/// Whether the attribute's domain is the engine's built-in one of its type, which `or_pack_domain` writes as a
/// one-word reference (`tp_domain_cache` hands back the built-in when every parameter matches): a type without
/// parameters (OBJECT only without a class), or VARCHAR/VARBIT/NUMERIC at the built-in precision and scale — VARCHAR
/// also in the database charset and its binary collation (`LANG_SYS_COLLATION`). T1 develop: `_db_serial` and
/// `_db_trigger` `unique_name` keys (VARCHAR, utf8, `utf8_bin`).
fn is_builtin_attribute_domain(d: &Domain, db_charset: u8, line: VersionLine) -> bool {
    let b = Domain::builtin(line, d.ty);
    match d.ty {
        // 11.4 `tp_VarNChar_domain` takes the database charset like `tp_String_domain` (object_domain.c:694-703)
        DbType::Varchar | DbType::Varnchar => {
            d.precision == b.precision
                && d.codeset == db_charset
                && d.collation_id == binary_collation(db_charset)
        }
        DbType::Varbit | DbType::Numeric => d.precision == b.precision && d.scale == b.scale,
        DbType::Char | DbType::Nchar | DbType::Bit | DbType::Enumeration | DbType::Json => false,
        t if t.is_set() => false,
        _ => d.class_oid.is_none(),
    }
}

/// Engine `LANG_GET_BINARY_COLLATION` (language_support.h, develop 682b080b2).
fn binary_collation(charset: u8) -> i32 {
    match charset {
        codeset::UTF8 => 1,
        codeset::KSC5601_EUC => 8,
        codeset::ISO88591 => 0,
        _ => 9,
    }
}

fn has_prefix_lengths(c: &Constraint) -> bool {
    use fastmig_ondisk::value::Value;
    let Some(Value::Set(s)) = &c.optional else {
        return false;
    };
    s.elements.iter().any(|e| match &e.value {
        Value::Set(p) => matches!(
            p.elements.first().map(|n| &n.value),
            Some(Value::Varchar(n) | Value::Char(n)) if n.as_slice() == b"*PLID*"
        ),
        _ => false,
    })
}
