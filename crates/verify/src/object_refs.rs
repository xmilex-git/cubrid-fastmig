//! OBJECT values and LOB locators of live rows, collected during the heap pass, and the check that every OBJECT value
//! names a live row of the class its domain allows.
//!
//! Input: a row's layout and decoded values (collection); every collected reference and the sorted live-OID set
//! (check). Output: [`ObjectRef`]s and [`LobRef`]s; check 3 defects `object-dangling` and `object-wrong-class`.
//! Catalog rows (`_db_*` / `db_*`: class_of, owner, grantee, ...) are the case measured on T1; user OBJECT columns go
//! through the same path but are no supported use case and have no fixture (user decision relayed by the map
//! supervisor, workspace#495).
//! Source: G3 workspace#483 Q7 (OBJECT values must name live rows; merged against the live OID set); G6
//! workspace#488 (LOB locators).

use fastmig_ondisk::Oid;
use fastmig_ondisk::record::RecordLayout;
use fastmig_ondisk::value::{Elo, Value};

use crate::defect::{Check, Defect, OidText};
use crate::live_oids::LiveOids;

/// One OBJECT value of a live row.
#[derive(Clone, Copy, Debug)]
pub struct ObjectRef {
    pub to: Oid,
    pub from: Oid,
    pub attr: i32,
    /// The class the attribute's domain names (`OBJECT` of a class); `None` for a generic OBJECT or a set element.
    pub domain_class: Option<Oid>,
}

/// One LOB value of a live row.
#[derive(Clone, Debug)]
pub struct LobRef {
    pub from: Oid,
    pub attr: i32,
    pub elo: Elo,
}

/// Appends the row's OBJECT values (set elements included) and LOB values.
pub fn collect_refs(
    from: Oid,
    layout: &RecordLayout,
    values: &[Value],
    refs: &mut Vec<ObjectRef>,
    lobs: &mut Vec<LobRef>,
) {
    for (a, v) in layout.attrs.iter().zip(values) {
        walk(v, from, a.id, a.domain.class_oid, refs, lobs);
    }
}

fn walk(
    v: &Value,
    from: Oid,
    attr: i32,
    domain_class: Option<Oid>,
    refs: &mut Vec<ObjectRef>,
    lobs: &mut Vec<LobRef>,
) {
    match v {
        Value::Object(o) | Value::Oid(o) if !o.is_null() => refs.push(ObjectRef {
            to: *o,
            from,
            attr,
            domain_class: domain_class.filter(|c| !c.is_null()),
        }),
        Value::Set(s) => {
            for e in &s.elements {
                walk(&e.value, from, attr, None, refs, lobs);
            }
        }
        Value::Elo(e) => lobs.push(LobRef {
            from,
            attr,
            elo: e.clone(),
        }),
        _ => {}
    }
}

/// Check 3: each reference names a live row; with a class domain, a row of that class or of one it accepts
/// (`accepts(domain class, row class)`: a partition of the domain class). `class_name` names a class OID.
pub fn check_refs(
    refs: &[ObjectRef],
    live: &LiveOids,
    class_name: &dyn Fn(Oid) -> String,
    accepts: &dyn Fn(Oid, Oid) -> bool,
) -> Vec<Defect> {
    let mut out = Vec::new();
    for r in refs.iter() {
        let from_class = live.class_of(r.from).map_or_else(|| "-".into(), class_name);
        let d = |code| {
            Defect::new(Check::Catalog, code)
                .class(from_class.clone())
                .object(format!("{} attr {}", OidText(r.from), r.attr))
        };
        match live.class_of(r.to) {
            None => out.push(
                d("object-dangling")
                    .expected("a live row")
                    .actual(OidText(r.to)),
            ),
            Some(c) => {
                if let Some(want) = r.domain_class
                    && !accepts(want, c)
                {
                    out.push(
                        d("object-wrong-class")
                            .expected(class_name(want))
                            .actual(format!("{} of {}", OidText(r.to), class_name(c))),
                    );
                }
            }
        }
    }
    out
}
