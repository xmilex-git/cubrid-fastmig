//! Sorts the defects of a verification into what fails the migration and what does not.
//!
//! Input: the target's defects and, when the source was verified too, the source's. Output: [`Classified`] defects
//! with a [`DefectOrigin`]. A target defect whose identity the source also has is pre-existing (reported, not failing);
//! one the source lacks is a migration defect; a source defect the target no longer has is resolved (e.g. by the
//! index rebuild). With no source run every defect counts as a migration defect.
//! Source: G3 workspace#483 Q6.

use std::collections::HashSet;

use crate::defect::{Defect, DefectId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefectOrigin {
    Migration,
    PreExisting,
    Resolved,
}

impl DefectOrigin {
    pub fn name(self) -> &'static str {
        match self {
            DefectOrigin::Migration => "migration",
            DefectOrigin::PreExisting => "pre-existing",
            DefectOrigin::Resolved => "resolved",
        }
    }
}

pub struct Classified {
    pub origin: DefectOrigin,
    /// `target` or `source`: the run the defect was found in.
    pub db: &'static str,
    pub defect: Defect,
}

/// The identity a target defect has under its source class name: a source before user schema (10.2, 11.0) names a
/// user class `t` where the target names it `owner.t` (workspace#544). Those names have one namespace and no `.`
/// (plan pre-check P3), so dropping the owner prefix is unambiguous; a later source's names carry the prefix on both
/// sides and only match as they are (workspace#539).
fn source_named_id(d: &Defect) -> DefectId {
    let mut id = d.id();
    if let Some((_, name)) = id.1.split_once('.') {
        id.1 = name.to_string();
    }
    id
}

pub fn classify(target: Vec<Defect>, source: Option<Vec<Defect>>) -> Vec<Classified> {
    let Some(source) = source else {
        return target
            .into_iter()
            .map(|defect| Classified {
                origin: DefectOrigin::Migration,
                db: "target",
                defect,
            })
            .collect();
    };
    let src_ids: HashSet<DefectId> = source.iter().map(Defect::id).collect();
    let tgt_ids: HashSet<DefectId> = target
        .iter()
        .flat_map(|d| [d.id(), source_named_id(d)])
        .collect();
    let mut out: Vec<Classified> = target
        .into_iter()
        .map(|defect| Classified {
            origin: if src_ids.contains(&defect.id()) || src_ids.contains(&source_named_id(&defect))
            {
                DefectOrigin::PreExisting
            } else {
                DefectOrigin::Migration
            },
            db: "target",
            defect,
        })
        .collect();
    out.extend(
        source
            .into_iter()
            .filter(|d| !tgt_ids.contains(&d.id()))
            .map(|defect| Classified {
                origin: DefectOrigin::Resolved,
                db: "source",
                defect,
            }),
    );
    out
}
