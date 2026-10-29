//! Serializes a [`StreamTree`] in one release line's layout, placing nodes where the engine's writer would: each
//! node reserves its engine `xts_sizeof_*` size (8-aligned) before its children, children follow in field order,
//! a shared node is written once, and a regu list's elements come before its int array.
//!
//! Input: the tree and the line to write. Output: the stream bytes (`[int 0][int body_size]` + body), every
//! reserved byte the engine leaves unwritten set to 0.
//! Source: engine `xasl_to_stream.c` release/11.4 @46212adcd and develop @682b080b2 (the T1 develop install). Develop
//! adds, in a filter/function/partition stream (`xts_Index_stream`), a `converts` word after a REGU_VARIABLE's
//! domain (then the original domain and conv_mode when set) and an ARITH `fails` word after `misc_operand`
//! (52193efa9 CBRD-27583, after R7's @5b1867174); the comparison and decision words it adds are for query streams
//! only. Functions: `xts_map_func_pred_to_stream`, `xts_reserve_location_in_stream`,
//! `xts_save_*`/`xts_process_*`, the sizes of `xts_sizeof_func_pred`, `xts_sizeof_cache_attrinfo`,
//! `xts_sizeof_regu_variable`, `xts_get_regu_variable_value_size` (an inline DB_VALUE reserves
//! `OR_VALUE_ALIGNED_SIZE` = `or_db_value_size` + 8), `xts_sizeof_arith_type` (reserves an unwritten `next`),
//! `xts_sizeof_function_type`, `xts_sizeof_pred_expr`; `object_representation.c:or_pack_value` (an inline DB_VALUE
//! starts at the next 8-byte boundary) and `or_put_value` (domain with class OIDs, data, padding to 4).

use std::collections::HashMap;

use super::read::ALIGN;
use super::tree::{Node, NodeId, PredExpr, ReguValue, StreamTree, pred_type};
use crate::VersionLine;
use crate::value::{Domain, OrWriter, Value, encode_data, encode_packed_domain};

const INT: usize = 4;
/// Engine `MAX_ALIGNMENT` (`DOUBLE_ALIGNMENT`), added to an inline DB_VALUE's reserved size.
const MAX_ALIGNMENT: usize = 8;

/// Writes `tree` in `line`'s layout.
pub fn serialize(line: VersionLine, tree: &StreamTree) -> Vec<u8> {
    serialize_with_mask(line, tree).0
}

/// [`serialize`], plus per byte whether a node wrote it (`false`: reserved by an `xts_sizeof_*` or the alignment but
/// left unwritten, where the engine's stream holds whatever its buffer held).
pub fn serialize_with_mask(line: VersionLine, tree: &StreamTree) -> (Vec<u8>, Vec<bool>) {
    let mut w = Writer {
        line,
        tree,
        buf: Vec::new(),
        written: Vec::new(),
        visited: HashMap::new(),
    };
    let header = w.reserve(2 * INT);
    w.written[..2 * INT].fill(true);
    w.save(Some(tree.root));
    let body = (w.buf.len() - header - 2 * INT) as i32;
    w.buf[4..8].copy_from_slice(&body.to_be_bytes());
    (w.buf, w.written)
}

/// Engine `or_put_value (buf, v, 0, 1, 1)`: the tag with class OIDs (NULL bit for a NULL value), the data, padding to 4.
pub fn encode_db_value(line: VersionLine, w: &mut OrWriter, tag: &Domain, v: &Value) {
    let start = w.len();
    encode_packed_domain(line, w, std::slice::from_ref(tag), true, v.is_null());
    if !v.is_null() {
        encode_data(line, w, tag, v);
    }
    w.zeros((4 - ((w.len() - start) & 3)) & 3);
}

struct Writer<'a> {
    line: VersionLine,
    tree: &'a StreamTree,
    buf: Vec<u8>,
    written: Vec<bool>,
    visited: HashMap<NodeId, i32>,
}

impl<'a> Writer<'a> {
    /// `xts_reserve_location_in_stream`: `size` rounded up to the alignment, zero-filled, at the free offset.
    fn reserve(&mut self, size: usize) -> usize {
        let off = self.buf.len();
        self.buf.resize(off + size.next_multiple_of(ALIGN), 0);
        self.written.resize(self.buf.len(), false);
        off
    }

    fn put(&mut self, off: usize, size: usize, node: OrWriter) {
        assert!(
            node.len() <= size,
            "node at {off} wrote {} B into {size} B",
            node.len()
        );
        self.buf[off..off + node.len()].copy_from_slice(&node.buf);
        self.written[off..off + node.len()].fill(true);
    }

    fn domain_len(&self, chain: &[Domain], class_oids: bool) -> usize {
        let mut w = OrWriter::new();
        encode_packed_domain(self.line, &mut w, chain, class_oids, false);
        w.len()
    }

