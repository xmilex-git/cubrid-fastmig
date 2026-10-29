//! Rewrites a parsed 11.4 stream tree in develop terms without evaluating anything: opcodes renumbered, domains and
//! constants mapped (NCHAR → CHAR, VARNCHAR → VARCHAR), DB_TYPE words of attribute and comparison nodes mapped.
//! Values are re-encoded later, when [`super::write`] writes them in the develop layout.
//!
//! Input: a [`StreamTree`] parsed from an 11.4 stream. Output: the same tree, in place, ready to write as develop.
//! Source: R7 § 2.3-2.4 (workspace#487), G2 decision 5 (workspace#482, NCHAR/VARNCHAR mapping in partition
//! streams); engine `object_domain.c:tp_domain_init` (a built-in NUMERIC reference names
//! `DB_DEFAULT_NUMERIC_PRECISION`, 15 in release/11.4 @46212adcd, larger in develop).

use super::opcode;
use super::tree::{Node, PredExpr, ReguValue, StreamTree};
use crate::value::{
    DbType, DecodeError, DecodeResult, Domain, domain_to_develop, value_to_develop,
};

/// 11.4 built-in NUMERIC domain: `DB_DEFAULT_NUMERIC_PRECISION`, `DB_DEFAULT_NUMERIC_SCALE`.
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
            *value = value_to_develop(value);
        }
        ReguValue::Attr { db_type: t, .. } => *t = db_type(*t),
        ReguValue::ValList(items) => items.iter_mut().for_each(|(_, i)| regu_value(i)),
        ReguValue::Ptr(_) | ReguValue::PosValue(_) | ReguValue::Empty | ReguValue::VarList(_) => {}
    }
}

/// Engine `REGU_VARIABLE_INFER_COLLATION`: develop's unpack asserts it is absent (retired, no setter since 2015).
const REGU_INFER_COLLATION: i32 = 0x20;

/// Maps every node of an 11.4 tree to develop. Refuses a node develop cannot load.
pub fn map_tree(tree: &mut StreamTree) -> DecodeResult<()> {
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
                *value = value_to_develop(value);
            }
            Node::Arith {
                domain: d,
                opcode: o,
                ..
            } => {
                *d = d.iter().map(domain).collect();
                *o = opcode::to_develop(*o);
            }
            Node::Pred(PredExpr::Comp { db_type: t, .. }) => *t = db_type(*t),
            Node::Pred(PredExpr::Alsm { item_type: t, .. }) => *t = db_type(*t),
            Node::FuncPred { .. }
            | Node::CacheAttrinfo
            | Node::Function { .. }
            | Node::ReguList(_)
            | Node::Pred(_) => {}
        }
    }
    Ok(())
}
