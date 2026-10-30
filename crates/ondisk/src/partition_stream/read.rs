//! Parses a partition expression stream into a [`StreamTree`], following child offsets from the root `FUNC_PRED`
//! and keeping a node reached through two offsets equal to one node.
//!
//! Input: the stream bytes (`[int header_size][int body_size]`, the body from the 8-aligned offset after the header)
//! and the release line that wrote it. Output: the tree, or the offset and reason of the first thing it cannot read.
//! Source: engine `stream_to_xasl.c:stx_map_stream_to_func_pred` (root offset), `stx_build_regu_variable`,
//! `stx_build_arith_type` (develop @682b080b2 `converts`/`fails` words, see [`super::write`]) and the writer field
//! order in `xasl_to_stream.c` (see [`super::tree`]); offsets are absolute in the stream, 0 is a null child
//! (`xts_save_*` returns 0 for NULL); `or_unpack_value` aligns an inline value to 8; R7 § 2.1-2.2.

use std::collections::HashMap;

use super::opcode;
use super::tree::{Node, NodeId, PredExpr, ReguValue, StreamTree, pred_type, regu_type};
use crate::VersionLine;
use crate::value::{
    DecodeError, DecodeResult, Domain, OrReader, Value, decode_packed_domain, decode_tagged,
};

/// Engine `XASL_STREAM_ALIGN_UNIT` (`sizeof (double)`): every node starts on this boundary.
pub const ALIGN: usize = 8;

/// The kind of node an offset was first read as; a second offset to it must expect the same kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Cache,
    Regu,
    DbValue,
    Arith,
    Function,
    ReguList,
    Pred,
}

struct Parser<'a> {
    line: VersionLine,
    stream: &'a [u8],
    nodes: Vec<Node>,
    by_offset: HashMap<usize, (NodeId, Kind)>,
}

/// Parses `stream`, written by `line`.
pub fn parse(line: VersionLine, stream: &[u8]) -> DecodeResult<StreamTree> {
    let mut p = Parser {
        line,
        stream,
        nodes: Vec::new(),
        by_offset: HashMap::new(),
    };
    let mut r = OrReader::new(stream);
    let header_size = r.i32()?;
    if header_size != 0 {
        return Err(r.error(format!("partition stream header size {header_size}, not 0")));
    }
    let body_size = usize::try_from(r.i32()?).map_err(|_| r.error("negative body size"))?;
    let root_off = 8usize.next_multiple_of(ALIGN);
    if root_off + body_size > stream.len() {
        return Err(r.error(format!(
            "body of {body_size} B at {root_off} runs past the {} B stream",
            stream.len()
        )));
    }
    let root = p.func_pred(root_off)?;
    Ok(StreamTree {
        nodes: p.nodes,
        root,
    })
}

