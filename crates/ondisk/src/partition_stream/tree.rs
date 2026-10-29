//! The node tree of a partition expression stream: the subset of XASL nodes a `FUNC_PRED` stream holds, with shared
//! nodes kept shared.
//!
//! Input: built by [`super::read`]; changed by [`super::to_develop`]; written by [`super::write`].
//! Output: [`StreamTree`] and its canonical text form (offsets replaced by first-visit node numbers), which is equal
//! for two streams exactly when their trees are.
//! Source: engine `xasl_to_stream.c` `xts_process_func_pred`, `xts_process_regu_variable`,
//! `xts_pack_regu_variable_value`, `xts_process_arith_type`, `xts_process_function_type`,
//! `xts_save_regu_variable_list`, `xts_process_pred_expr`, `xts_process_pred`, `xts_process_*_eval_term`,
//! `xts_process_attr_descr`, `xts_process_regu_value_list` (release/11.4 @46212adcd and develop @5b1867174, field
//! order the same; R7 § 2.2).

use std::fmt::Write as _;

use crate::value::{Domain, Value};

/// Index of a node in [`StreamTree::nodes`]. Two fields holding the same id are one shared node, as two stream
/// offsets naming the same node are.
pub type NodeId = usize;

/// Engine `REGU_DATATYPE` (`regu_var.hpp`, same order in both lines).
pub mod regu_type {
    pub const DBVAL: i32 = 0;
    pub const CONSTANT: i32 = 1;
    pub const ORDERBY_NUM: i32 = 2;
    pub const INARITH: i32 = 3;
    pub const OUTARITH: i32 = 4;
    pub const ATTR_ID: i32 = 5;
    pub const CLASS_ATTR_ID: i32 = 6;
    pub const SHARED_ATTR_ID: i32 = 7;
    pub const POSITION: i32 = 8;
    pub const LIST_ID: i32 = 9;
    pub const POS_VALUE: i32 = 10;
    pub const OID: i32 = 11;
    pub const CLASSOID: i32 = 12;
    pub const FUNC: i32 = 13;
    pub const REGUVAL_LIST: i32 = 14;
    pub const REGU_VAR_LIST: i32 = 15;
    pub const SP: i32 = 16;
}

/// Engine `TYPE_PRED_EXPR` and `TYPE_EVAL_TERM` (`xasl_predicate.hpp`, same in both lines).
pub mod pred_type {
    pub const T_PRED: i32 = 1;
    pub const T_EVAL_TERM: i32 = 2;
    pub const T_NOT_TERM: i32 = 3;
    pub const T_COMP_EVAL_TERM: i32 = 1;
    pub const T_ALSM_EVAL_TERM: i32 = 2;
    pub const T_LIKE_EVAL_TERM: i32 = 3;
    pub const T_RLIKE_EVAL_TERM: i32 = 4;
}

/// The part of a `REGU_VARIABLE` that depends on its type (`xts_pack_regu_variable_value`).
// a stream holds tens of nodes, so the inline DB_VALUE is not worth a box
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum ReguValue {
    /// `TYPE_DBVAL`: a tagged value written inline.
    DbVal { tag: Domain, value: Value },
    /// `TYPE_CONSTANT`, `TYPE_ORDERBY_NUM` (a DB_VALUE node), `TYPE_INARITH`, `TYPE_OUTARITH` (an ARITH node),
    /// `TYPE_FUNC` (a FUNCTION node): one offset.
    Ptr(Option<NodeId>),
    /// `TYPE_ATTR_ID`, `TYPE_CLASS_ATTR_ID`, `TYPE_SHARED_ATTR_ID`: `ATTR_DESCR` inline.
    Attr {
        id: i32,
        db_type: i32,
        cache: Option<NodeId>,
    },
    /// `TYPE_POS_VALUE`: a host variable position.
    PosValue(i32),
    /// `TYPE_OID`, `TYPE_CLASSOID`: nothing written.
    Empty,
    /// `TYPE_REGU_VAR_LIST`: the offset of a regu list node.
    VarList(Option<NodeId>),
    /// `TYPE_REGUVAL_LIST`: count, then per item its type and its value written inline.
    ValList(Vec<(i32, ReguValue)>),
}

