//! Gives every date/number language flag of a stream tree the loaded-locales parity the 11.4 and develop engines
//! check: the INTEGER third operand of a `T_TO_CHAR`/`T_TO_DATE`-family ARITH node.
//!
//! Input: a [`StreamTree`] whose opcodes are already the target line's numbers, and the develop number of each of
//! them. Output: the same tree, each such flag with the parity bits set.
//! Source: engine `language_support.h` `LANG_LOADED_LOCALES_PARITY` (release/11.4 and develop); since 11.4
//! (CBRD-25302, 73cc3fd40) the compiler sets it in every flag, and both 11.4 and develop
//! (`string_opfunc.c` `db_get_date_lang_id`, release/11.4 :13212) read a flag without it as a host variable and
//! refuse it ("Invalid data type"); workspace#540 (develop target), workspace#552 (11.4 target); ADR 0017.

use super::opcode;
use super::tree::{Node, ReguValue, StreamTree};
use crate::value::Value;

/// Engine `LANG_LOADED_LOCALES_PARITY` and its mask.
const LANG_PARITY: i32 = 0x75A5_C000;
const LANG_PARITY_MASK: i32 = 0xFFFF_F000_u32 as i32;

/// The flag with the parity. A source before 11.4 (10.2..11.3) stores the bare
/// `lang id << 2 | has_user_lang << 1 | has_user_format`, the same bits 11.4 and develop keep below the parity; an 11.4
/// flag already carries the parity and stays.
fn with_parity(v: i32) -> i32 {
    if v & LANG_PARITY_MASK == 0 {
        v | LANG_PARITY
    } else {
        v
    }
}

/// Sets the parity in the flag of every node whose develop opcode (`develop_opcode` of its stored one) takes one.
pub fn add_parity(tree: &mut StreamTree, develop_opcode: impl Fn(i32) -> i32) {
    let flags: Vec<_> = tree
        .nodes
        .iter()
        .filter_map(|n| match n {
            Node::Arith { opcode, third, .. }
                if opcode::takes_lang_flag(develop_opcode(*opcode)) =>
            {
                *third
            }
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
            *v = with_parity(*v);
        }
    }
}
