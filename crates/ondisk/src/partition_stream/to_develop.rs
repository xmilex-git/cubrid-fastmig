//! Rewrites a parsed 11.4 or 10.2 stream tree in develop terms without evaluating anything: opcodes renumbered,
//! language flags given the 11.4 parity, domains and constants mapped (NCHAR → CHAR, VARNCHAR → VARCHAR), DB_TYPE
//! words of attribute and comparison nodes mapped. Values are re-encoded later, when [`super::write`] writes them in
//! the develop layout.
//!
//! Input: a [`StreamTree`] parsed from an 11.4 or 10.2 stream. Output: the same tree, in place, ready to write as
//! develop. 10.2 differs from 11.4 here only in its opcode and function code numbers ([`opcode`], R13 § 2: DB_TYPE,
//! MISC_OPERAND, REGU_DATATYPE and node layouts are the same) and, like 11.2 and 11.3, in language flags without
//! the parity develop checks.
//! Source: R7 § 2.3-2.4 (workspace#487), G2 decision 5 (workspace#482, NCHAR/VARNCHAR mapping in partition
//! streams); engine `object_domain.c:tp_domain_init` (a built-in NUMERIC reference names
//! `DB_DEFAULT_NUMERIC_PRECISION`, 15 in release/10.2 @416bb667c and release/11.4 @46212adcd, larger in develop).

use super::opcode;
use super::tree::{Node, PredExpr, ReguValue, StreamTree};
use crate::VersionLine;
use crate::value::{
    DbType, DecodeError, DecodeResult, Domain, Value, domain_to_develop, value_to_develop,
};

/// 11.4 (and 10.2) built-in NUMERIC domain: `DB_DEFAULT_NUMERIC_PRECISION`, `DB_DEFAULT_NUMERIC_SCALE`.
const NUMERIC_BUILTIN_11_4: (i32, i32) = (15, 0);

/// The develop domain of an 11.4 stream domain. A built-in NUMERIC reference becomes an explicit NUMERIC(15,0), so
/// develop does not read it as its own wider default.
fn domain(d: &Domain) -> Domain {
    let mut out = domain_to_develop(d);
    if out.builtin && out.ty == DbType::Numeric {
        out.builtin = false;
        (out.precision, out.scale) = NUMERIC_BUILTIN_11_4;
    }
    out
}

/// A DB_TYPE word stored in a node (ATTR_DESCR type, comparison type, ALSM item type): 26/27 → CHAR/VARCHAR.
fn db_type(id: i32) -> i32 {
    if id == DbType::Nchar.id() as i32 {
        DbType::Char.id() as i32
    } else if id == DbType::Varnchar.id() as i32 {
        DbType::Varchar.id() as i32
    } else {
        id
    }
}

fn regu_value(v: &mut ReguValue) {
    match v {
        ReguValue::DbVal { tag, value } => {
            *tag = domain(tag);
            *value = value_to_develop(std::mem::replace(value, Value::Null));
        }
        ReguValue::Attr { db_type: t, .. } => *t = db_type(*t),
        ReguValue::ValList(items) => items.iter_mut().for_each(|(_, i)| regu_value(i)),
        ReguValue::Ptr(_) | ReguValue::PosValue(_) | ReguValue::Empty | ReguValue::VarList(_) => {}
    }
}

/// Engine `LANG_LOADED_LOCALES_PARITY` and its mask (language_support.h): since 11.4 (CBRD-25302, 73cc3fd40) the
/// compiler sets it in every language flag, and develop takes a flag without it for a host variable and refuses it.
const LANG_PARITY: i32 = 0x75A5_C000;
const LANG_PARITY_MASK: i32 = 0xFFFF_F000_u32 as i32;

/// The develop language flag of a stored one. A source before 11.4 (10.2..11.3) stores the bare
/// `lang id << 2 | has_user_lang << 1 | has_user_format`, the same bits develop keeps below the parity; an 11.4 flag
/// already carries the parity and stays (workspace#540, ADR 0017).
fn lang_flag(v: i32) -> i32 {
    if v & LANG_PARITY_MASK == 0 {
        v | LANG_PARITY
    } else {
        v
    }
}

/// Engine `REGU_VARIABLE_INFER_COLLATION`: develop's unpack asserts it is absent (retired, no setter since 2015).
const REGU_INFER_COLLATION: i32 = 0x20;

/// Maps every node of a tree `from` wrote to develop. Refuses a node develop cannot load.
pub fn map_tree(from: VersionLine, tree: &mut StreamTree) -> DecodeResult<()> {
    for node in &mut tree.nodes {
        match node {
            Node::Regu {
                domain: d,
                flags,
                value,
                ..
            } => {
                if *flags & REGU_INFER_COLLATION != 0 {
                    return Err(DecodeError {
                        at: 0,
                        what: "REGU_VARIABLE_INFER_COLLATION, which develop refuses".into(),
                    });
                }
                *d = d.iter().map(domain).collect();
                regu_value(value);
            }
            Node::DbValue { tag, value } => {
                *tag = domain(tag);
                *value = value_to_develop(std::mem::replace(value, Value::Null));
            }
            Node::Arith {
                domain: d,
                opcode: o,
                ..
            } => {
                *d = d.iter().map(domain).collect();
                *o = opcode::to_develop(from, *o);
            }
            Node::Pred(PredExpr::Comp { db_type: t, .. }) => *t = db_type(*t),
            Node::Pred(PredExpr::Alsm { item_type: t, .. }) => *t = db_type(*t),
            Node::Function { ftype, .. } if !opcode::function_kept(from, *ftype) => {
                return Err(DecodeError {
                    at: 0,
                    what: format!(
                        "function code {ftype} (F_BENCHMARK or later in {from:?}) in a partition expression"
                    ),
                });
            }
            Node::FuncPred { .. }
            | Node::CacheAttrinfo
            | Node::Function { .. }
            | Node::ReguList(_)
            | Node::Pred(_) => {}
        }
    }
    // after the opcodes are develop numbers: the language flags of the TO_CHAR/TO_DATE family
    let flags: Vec<_> = tree
        .nodes
        .iter()
        .filter_map(|n| match n {
            Node::Arith { opcode, third, .. } if opcode::takes_lang_flag(*opcode) => *third,
            _ => None,
        })
        .collect();
    for id in flags {
        if let Node::Regu {
            value:
                ReguValue::DbVal {
                    value: Value::Integer(v),
                    ..
                },
            ..
        } = &mut tree.nodes[id]
        {
            *v = lang_flag(*v);
        }
    }
    super::char_result::map_char_results(tree)
}