    fn db_value_len(&self, tag: &Domain, v: &Value) -> usize {
        let mut w = OrWriter::new();
        encode_db_value(self.line, &mut w, tag, v);
        w.len()
    }

    /// `xts_get_regu_variable_value_size`.
    fn regu_value_size(&self, v: &ReguValue) -> usize {
        match v {
            ReguValue::DbVal { tag, value } => self.db_value_len(tag, value) + MAX_ALIGNMENT,
            ReguValue::Ptr(_) | ReguValue::PosValue(_) => INT,
            ReguValue::Attr { .. } => 3 * INT,
            ReguValue::Empty => 0,
            // xts_sizeof_regu_variable_list: count, then per element its type word and value size
            ReguValue::VarList(list) => {
                let items = match list.map(|id| &self.tree.nodes[id]) {
                    Some(Node::ReguList(items)) => items.as_slice(),
                    _ => &[],
                };
                INT + items
                    .iter()
                    .map(|i| match i.map(|id| &self.tree.nodes[id]) {
                        Some(Node::Regu { value, .. }) => INT + self.regu_value_size(value),
                        _ => INT,
                    })
                    .sum::<usize>()
            }
            ReguValue::ValList(items) => {
                INT + items
                    .iter()
                    .map(|(_, v)| INT + self.regu_value_size(v))
                    .sum::<usize>()
            }
        }
    }

    /// develop's extra word (REGU `converts`, ARITH `fails`, CBRD-27583); none in 11.4.
    fn develop_word(&self) -> usize {
        match self.line {
            VersionLine::V11_4 => 0,
            VersionLine::Develop => INT,
        }
    }

    /// develop `xts_sizeof_regu_variable`: the `converts` word, plus the original domain and conv_mode when set.
    fn conversion_size(&self, c: &Option<(Vec<Domain>, i32)>) -> usize {
        self.develop_word()
            + c.as_ref()
                .map_or(0, |(d, _)| self.domain_len(d, false) + INT)
    }

    /// `xts_sizeof_*` of the node.
    fn size_of(&self, node: &Node) -> usize {
        match node {
            Node::FuncPred { .. } => 2 * INT,
            Node::CacheAttrinfo => INT,
            Node::Regu {
                domain,
                conversion,
                value,
                ..
            } => {
                self.domain_len(domain, false)
                    + 4 * INT
                    + self.regu_value_size(value)
                    + self.conversion_size(conversion)
            }
            Node::DbValue { tag, value } => self.db_value_len(tag, value),
            // next (never written), value, opcode, left, right, third, misc_operand, pred when present
            Node::Arith { domain, pred, .. } => {
                7 * INT
                    + self.develop_word()
                    + pred.map_or(0, |_| INT)
                    + self.domain_len(domain, false)
            }
            Node::Function { .. } => 3 * INT,
            Node::ReguList(items) => INT * (items.len() + 1),
            Node::Pred(p) => {
                INT + match p {
                    PredExpr::Pred { links, .. } => 3 * INT * links.len() + INT,
                    PredExpr::Comp { .. } => INT + 4 * INT,
                    PredExpr::Alsm { .. } => INT + 5 * INT,
                    PredExpr::Like { .. } | PredExpr::Rlike { .. } => INT + 3 * INT,
                    PredExpr::Not(_) => INT,
                }
            }
        }
    }

    /// `xts_save_*`: the node's offset, writing it first if not yet written; 0 for no node.
    fn save(&mut self, id: Option<NodeId>) -> i32 {
        let Some(id) = id else {
            return 0;
        };
        if let Some(&off) = self.visited.get(&id) {
            return off;
        }
        let tree: &'a StreamTree = self.tree;
        let node = &tree.nodes[id];
        if let Node::ReguList(items) = node {
            // xts_save_regu_variable_list: the elements first, then the array, marked visited last
            let offs: Vec<i32> = items.iter().map(|i| self.save(*i)).collect();
            let size = self.size_of(node);
            let off = self.reserve(size);
            let mut w = OrWriter::new();
            w.i32(offs.len() as i32);
            offs.iter().for_each(|o| w.i32(*o));
            self.put(off, size, w);
            self.visited.insert(id, off as i32);
            return off as i32;
        }
        let size = self.size_of(node);
        let off = self.reserve(size);
        self.visited.insert(id, off as i32);
        let mut w = OrWriter::new();
        self.process(node, &mut w);
        self.put(off, size, w);
        off as i32
    }

