//! Gives a string operator whose source compiler typed its result as the built-in CHAR domain the domains develop's
//! compiler writes for the same expression, so develop evaluates it on its planned path.
//!
//! Input: a [`StreamTree`] already in develop numbers and domains ([`super::to_develop`]). Output: the same tree; an
//! error for a built-in CHAR result this module has no develop compile for.
//! Source: 10.2 compiles `CONCAT(<CHAR column>, 'x')` to a built-in (floating, ISO-8859-1) CHAR result; develop
//! 682b080b2 compiles the same DDL to VARCHAR(`DB_MAX_VARCHAR_PRECISION`) in the column's codeset and collation on the
//! ARITH node and the REGU holding it, and a built-in VARCHAR result holder (workspace#540, fixture
//! basedb/partition-streams-102, T4 `part_char_const`). Kept as is, develop's `qdata_coerce_result_to_domain`
//! (query_opfunc.c, CBRD-27583) reaches its not-reached branch: an optdebug assert, a release fallback coerce.
//! 11.2, 11.3 and 11.4 already compile VARCHAR there (`part_char_const` source trees). ADR 0017.

use super::tree::{Node, NodeId, ReguValue, StreamTree};
use crate::VersionLine;
use crate::value::domain::MAX_VARCHAR_PRECISION;
use crate::value::{DbType, DecodeError, DecodeResult, Domain};

/// Engine `T_CONCAT`: 33 in every line.
const T_CONCAT: i32 = 33;

fn is_builtin_char(d: &[Domain]) -> bool {
    matches!(d, [x] if x.builtin && x.ty == DbType::Char)
}

/// The codeset and collation develop gives the result: those of the first operand with a declared string domain.
fn operand_domain(tree: &StreamTree, operands: [Option<NodeId>; 3]) -> Option<Domain> {
    operands
        .into_iter()
        .flatten()
        .find_map(|id| match &tree.nodes[id] {
            Node::Regu { domain: d, .. } => match d.as_slice() {
                [x] if !x.builtin && matches!(x.ty, DbType::Char | DbType::Varchar) => {
                    Some(x.clone())
                }
                _ => None,
            },
            _ => None,
        })
}

/// Rewrites every built-in CHAR result of a string operator. Children have larger ids than their parents (the parser
/// numbers a node before its children), so walking the ids down fixes a nested operator before the one using it.
pub fn map_char_results(tree: &mut StreamTree) -> DecodeResult<()> {
    for id in (0..tree.nodes.len()).rev() {
        let Node::Arith {
            domain,
            opcode,
            value,
            left,
            right,
            third,
            ..
        } = &tree.nodes[id]
        else {
            continue;
        };
        if !is_builtin_char(domain) {
            continue;
        }
        let (opcode, value) = (*opcode, *value);
        let column = (opcode == T_CONCAT)
            .then(|| operand_domain(tree, [*left, *right, *third]))
            .flatten()
            .ok_or_else(|| DecodeError {
                at: 0,
                what: format!(
                    "operator {opcode} with a built-in CHAR result, which develop does not plan (workspace#540)"
                ),
            })?;
        let result = Domain {
            precision: MAX_VARCHAR_PRECISION,
            codeset: column.codeset,
            collation_id: column.collation_id,
            ..Domain::of_type(DbType::Varchar)
        };
        if let Node::Arith { domain, .. } = &mut tree.nodes[id] {
            *domain = vec![result.clone()];
        }
        if let Some(v) = value
            && let Node::DbValue { tag, .. } = &mut tree.nodes[v]
        {
            *tag = Domain::builtin(VersionLine::Develop, DbType::Varchar);
        }
        // the REGU that holds this ARITH carries the same domain
        for n in &mut tree.nodes {
            if let Node::Regu {
                domain,
                value: ReguValue::Ptr(Some(p)),
                ..
            } = n
                && *p == id
                && is_builtin_char(domain)
            {
                *domain = vec![result.clone()];
            }
        }
    }
    Ok(())
}
