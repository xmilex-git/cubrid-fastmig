//! `OPERATOR_TYPE` numbers per release line: which opcodes carry a predicate in an `ARITH_TYPE` node, and the 11.4 →
//! develop renumbering.
//!
//! Input: an opcode as stored in an `ARITH_TYPE` node. Output: whether the node has a `pred` offset; the develop
//! number of an 11.4 opcode.
//! Source: engine `src/storage/storage_common.h` `OPERATOR_TYPE` (no explicit `=`; release/11.4 @46212adcd 199
//! names, develop @5b1867174 and @682b080b2 206 names, every 11.4 name found at its mapped develop number),
//! `xts_process_arith_type` (pred for T_CASE, T_DECODE, T_PREDICATE, T_IF); R7 § 1.1 (workspace#487).

use crate::VersionLine;

/// Engine `T_IF`, `T_CASE`, `T_DECODE`: the same number in both lines.
const T_IF: i32 = 18;
const T_CASE: i32 = 114;
const T_DECODE: i32 = 136;

/// Engine `T_PREDICATE`.
fn t_predicate(line: VersionLine) -> i32 {
    match line {
        VersionLine::V11_4 => 161,
        VersionLine::Develop => 165,
    }
}

/// Number of `OPERATOR_TYPE` names, so an opcode at or above it is not one.
pub fn opcode_count(line: VersionLine) -> i32 {
    match line {
        VersionLine::V11_4 => 199,
        VersionLine::Develop => 206,
    }
}

/// Whether an `ARITH_TYPE` with this opcode writes a `pred` offset (`xts_process_arith_type`).
pub fn has_pred(line: VersionLine, opcode: i32) -> bool {
    opcode == T_IF || opcode == T_CASE || opcode == T_DECODE || opcode == t_predicate(line)
}

/// The develop opcode of an 11.4 opcode: 0..158 unchanged, 159..183 +4 (develop inserts the four
/// `T_ESTIMATED_*`), 184..198 +6 (and `T_UUID`, `T_UUID_FORMAT`).
pub fn to_develop(opcode: i32) -> i32 {
    opcode
        + if opcode >= 184 {
            6
        } else if opcode >= 159 {
            4
        } else {
            0
        }
}
