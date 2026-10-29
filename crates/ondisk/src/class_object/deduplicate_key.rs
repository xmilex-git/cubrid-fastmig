//! The hidden key attribute of an 11.3/11.4 DEDUPLICATE index, and its removal from a constraint entry.
//!
//! Input: the elements of one 11.4 constraint info sequence between its BTID and its tail: the `(attribute id,
//! asc_desc)` key pairs and the optional element.
//! Output: the same elements as the plain index the target keeps: the hidden key pair dropped, the prefix length
//! list cut to the real keys, a function index's `attr_index_start` lowered past the hidden key.
//! Source: release/11.4 `deduplicate_key.h` (`MK_DEDUPLICATE_KEY_ATTR_ID(level)` = 0x8A8B8C90 − level, level
//! 1..14), `deduplicate_key.c:dk_create_index_level_adjust` (hidden attribute last, or at `attr_index_start` ahead of
//! the function arguments with `attr_index_start` + 1), `class_object.c:classobj_put_index` (one prefix length per
//! key attribute, hidden included; plain list, or `*PLID*` next to a filter); develop has no hidden key
//! (CUBRID/cubrid#8104) and asserts one prefix length per key (`class_object.c:2955`); R16 workspace#532 § 3.4, 3.7.

use super::constraint::ConstraintKind;
use crate::value::{SetElement, SetValue, Value};

/// 11.4 `MK_DEDUPLICATE_KEY_ATTR_ID(0)`.
const DEDUPLICATE_KEY_ID_BASE: i32 = 0x8A8B_8C90_u32 as i32;

/// The attribute id of a DEDUPLICATE index's hidden key (`_dedup_NN`, NN = level 1..14).
pub fn is_deduplicate_key(attr_id: i32) -> bool {
    (DEDUPLICATE_KEY_ID_BASE - 14..=DEDUPLICATE_KEY_ID_BASE - 1).contains(&attr_id)
}

fn key_id(e: &SetElement) -> Option<i32> {
    match e.value {
        Value::Integer(i) => Some(i),
        _ => None,
    }
}

/// `list` without the elements at `hidden` positions, when it has one element per key (`keys`); else unchanged.
fn without_positions(list: &SetValue, hidden: &[usize], keys: usize) -> SetValue {
    let mut out = list.clone();
    if out.elements.len() == keys {
        out.elements = (0..keys)
            .filter(|i| !hidden.contains(i))
            .map(|i| list.elements[i].clone())
            .collect();
    }
    out
}

/// One `{name, info}` entry of a named optional sequence with the hidden key taken out of its info.
fn named_entry(entry: &SetElement, hidden: &[usize], keys: usize) -> SetElement {
    let Value::Set(pair) = &entry.value else {
        return entry.clone();
    };
    let (
        Some(name),
        Some(SetElement {
            value: Value::Set(info),
            tag,
        }),
    ) = (pair.elements.first(), pair.elements.get(1))
    else {
        return entry.clone();
    };
    let info = match &name.value {
        Value::Varchar(n) | Value::Char(n) if n.as_slice() == b"*PLID*" => {
            without_positions(info, hidden, keys)
        }
        // function info {expr_str, expr_stream, col_id, attr_index_start, fi_domain}: the hidden key sits in
        // front of attr_index_start
        Value::Varchar(n) | Value::Char(n) if n.as_slice() == b"*FI*" => {
            let mut info = info.clone();
            if let Some(SetElement {
                value: Value::Integer(start),
                ..
            }) = info.elements.get_mut(3)
            {
                *start -= hidden.iter().filter(|&&p| (p as i32) < *start).count() as i32;
            }
            info
        }
        _ => return entry.clone(),
    };
    let mut pair = pair.clone();
    pair.elements[1] = SetElement {
        tag: tag.clone(),
        value: Value::Set(info),
    };
    SetElement {
        tag: entry.tag.clone(),
        value: Value::Set(pair),
    }
}

/// The key pairs and optional element of an 11.4 constraint entry, without a DEDUPLICATE index's hidden key.
/// Elements with no hidden key come back as they are. `kind` tells a foreign key's info (kept) from an index's prefix
/// lengths.
pub fn without_deduplicate_key(kind: ConstraintKind, elements: &[SetElement]) -> Vec<SetElement> {
    let (pairs, optional) = match elements.last() {
        Some(
            o @ SetElement {
                value: Value::Set(_),
                ..
            },
        ) => (&elements[..elements.len() - 1], Some(o)),
        _ => (elements, None),
    };
    let hidden: Vec<usize> = pairs
        .chunks(2)
        .enumerate()
        .filter(|(_, k)| key_id(&k[0]).is_some_and(is_deduplicate_key))
        .map(|(i, _)| i)
        .collect();
    if hidden.is_empty() {
        return elements.to_vec();
    }
    let keys = pairs.chunks(2).count();
    let mut out: Vec<SetElement> = pairs
        .chunks(2)
        .enumerate()
        .filter(|(i, _)| !hidden.contains(i))
        .flat_map(|(_, k)| k.iter().cloned())
        .collect();
    if let Some(o) = optional {
        let Value::Set(list) = &o.value else {
            unreachable!("matched as a set above")
        };
        let named = list
            .elements
            .iter()
            .any(|e| matches!(e.value, Value::Set(_)));
        let list = if matches!(
            kind,
            ConstraintKind::ForeignKey | ConstraintKind::PrimaryKey
        ) {
            // FK info and a PK's referencing FKs name no key position
            list.clone()
        } else if named {
            SetValue {
                elements: list
                    .elements
                    .iter()
                    .map(|e| named_entry(e, &hidden, keys))
                    .collect(),
                ..list.clone()
            }
        } else {
            without_positions(list, &hidden, keys)
        };
        out.push(SetElement {
            tag: o.tag.clone(),
            value: Value::Set(list),
        });
    }
    out
}