impl<'a> Parser<'a> {
    /// A reader at `off`; its error positions are relative to `off` until [`Self::rebase`] fixes them.
    fn reader(&self, off: usize) -> DecodeResult<OrReader<'a>> {
        if !off.is_multiple_of(ALIGN) || off >= self.stream.len() {
            return Err(DecodeError {
                at: off,
                what: format!("node offset {off} not aligned or past the stream"),
            });
        }
        Ok(OrReader::new(&self.stream[off..]))
    }

    fn rebase<T>(off: usize, r: DecodeResult<T>) -> DecodeResult<T> {
        r.map_err(|e| DecodeError {
            at: off + e.at,
            what: e.what,
        })
    }

    /// The node at a child offset: `None` for 0, the existing node for an offset already read, else `read` it.
    fn child(
        &mut self,
        off: i32,
        kind: Kind,
        read: fn(&mut Self, usize) -> DecodeResult<Node>,
    ) -> DecodeResult<Option<NodeId>> {
        if off == 0 {
            return Ok(None);
        }
        let off = usize::try_from(off).map_err(|_| DecodeError {
            at: 0,
            what: format!("negative child offset {off}"),
        })?;
        if let Some(&(id, k)) = self.by_offset.get(&off) {
            if k != kind {
                return Err(DecodeError {
                    at: off,
                    what: format!("offset read as {k:?} is also referenced as {kind:?}"),
                });
            }
            return Ok(Some(id));
        }
        // reserve the id first so a cycle resolves to this node instead of recursing forever
        let id = self.nodes.len();
        self.nodes.push(Node::CacheAttrinfo);
        self.by_offset.insert(off, (id, kind));
        self.nodes[id] = read(self, off)?;
        Ok(Some(id))
    }

    fn func_pred(&mut self, off: usize) -> DecodeResult<NodeId> {
        let mut r = self.reader(off)?;
        let (regu, cache) = Self::rebase(off, (|| Ok((r.i32()?, r.i32()?)))())?;
        let id = self.nodes.len();
        self.nodes.push(Node::CacheAttrinfo);
        let regu = self.child(regu, Kind::Regu, Self::regu)?;
        let cache = self.child(cache, Kind::Cache, Self::cache)?;
        self.nodes[id] = Node::FuncPred { regu, cache };
        Ok(id)
    }

    fn cache(&mut self, off: usize) -> DecodeResult<Node> {
        let mut r = self.reader(off)?;
        match Self::rebase(off, r.i32())? {
            0 => Ok(Node::CacheAttrinfo),
            x => Err(DecodeError {
                at: off,
                what: format!("cache_attrinfo word {x}, not 0"),
            }),
        }
    }

    /// `or_get_value` of a tagged value.
    fn tagged(off: usize, line: VersionLine, r: &mut OrReader) -> DecodeResult<(Domain, Value)> {
        Self::rebase(off, decode_tagged(line, r, None))
    }

    fn db_value(&mut self, off: usize) -> DecodeResult<Node> {
        let mut r = self.reader(off)?;
        let (tag, value) = Self::tagged(off, self.line, &mut r)?;
        Ok(Node::DbValue { tag, value })
    }

    fn regu(&mut self, off: usize) -> DecodeResult<Node> {
        let mut r = self.reader(off)?;
        let line = self.line;
        let (domain, conversion, ty, flags, vfetch_to, xasl) = Self::rebase(
            off,
            (|| {
                Ok((
                    decode_packed_domain(line, &mut r)?,
                    // develop: `xts_regu_converts`, then the original domain and conv_mode when set
                    match line {
                        VersionLine::V10_2 | VersionLine::V11_4 => None,
                        VersionLine::Develop => match r.i32()? {
                            0 => None,
                            _ => Some((decode_packed_domain(line, &mut r)?, r.i32()?)),
                        },
                    },
                    r.i32()?,
                    r.i32()?,
                    r.i32()?,
                    r.i32()?,
                ))
            })(),
        )?;
        if xasl != 0 {
            return Err(DecodeError {
                at: off,
                what: "REGU_VARIABLE with a subquery (xasl offset) in a partition expression"
                    .into(),
            });
        }
        let vfetch_to = self.child(vfetch_to, Kind::DbValue, Self::db_value)?;
        let value = self.regu_value(ty, &mut r, off)?;
        Ok(Node::Regu {
            domain,
            conversion,
            ty,
            flags,
            vfetch_to,
            value,
        })
    }

    /// `xts_pack_regu_variable_value`: the type-dependent part, read from `r` (a reader at node offset `off`).
    fn regu_value(&mut self, ty: i32, r: &mut OrReader, off: usize) -> DecodeResult<ReguValue> {
        use regu_type as T;
        let int = |r: &mut OrReader| Self::rebase(off, r.i32());
        Ok(match ty {
            T::DBVAL => {
                // or_pack_value aligns the value to MAX_ALIGNMENT; a node starts aligned, so the cursor does too
                let pad = r.pos().next_multiple_of(ALIGN) - r.pos();
                Self::rebase(off, r.skip(pad))?;
                let (tag, value) = Self::tagged(off, self.line, r)?;
                ReguValue::DbVal { tag, value }
            }
            T::CONSTANT | T::ORDERBY_NUM => {
                ReguValue::Ptr(self.child(int(r)?, Kind::DbValue, Self::db_value)?)
            }
            T::INARITH | T::OUTARITH => {
                ReguValue::Ptr(self.child(int(r)?, Kind::Arith, Self::arith)?)
            }
            T::FUNC => ReguValue::Ptr(self.child(int(r)?, Kind::Function, Self::function)?),
            T::ATTR_ID | T::CLASS_ATTR_ID | T::SHARED_ATTR_ID => {
                let id = int(r)?;
                let db_type = int(r)?;
                let cache = self.child(int(r)?, Kind::Cache, Self::cache)?;
                ReguValue::Attr { id, db_type, cache }
            }
            T::POS_VALUE => ReguValue::PosValue(int(r)?),
            T::OID | T::CLASSOID => ReguValue::Empty,
            T::REGU_VAR_LIST => {
                ReguValue::VarList(self.child(int(r)?, Kind::ReguList, Self::regu_list)?)
            }
            T::REGUVAL_LIST => {
                let count = int(r)?;
                let mut items = Vec::new();
                for _ in 0..count.max(0) {
                    let item_ty = int(r)?;
                    if !matches!(item_ty, T::DBVAL | T::INARITH | T::POS_VALUE) {
                        return Err(DecodeError {
                            at: off,
                            what: format!("regu value list item of type {item_ty}"),
                        });
                    }
                    items.push((item_ty, self.regu_value(item_ty, r, off)?));
                }
                ReguValue::ValList(items)
            }
            // TYPE_POSITION, TYPE_LIST_ID, TYPE_SP: list files and stored procedures, not allowed in a partition
            // expression
            t => {
                return Err(DecodeError {
                    at: off,
                    what: format!(
                        "REGU_VARIABLE type {t} is not expected in a partition expression"
                    ),
                });
            }
        })
    }

    fn arith(&mut self, off: usize) -> DecodeResult<Node> {
        let mut r = self.reader(off)?;
        let (domain, value, opcode, left, right, third, misc_operand) = Self::rebase(
            off,
            (|| {
                Ok((
                    decode_packed_domain(self.line, &mut r)?,
                    r.i32()?,
                    r.i32()?,
                    r.i32()?,
                    r.i32()?,
                    r.i32()?,
                    r.i32()?,
                ))
            })(),
        )?;
        if !(0..opcode::opcode_count(self.line)).contains(&opcode) {
            return Err(DecodeError {
                at: off,
                what: format!("opcode {opcode} out of range for {:?}", self.line),
            });
        }
        // develop: `arith->fails` after misc_operand
        let fails = match self.line {
            VersionLine::V10_2 | VersionLine::V11_4 => 0,
            VersionLine::Develop => Self::rebase(off, r.i32())?,
        };
        let pred_off = if opcode::has_pred(self.line, opcode) {
            Some(Self::rebase(off, r.i32())?)
        } else {
            None
        };
        let value = self.child(value, Kind::DbValue, Self::db_value)?;
        let left = self.child(left, Kind::Regu, Self::regu)?;
        let right = self.child(right, Kind::Regu, Self::regu)?;
        let third = self.child(third, Kind::Regu, Self::regu)?;
        let pred = match pred_off {
            Some(p) => Some(self.child(p, Kind::Pred, Self::pred_expr)?),
            None => None,
        };
        Ok(Node::Arith {
            domain,
            value,
            opcode,
            left,
            right,
            third,
            misc_operand,
            fails,
            pred,
        })
    }

    fn function(&mut self, off: usize) -> DecodeResult<Node> {
        let mut r = self.reader(off)?;
        let (value, ftype, operands) =
            Self::rebase(off, (|| Ok((r.i32()?, r.i32()?, r.i32()?)))())?;
        Ok(Node::Function {
            value: self.child(value, Kind::DbValue, Self::db_value)?,
            ftype,
            operands: self.child(operands, Kind::ReguList, Self::regu_list)?,
        })
    }

    fn regu_list(&mut self, off: usize) -> DecodeResult<Node> {
        let mut r = self.reader(off)?;
        let count = Self::rebase(off, r.i32())?;
        let mut offs = Vec::new();
        for _ in 0..count.max(0) {
            offs.push(Self::rebase(off, r.i32())?);
        }
        let mut items = Vec::new();
        for o in offs {
            items.push(self.child(o, Kind::Regu, Self::regu)?);
        }
        Ok(Node::ReguList(items))
    }

    fn pred_expr(&mut self, off: usize) -> DecodeResult<Node> {
        use pred_type as P;
        let mut r = self.reader(off)?;
        let mut int = || Self::rebase(off, r.i32());
        let ty = int()?;
        let p = match ty {
            P::T_PRED => {
                let mut raw_links = Vec::new();
                let rhs_type = loop {
                    let lhs = int()?;
                    let bool_op = int()?;
                    let rhs_type = int()?;
                    raw_links.push((lhs, bool_op));
                    if rhs_type != P::T_PRED {
                        break rhs_type;
                    }
                };
                let rhs = int()?;
                let mut links = Vec::new();
                for (lhs, op) in raw_links {
                    links.push((self.child(lhs, Kind::Pred, Self::pred_expr)?, op));
                }
                let rhs = self.child(rhs, Kind::Pred, Self::pred_expr)?;
                let written = rhs.map(|id| match &self.nodes[id] {
                    Node::Pred(p) => p.type_word(),
                    _ => 0,
                });
                if written.is_some_and(|w| w != rhs_type) {
                    return Err(DecodeError {
                        at: off,
                        what: format!(
                            "chain end type {rhs_type} differs from its node {written:?}"
                        ),
                    });
                }
                PredExpr::Pred { links, rhs }
            }
            P::T_EVAL_TERM => {
                let et = int()?;
                match et {
                    P::T_COMP_EVAL_TERM => {
                        let (lhs, rhs, rel_op, db_type) = (int()?, int()?, int()?, int()?);
                        PredExpr::Comp {
                            lhs: self.child(lhs, Kind::Regu, Self::regu)?,
                            rhs: self.child(rhs, Kind::Regu, Self::regu)?,
                            rel_op,
                            db_type,
                        }
                    }
                    P::T_ALSM_EVAL_TERM => {
                        let (elem, elemset) = (int()?, int()?);
                        let (eq_flag, rel_op, item_type) = (int()?, int()?, int()?);
                        PredExpr::Alsm {
                            elem: self.child(elem, Kind::Regu, Self::regu)?,
                            elemset: self.child(elemset, Kind::Regu, Self::regu)?,
                            eq_flag,
                            rel_op,
                            item_type,
                        }
                    }
                    P::T_LIKE_EVAL_TERM | P::T_RLIKE_EVAL_TERM => {
                        let (a, b, c) = (int()?, int()?, int()?);
                        let a = self.child(a, Kind::Regu, Self::regu)?;
                        let b = self.child(b, Kind::Regu, Self::regu)?;
                        let c = self.child(c, Kind::Regu, Self::regu)?;
                        if et == P::T_LIKE_EVAL_TERM {
                            PredExpr::Like {
                                src: a,
                                pattern: b,
                                esc_char: c,
                            }
                        } else {
                            PredExpr::Rlike {
                                src: a,
                                pattern: b,
                                case_sensitive: c,
                            }
                        }
                    }
                    x => {
                        return Err(DecodeError {
                            at: off,
                            what: format!("eval term type {x}"),
                        });
                    }
                }
            }
            P::T_NOT_TERM => {
                let x = int()?;
                PredExpr::Not(self.child(x, Kind::Pred, Self::pred_expr)?)
            }
            x => {
                return Err(DecodeError {
                    at: off,
                    what: format!("pred expr type {x}"),
                });
            }
        };
        Ok(Node::Pred(p))
    }
}
