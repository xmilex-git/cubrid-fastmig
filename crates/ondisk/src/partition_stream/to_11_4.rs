//! Rewrites a parsed 10.2, 11.2 or 11.3 stream tree in 11.4 terms without evaluating anything: 10.2 opcodes
//! renumbered, every language flag given the parity 11.4 checks. Domains, constants and DB_TYPE words stay: 11.4 has
//! NCHAR and the source's built-in NUMERIC, and 10.2's built-in CHAR result of CONCAT evaluates on 11.4 as on 10.2
//! (develop's CBRD-27583 assert is not in 11.4). Values are re-encoded later, when [`super::write`] writes them in
//! the 11.4 layout (a 10.2 LZO1X string becomes LZ4).
//!
//! Input: a [`StreamTree`] parsed from a 10.2 stream or an 11.2/11.3 one (read with the 11.4 line, R8). Output: the
//! same tree, in place, ready to write as 11.4.
//! Source: R13 § 2 (workspace#523: 10.2 differs from 11.4 in its opcode and function code numbers only), ADR 0017
//! (the source compile is kept; only what the target cannot evaluate changes), workspace#552.

use super::opcode;
use super::tree::{Node, StreamTree};
use crate::VersionLine;
use crate::value::{DecodeError, DecodeResult};

/// Maps every node of a tree `from` (10.2 or 11.4 line) wrote to 11.4. Refuses a function 11.0 renumbered.
pub fn map_tree(from: VersionLine, tree: &mut StreamTree) -> DecodeResult<()> {
    for node in &mut tree.nodes {
        match node {
            Node::Arith { opcode: o, .. } => *o = opcode::to_11_4(from, *o),
            Node::Function { ftype, .. } if !opcode::function_kept(from, *ftype) => {
                return Err(DecodeError {
                    at: 0,
                    what: format!(
                        "function code {ftype} (F_BENCHMARK or later in {from:?}) in a partition expression"
                    ),
                });
            }
            _ => {}
        }
    }
    super::lang_flag::add_parity(tree, |o| opcode::to_develop(VersionLine::V11_4, o));
    Ok(())
}