    /// `xts_process_*`: the node's bytes, saving children in the order the engine does.
    fn process(&mut self, node: &Node, w: &mut OrWriter) {
        match node {
            Node::FuncPred { regu, cache } => {
                let r = self.save(*regu);
                w.i32(r);
                let c = self.save(*cache);
                w.i32(c);
            }
            Node::CacheAttrinfo => w.i32(0),
            Node::Regu {
                domain,
                conversion,
                ty,
                flags,
                vfetch_to,
                value,
            } => {
                encode_packed_domain(self.line, w, domain, false, false);
                match (self.line, conversion) {
                    (VersionLine::Develop, None) => w.i32(0),
                    (VersionLine::Develop, Some((d, mode))) => {
                        w.i32(1);
                        encode_packed_domain(self.line, w, d, false, false);
                        w.i32(*mode);
                    }
                    (VersionLine::V11_4, None) => {}
                    (VersionLine::V11_4, Some(_)) => panic!("11.4 has no REGU conversion"),
                }
                w.i32(*ty);
                w.i32(*flags);
                let v = self.save(*vfetch_to);
                w.i32(v);
                w.i32(0); // xasl: no subquery
                self.regu_value(value, w);
            }
            Node::DbValue { tag, value } => encode_db_value(self.line, w, tag, value),
            Node::Arith {
                domain,
                value,
                opcode,
                left,
                right,
                third,
                misc_operand,
                fails,
                pred,
            } => {
                encode_packed_domain(self.line, w, domain, false, false);
                let v = self.save(*value);
                w.i32(v);
                w.i32(*opcode);
                for c in [*left, *right, *third] {
                    let o = self.save(c);
                    w.i32(o);
                }
                w.i32(*misc_operand);
                match self.line {
                    VersionLine::Develop => w.i32(*fails),
                    VersionLine::V11_4 => assert_eq!(*fails, 0, "11.4 has no ARITH fails"),
                }
                if let Some(p) = pred {
                    let o = self.save(*p);
                    w.i32(o);
                }
            }
            Node::Function {
                value,
                ftype,
                operands,
            } => {
                let v = self.save(*value);
                w.i32(v);
                w.i32(*ftype);
                let o = self.save(*operands);
                w.i32(o);
            }
            Node::ReguList(_) => unreachable!("regu lists are written by save"),
            Node::Pred(p) => self.pred(p, w),
        }
    }

    fn regu_value(&mut self, v: &ReguValue, w: &mut OrWriter) {
        match v {
            ReguValue::DbVal { tag, value } => {
                // or_pack_value: PTR_ALIGN to MAX_ALIGNMENT; the node's bytes start at an aligned offset
                w.zeros(w.len().next_multiple_of(MAX_ALIGNMENT) - w.len());
                encode_db_value(self.line, w, tag, value);
            }
            ReguValue::Ptr(p) | ReguValue::VarList(p) => {
                let o = self.save(*p);
                w.i32(o);
            }
            ReguValue::Attr { id, db_type, cache } => {
                w.i32(*id);
                w.i32(*db_type);
                let c = self.save(*cache);
                w.i32(c);
            }
            ReguValue::PosValue(p) => w.i32(*p),
            ReguValue::Empty => {}
            ReguValue::ValList(items) => {
                w.i32(items.len() as i32);
                for (ty, item) in items {
                    w.i32(*ty);
                    self.regu_value(item, w);
                }
            }
        }
    }

    fn pred(&mut self, p: &PredExpr, w: &mut OrWriter) {
        use pred_type as P;
        w.i32(p.type_word());
        let children = |me: &mut Self, w: &mut OrWriter, ids: &[Option<NodeId>]| {
            for c in ids {
                let o = me.save(*c);
                w.i32(o);
            }
        };
        match p {
            PredExpr::Pred { links, rhs } => {
                let end_type = match rhs.map(|id| &self.tree.nodes[id]) {
                    Some(Node::Pred(r)) => r.type_word(),
                    _ => 0,
                };
                for (i, (lhs, bool_op)) in links.iter().enumerate() {
                    let o = self.save(*lhs);
                    w.i32(o);
                    w.i32(*bool_op);
                    w.i32(if i + 1 < links.len() {
                        P::T_PRED
                    } else {
                        end_type
                    });
                }
                children(self, w, &[*rhs]);
            }
            PredExpr::Comp {
                lhs,
                rhs,
                rel_op,
                db_type,
            } => {
                w.i32(P::T_COMP_EVAL_TERM);
                children(self, w, &[*lhs, *rhs]);
                w.i32(*rel_op);
                w.i32(*db_type);
            }
            PredExpr::Alsm {
                elem,
                elemset,
                eq_flag,
                rel_op,
                item_type,
            } => {
                w.i32(P::T_ALSM_EVAL_TERM);
                children(self, w, &[*elem, *elemset]);
                w.i32(*eq_flag);
                w.i32(*rel_op);
                w.i32(*item_type);
            }
            PredExpr::Like {
                src,
                pattern,
                esc_char,
            } => {
                w.i32(P::T_LIKE_EVAL_TERM);
                children(self, w, &[*src, *pattern, *esc_char]);
            }
            PredExpr::Rlike {
                src,
                pattern,
                case_sensitive,
            } => {
                w.i32(P::T_RLIKE_EVAL_TERM);
                children(self, w, &[*src, *pattern, *case_sensitive]);
            }
            PredExpr::Not(x) => children(self, w, &[*x]),
        }
    }
}
