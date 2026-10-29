//! The partition substructure of a class object: the partitioning of a partitioned class, or one partition's bounds.
//!
//! Input: one element of the class's `partition` set and the release line. Output: [`Partition`].
//! Source: engine `transform_cl.c:partition_info_to_disk` / `disk_to_partition_info` (vars: pname, pexpr, pvalues,
//! comment; fixed: ptype, plus class_partition_type in develop only); release/11.4 @46212adcd fixed 4 B,
//! develop @5b1867174 fixed 8 B (1f1263217, PR #6443); R3 § 2.3.

use super::or_be::{Structure, opt_string, within};
use super::sub_set;
use crate::VersionLine;
use crate::value::DecodeResult;

/// One partition element. `class_partition_type` is `None` in 11.4, where the record does not store it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Partition {
    /// `DB_PARTITION_TYPE`: 0 HASH, 1 RANGE, 2 LIST.
    pub ptype: i32,
    /// develop `DB_CLASS_PARTITION_TYPE`: 1 partitioned class, 2 partition.
    pub class_partition_type: Option<i32>,
    /// Partition name; `None` on the partitioned class itself.
    pub pname: Option<Vec<u8>>,
    /// Partition expression text.
    pub pexpr: Option<Vec<u8>>,
    /// Packed `pvalues` sequence (`or_put_value`): the expression stream and the bound values.
    pub pvalues: Vec<u8>,
    pub comment: Option<Vec<u8>>,
}

impl Partition {
    fn decode(elem: &[u8], line: VersionLine) -> DecodeResult<Partition> {
        let s = Structure::new(elem, 4)?;
        let mut f = s.fixed();
        let ptype = f.i32()?;
        let class_partition_type = match line {
            VersionLine::V11_4 => None,
            VersionLine::Develop => Some(f.i32()?),
        };
        Ok(Partition {
            ptype,
            class_partition_type,
            pname: within(opt_string(s.var(0)), "pname")?,
            pexpr: within(opt_string(s.var(1)), "pexpr")?,
            pvalues: s.var(2).to_vec(),
            comment: within(opt_string(s.var(3)), "comment")?,
        })
    }
}

pub(crate) fn list(field: &[u8], line: VersionLine) -> DecodeResult<Vec<Partition>> {
    sub_set::elements(field)?
        .into_iter()
        .map(|e| Partition::decode(e, line))
        .collect()
}
