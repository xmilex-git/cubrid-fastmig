//! Every class of the target catalog as a develop class object: the source's user classes rewritten, the system
//! classes taken from the template (kept slots, reallocated tables, new tables, new views).
//!
//! Input: the source classes and their plans, the target's catalog template, and the caller's placement: OIDs of
//! new root-heap slots, heaps of reallocated and new tables, representation directories, rebuilt BTIDs, and user
//! rows. Output: [`TargetClass`]es; dropped classes and 11.4 views are absent.
//! Source: workspace#482 decision 1 (roles), 2 (template), R3 § 5 F (what is remapped); placement is P1
//! integration (workspace#484).

use std::collections::HashMap;

use fastmig_ondisk::class_object::{ClassObject, ClassType, decode};
use fastmig_ondisk::value::{DecodeError, DecodeResult};
use fastmig_ondisk::{Btid, Hfid, Oid, VersionLine};

use crate::class_rewrite::{RewriteInputs, rewrite_user_class};
use crate::classify::{ClassKind, ClassPlan};
use crate::source_classes::SourceClass;
use crate::system_classes::{SystemClassRole, target_role};
use crate::template::CatalogTemplate;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A source user class, rewritten in its slot.
    User,
    /// A template system class put in the source class's slot (same-slot or reallocated table).
    TemplateInPlace(SystemClassRole),
    /// A template system class in a new slot (new table or system view).
    TemplateNewSlot(SystemClassRole),
}

#[derive(Clone, Debug)]
pub struct TargetClass {
    pub oid: Oid,
    pub class: ClassObject,
    pub origin: Origin,
}

/// The caller's placement decisions.
pub struct Placement<'a> {
    /// OID of a new root-heap slot for the template class named so.
    pub new_slot: &'a mut dyn FnMut(&[u8]) -> Oid,
    /// Heap of a reallocated or new table: `(target class OID, class name)`.
    pub new_heap: &'a mut dyn FnMut(Oid, &[u8]) -> Hfid,
    /// Representation directory of a target class.
    pub rep_dir: &'a dyn Fn(Oid) -> Oid,
    /// New BTID by old BTID (source or template); `None` writes the NULL BTID.
    pub new_btid: &'a dyn Fn(Btid) -> Option<Btid>,
    /// Target user row for a template user row (DBA → the source's DBA row, INFORMATION_SCHEMA → its new row).
    pub user: &'a dyn Fn(Oid) -> Oid,
}

fn err(what: String) -> DecodeError {
    DecodeError { at: 0, what }
}

pub fn target_classes(
    source: &[SourceClass],
    plans: &[ClassPlan],
    template: &CatalogTemplate,
    p: &mut Placement,
) -> DecodeResult<Vec<TargetClass>> {
    let mut out = Vec::new();
    // source system classes kept in place, by develop name
    let mut in_place: HashMap<Vec<u8>, (&SourceClass, SystemClassRole)> = HashMap::new();
    for (c, plan) in source.iter().zip(plans) {
        match plan.kind {
            ClassKind::User => out.push(TargetClass {
                oid: c.oid,
                class: rewrite_user_class(
                    c,
                    plan,
                    &RewriteInputs {
                        rep_dir: (p.rep_dir)(c.oid),
                        new_btid: p.new_btid,
                    },
                )?,
                origin: Origin::User,
            }),
            ClassKind::System(r @ SystemClassRole::SameSlot { develop_name }) => {
                in_place.insert(develop_name.as_bytes().to_vec(), (c, r));
            }
            ClassKind::System(r @ SystemClassRole::Reallocated) => {
                in_place.insert(c.class.name.clone(), (c, r));
            }
            ClassKind::System(_) => {}
        }
    }
    let tclasses = template
        .classes
        .iter()
        .map(|r| decode(&r.bytes, VersionLine::Develop).map(|c| (r.oid, c)))
        .collect::<DecodeResult<Vec<_>>>()?;
    // template class OID -> target class OID, fixed before any class is instantiated (domains name other classes)
    let mut map: HashMap<Oid, (Oid, Origin)> = HashMap::new();
    for (toid, t) in &tclasses {
        let name = String::from_utf8_lossy(&t.name).into_owned();
        let target = match in_place.get(&t.name) {
            Some((c, r)) => (c.oid, Origin::TemplateInPlace(*r)),
            None => match target_role(&name, t.class_type == ClassType::Vclass) {
                Some(r @ (SystemClassRole::New | SystemClassRole::View)) => {
                    ((p.new_slot)(&t.name), Origin::TemplateNewSlot(r))
                }
                r => {
                    return Err(err(format!(
                        "template class {name} ({r:?}) has no source class to replace"
                    )));
                }
            },
        };
        map.insert(*toid, target);
    }
    for (toid, t) in &tclasses {
        let (oid, origin) = map[toid];
        let heap = match (origin, in_place.get(&t.name)) {
            (Origin::TemplateInPlace(SystemClassRole::SameSlot { .. }), Some((c, _))) => {
                c.class.heap
            }
            _ if t.heap.is_null() => Hfid::NULL,
            _ => (p.new_heap)(oid, &t.name),
        };
        let class = crate::template_class::instantiate(
            t,
            &crate::template_class::Instance {
                class_oid: &|o| map.get(&o).map_or(o, |m| m.0),
                heap,
                rep_dir: (p.rep_dir)(oid),
                owner: p.user,
                new_btid: p.new_btid,
            },
        )?;
        out.push(TargetClass { oid, class, origin });
    }
    Ok(out)
}