/// One `PRED_EXPR` (`xts_process_pred_expr`).
#[derive(Clone, Debug, PartialEq)]
pub enum PredExpr {
    /// `T_PRED`: a right-linear AND/OR chain written flat. `links[i]` is (lhs, bool_op); the chain ends in `rhs`,
    /// whose own type the stream repeats before its offset.
    Pred {
        links: Vec<(Option<NodeId>, i32)>,
        rhs: Option<NodeId>,
    },
    Comp {
        lhs: Option<NodeId>,
        rhs: Option<NodeId>,
        rel_op: i32,
        db_type: i32,
    },
    Alsm {
        elem: Option<NodeId>,
        elemset: Option<NodeId>,
        eq_flag: i32,
        rel_op: i32,
        item_type: i32,
    },
    Like {
        src: Option<NodeId>,
        pattern: Option<NodeId>,
        esc_char: Option<NodeId>,
    },
    Rlike {
        src: Option<NodeId>,
        pattern: Option<NodeId>,
        case_sensitive: Option<NodeId>,
    },
    Not(Option<NodeId>),
}

impl PredExpr {
    /// The `TYPE_PRED_EXPR` the stream writes for this node.
    pub fn type_word(&self) -> i32 {
        match self {
            PredExpr::Pred { .. } => pred_type::T_PRED,
            PredExpr::Not(_) => pred_type::T_NOT_TERM,
            _ => pred_type::T_EVAL_TERM,
        }
    }
}

/// One node stored at its own stream offset.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// `FUNC_PRED`, the root.
    FuncPred {
        regu: Option<NodeId>,
        cache: Option<NodeId>,
    },
    /// `HEAP_CACHE_ATTRINFO`: a single int 0.
    CacheAttrinfo,
    /// `REGU_VARIABLE`. The `xasl` offset is always 0 here (no subqueries in a partition expression).
    Regu {
        domain: Vec<Domain>,
        /// develop only (CBRD-27583): the domain the value is read in and the `REGU_CONV_*` mode, when its consumer
        /// wants it in `domain`. An 11.4 stream has none.
        conversion: Option<(Vec<Domain>, i32)>,
        ty: i32,
        flags: i32,
        vfetch_to: Option<NodeId>,
        value: ReguValue,
    },
    /// A `DB_VALUE` saved on its own (`xts_save_db_value`): tag plus value.
    DbValue {
        tag: Domain,
        value: Value,
    },
    /// `ARITH_TYPE`; `pred` exists only for the opcodes that carry one (T_CASE, T_DECODE, T_PREDICATE, T_IF).
    Arith {
        domain: Vec<Domain>,
        value: Option<NodeId>,
        opcode: i32,
        left: Option<NodeId>,
        right: Option<NodeId>,
        third: Option<NodeId>,
        misc_operand: i32,
        /// develop only (CBRD-27583): the error the operator raises on non-NULL operands, or 0 (`NO_ERROR`).
        fails: i32,
        pred: Option<Option<NodeId>>,
    },
    /// `FUNCTION_TYPE`.
    Function {
        value: Option<NodeId>,
        ftype: i32,
        operands: Option<NodeId>,
    },
    /// `REGU_VARIABLE_LIST`: an int array [count, offsets of the elements' REGU_VARIABLE nodes].
    ReguList(Vec<Option<NodeId>>),
    Pred(PredExpr),
}

/// A parsed stream: its nodes and the root `FUNC_PRED`.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamTree {
    pub nodes: Vec<Node>,
    pub root: NodeId,
}

impl StreamTree {
    /// The tree as text: nodes in depth-first order from the root, each numbered at its first visit, a revisit
    /// printed as `@n`. Two streams with the same tree give the same text whatever their offsets.
    pub fn canonical(&self) -> String {
        let mut c = Canon {
            tree: self,
            seen: vec![None; self.nodes.len()],
            next: 0,
            out: String::new(),
        };
        c.node(Some(self.root), 0);
        c.out
    }
}

struct Canon<'a> {
    tree: &'a StreamTree,
    seen: Vec<Option<usize>>,
    next: usize,
    out: String,
}

