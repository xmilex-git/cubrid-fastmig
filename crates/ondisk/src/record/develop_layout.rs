//! The develop layout of an 11.4 representation: domains mapped, attributes put in the storage order develop's
//! schema manager derives (fixed attributes by alignment, then variable ones), keeping each attribute's id and the
//! repid (workspace#482 decision 4).
//!
//! Input: an 11.4 [`RecordLayout`]. Output: the develop [`RecordLayout`] a re-encoded class's records are written
//! under; the class-object writer must list the attributes in this same order.
//! Source: develop @5b1867174 `src/object/schema_manager.c` `build_storage_order` (walks the current storage order,
//! splits by `variable_p`, variables keep that order) and `order_atts_by_alignment` (repeatedly takes the first
//! attribute with the largest `alignment`, ties to the smaller `tp_domain_disk_size`), :9962-10170;
//! `PR_TYPE.alignment` in `src/object/object_primitive.c`; R2 § 1.7, § 3 R7.2.

use crate::VersionLine;
use crate::record::layout::{LayoutAttr, RecordLayout};
use crate::value::db_type::DbType;
use crate::value::develop_map::domain_to_develop;

/// Develop `PR_TYPE.alignment` of a fixed-size type (`variable_p == 0`).
fn alignment(ty: DbType) -> usize {
    match ty {
        DbType::Short | DbType::Enumeration => 2,
        DbType::Bit => 1,
        _ => 4,
    }
}

/// Develop `variable_p`: CHAR and NUMERIC became variable; BIT stays fixed whatever its precision.
fn is_variable(ty: DbType) -> bool {
    ty.constant_disk_size(VersionLine::Develop).is_none() && ty != DbType::Bit
}

impl RecordLayout {
    /// The develop layout for this 11.4 layout.
    pub fn to_develop(&self) -> RecordLayout {
        let mut fixed = Vec::new();
        let mut variable = Vec::new();
        for a in &self.attrs {
            let a = LayoutAttr {
                id: a.id,
                domain: domain_to_develop(&a.domain),
            };
            if is_variable(a.domain.ty) {
                variable.push(a);
            } else {
                fixed.push(a);
            }
        }
        let mut ordered = Vec::with_capacity(self.attrs.len());
        while !fixed.is_empty() {
            let key = |a: &LayoutAttr| {
                (
                    alignment(a.domain.ty),
                    a.domain.disk_size(VersionLine::Develop).unwrap_or(0),
                )
            };
            let mut best = 0;
            for i in 1..fixed.len() {
                let (al, sz) = key(&fixed[i]);
                let (bal, bsz) = key(&fixed[best]);
                if al > bal || (al == bal && sz < bsz) {
                    best = i;
                }
            }
            ordered.push(fixed.remove(best));
        }
        let n_fixed = ordered.len();
        ordered.extend(variable);
        RecordLayout {
            repid: self.repid,
            n_fixed,
            attrs: ordered,
        }
    }
}
