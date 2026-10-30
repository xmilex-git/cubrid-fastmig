//! `OPERATOR_TYPE` and `FUNC_CODE` numbers per release line: which opcodes carry a predicate in an `ARITH_TYPE` node,
//! the renumbering of a source opcode to develop, and the function codes a 10.2 stream may hold.
//!
//! Input: an opcode as stored in an `ARITH_TYPE` node, a function code as stored in a `FUNCTION_TYPE` node. Output:
//! whether the node has a `pred` offset; the develop number of a source opcode; whether a function code is kept.
//! Source: engine `src/storage/storage_common.h` `OPERATOR_TYPE` (no explicit `=`; release/10.2 @416bb667c 198 names,
//! release/11.4 @46212adcd 199 names, develop @5b1867174 and @682b080b2 206 names, every source name found at its
//! mapped develop number), `FUNC_TYPE` (release/10.2 `F_BENCHMARK` 963), `xts_process_arith_type` (pred for T_CASE,
//! T_DECODE, T_PREDICATE, T_IF); R7 § 1.1 (workspace#487), R13 § 2-3 (workspace#523).

use crate::VersionLine;

/// Engine `T_IF`: the same number in every line.
const T_IF: i32 = 18;

/// Engine `T_CASE`: 10.2 lacks `T_CAST_WRAP` before it.
fn t_case(line: VersionLine) -> i32 {
    match line {
        VersionLine::V10_2 => 113,
        VersionLine::V11_4 | VersionLine::Develop => 114,
    }
}

/// Engine `T_DECODE`.
fn t_decode(line: VersionLine) -> i32 {
    match line {
        VersionLine::V10_2 => 135,
        VersionLine::V11_4 | VersionLine::Develop => 136,
    }
}

/// Engine `T_PREDICATE`.
fn t_predicate(line: VersionLine) -> i32 {
    match line {
        VersionLine::V10_2 => 160,
        VersionLine::V11_4 => 161,
        VersionLine::Develop => 165,
    }
}

/// Number of `OPERATOR_TYPE` names, so an opcode at or above it is not one.
pub fn opcode_count(line: VersionLine) -> i32 {
    match line {
        VersionLine::V10_2 => 198,
        VersionLine::V11_4 => 199,
        VersionLine::Develop => 206,
    }
}

/// Whether an `ARITH_TYPE` with this opcode writes a `pred` offset (`xts_process_arith_type`). Judged with the
/// writing line's numbers: 10.2 `T_EXTRACT` (114) and `T_RAND` (136) are 11.4 `T_CASE` and `T_DECODE` (R13 § 3.3).
pub fn has_pred(line: VersionLine, opcode: i32) -> bool {
    opcode == T_IF
        || opcode == t_case(line)
        || opcode == t_decode(line)
        || opcode == t_predicate(line)
}

/// The 11.4 opcode of a 10.2 one: 11.0 inserts `T_CAST_WRAP` at 113, so 113..197 move +1.
fn from_10_2_to_11_4(opcode: i32) -> i32 {
    opcode + i32::from(opcode >= 113)
}

/// The develop opcode of an 11.4 one: 0..158 unchanged, 159..183 +4 (develop inserts the four `T_ESTIMATED_*`),
/// 184..198 +6 (and `T_UUID`, `T_UUID_FORMAT`).
fn from_11_4_to_develop(opcode: i32) -> i32 {
    opcode
        + if opcode >= 184 {
            6
        } else if opcode >= 159 {
            4
        } else {
            0
        }
}

/// The develop opcode of an opcode `from` wrote. 10.2 goes through 11.4, so a develop change to
/// [`from_11_4_to_develop`] reaches both lines (R13 § 4.3, workspace#528).
pub fn to_develop(from: VersionLine, opcode: i32) -> i32 {
    match from {
        VersionLine::V10_2 => from_11_4_to_develop(from_10_2_to_11_4(opcode)),
        VersionLine::V11_4 => from_11_4_to_develop(opcode),
        VersionLine::Develop => opcode,
    }
}

/// The 11.4 opcode of an opcode `from` (10.2 or 11.4) wrote (workspace#552).
pub fn to_11_4(from: VersionLine, opcode: i32) -> i32 {
    match from {
        VersionLine::V10_2 => from_10_2_to_11_4(opcode),
        VersionLine::V11_4 => opcode,
        VersionLine::Develop => {
            unreachable!("develop to 11.4 is a downgrade the transcoder refuses first")
        }
    }
}

/// Engine 10.2 `F_BENCHMARK`: the first function code 11.0 moves (it inserts the five `F_REGEXP_*` at 963). Below it
/// the codes are the same in 10.2, 11.4 and develop, and every function the 10.2 partition check allows is there.
const F_BENCHMARK_10_2: i32 = 963;

/// Whether a `FUNCTION_TYPE` code `from` wrote stays as is in develop. A 10.2 code from `F_BENCHMARK` on reaches a
/// partition expression only nested where the 10.2 check does not look; it is refused, not renumbered (R13 § 3.4,
/// workspace#540).
pub fn function_kept(from: VersionLine, ftype: i32) -> bool {
    from != VersionLine::V10_2 || ftype < F_BENCHMARK_10_2
}

/// Whether a develop opcode takes the date/number language flag (an INTEGER constant the compiler builds with
/// `lang_set_flag_from_lang`) as its third operand: `T_TO_CHAR`, `T_TO_DATE`, `T_TO_TIME`, `T_TO_TIMESTAMP`,
/// `T_TO_DATETIME` (103..107 in every line), `T_TO_DATETIME_TZ`, `T_TO_TIMESTAMP_TZ` (develop 196, 197). Engine
/// `fetch.c:fetch_peek_arith` passes `peek_third` to `db_to_char`, `db_to_date`, `db_to_time`, `db_to_timestamp`,
/// `db_to_datetime` (develop @5b1867174).
pub fn takes_lang_flag(develop_opcode: i32) -> bool {
    matches!(develop_opcode, 103..=107 | 196 | 197)
}