impl Canon<'_> {
    fn line(&mut self, depth: usize, text: &str) {
        let _ = writeln!(self.out, "{:width$}{text}", "", width = depth * 2);
    }

    fn node(&mut self, id: Option<NodeId>, depth: usize) {
        let Some(id) = id else {
            self.line(depth, "null");
            return;
        };
        if let Some(n) = self.seen[id] {
            self.line(depth, &format!("@{n}"));
            return;
        }
        let n = self.next;
        self.next += 1;
        self.seen[id] = Some(n);
        let d = depth + 1;
        match &self.tree.nodes[id] {
            Node::FuncPred { regu, cache } => {
                self.line(depth, &format!("#{n} FUNC_PRED"));
                self.node(*regu, d);
                self.node(*cache, d);
            }
            Node::CacheAttrinfo => self.line(depth, &format!("#{n} CACHE_ATTRINFO")),
            Node::Regu {
                domain,
                conversion,
                ty,
                flags,
                vfetch_to,
                value,
            } => {
                self.line(
                    depth,
                    &format!(
                        "#{n} REGU type={ty} flags={flags:#x} domain={domain:?} conversion={conversion:?}"
                    ),
                );
                self.node(*vfetch_to, d);
                self.regu_value(value, d);
            }
            Node::DbValue { tag, value } => {
                self.line(depth, &format!("#{n} DB_VALUE {tag:?} {value:?}"))
            }
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
                self.line(
                    depth,
                    &format!(
                        "#{n} ARITH opcode={opcode} misc={misc_operand} fails={fails} domain={domain:?}"
                    ),
                );
                for c in [*value, *left, *right, *third] {
                    self.node(c, d);
                }
                if let Some(p) = pred {
                    self.node(*p, d);
                }
            }
            Node::Function {
                value,
                ftype,
                operands,
            } => {
                self.line(depth, &format!("#{n} FUNCTION ftype={ftype}"));
                self.node(*value, d);
                self.node(*operands, d);
            }
            Node::ReguList(items) => {
                self.line(depth, &format!("#{n} REGU_LIST count={}", items.len()));
                for i in items.clone() {
                    self.node(i, d);
                }
            }
            Node::Pred(p) => self.pred(n, p, depth),
        }
    }

    fn regu_value(&mut self, v: &ReguValue, depth: usize) {
        match v {
            ReguValue::DbVal { tag, value } => {
                self.line(depth, &format!("dbval {tag:?} {value:?}"))
            }
            ReguValue::Ptr(p) | ReguValue::VarList(p) => self.node(*p, depth),
            ReguValue::Attr { id, db_type, cache } => {
                self.line(depth, &format!("attr id={id} type={db_type}"));
                self.node(*cache, depth + 1);
            }
            ReguValue::PosValue(p) => self.line(depth, &format!("pos_value {p}")),
            ReguValue::Empty => self.line(depth, "empty"),
            ReguValue::ValList(items) => {
                self.line(depth, &format!("val_list count={}", items.len()));
                for (ty, item) in items {
                    self.line(depth + 1, &format!("item type={ty}"));
                    self.regu_value(item, depth + 2);
                }
            }
        }
    }

    fn pred(&mut self, n: usize, p: &PredExpr, depth: usize) {
        let d = depth + 1;
        match p {
            PredExpr::Pred { links, rhs } => {
                let ops: Vec<i32> = links.iter().map(|l| l.1).collect();
                self.line(depth, &format!("#{n} PRED bool_ops={ops:?}"));
                for (lhs, _) in links.clone() {
                    self.node(lhs, d);
                }
                self.node(*rhs, d);
            }
            PredExpr::Comp {
                lhs,
                rhs,
                rel_op,
                db_type,
            } => {
                self.line(depth, &format!("#{n} COMP rel_op={rel_op} type={db_type}"));
                self.node(*lhs, d);
                self.node(*rhs, d);
            }
            PredExpr::Alsm {
                elem,
                elemset,
                eq_flag,
                rel_op,
                item_type,
            } => {
                self.line(
                    depth,
                    &format!("#{n} ALSM eq_flag={eq_flag} rel_op={rel_op} item_type={item_type}"),
                );
                self.node(*elem, d);
                self.node(*elemset, d);
            }
            PredExpr::Like {
                src,
                pattern,
                esc_char,
            } => {
                self.line(depth, &format!("#{n} LIKE"));
                for c in [*src, *pattern, *esc_char] {
                    self.node(c, d);
                }
            }
            PredExpr::Rlike {
                src,
                pattern,
                case_sensitive,
            } => {
                self.line(depth, &format!("#{n} RLIKE"));
                for c in [*src, *pattern, *case_sensitive] {
                    self.node(c, d);
                }
            }
            PredExpr::Not(x) => {
                self.line(depth, &format!("#{n} NOT"));
                self.node(*x, d);
            }
        }
    }
}
