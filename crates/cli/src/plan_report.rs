//! The plan as `plan.json` and as the table `plan` prints.
//!
//! Input: a [`Plan`] and its [`TargetSpace`]. Output: JSON text (`plan --json`, `<dest>/fastmig/plan.json`) or a human-readable table.
//! Source: decision G1 workspace#481 (plan prints a table, `--json` too; migrate leaves plan.json).

use fastmig_plan::version_pair::line_name;
use fastmig_plan::{HeapTreatment, PageTreatment, Plan};
use fastmig_verify::report::json_str;

use crate::target_space::TargetSpace;

fn heap_name(h: HeapTreatment) -> &'static str {
    match h {
        HeapTreatment::None => "none",
        HeapTreatment::Freed => "freed",
        HeapTreatment::InPlace(PageTreatment::AsIs) => "as-is",
        HeapTreatment::InPlace(PageTreatment::Patch) => "patch",
        HeapTreatment::InPlace(PageTreatment::Transform) => "transform",
    }
}

fn strings<'a>(v: impl IntoIterator<Item = &'a str>) -> String {
    let v: Vec<String> = v.into_iter().map(json_str).collect();
    format!("[{}]", v.join(", "))
}

/// `resources`: the resource table as JSON (`resource_estimate`), or `null` when it was not estimated.
pub fn json(p: &Plan, t: &TargetSpace, resources: &str) -> String {
    let vols: Vec<String> = p
        .volumes
        .iter()
        .map(|v| {
            format!(
                "{{\"volid\": {}, \"path\": {}, \"bytes\": {}, \"copy_method\": \"{}\"}}",
                v.volid,
                json_str(&v.path.to_string_lossy()),
                v.bytes,
                v.copy_method.name()
            )
        })
        .collect();
    let classes: Vec<String> = p
        .classes
        .iter()
        .map(|c| {
            format!(
                "{{\"oid\": \"{}|{}|{}\", \"name\": {}, \"system\": {}, \"re_encoded\": {}, \"heap\": \"{}\"}}",
                c.oid.volid,
                c.oid.pageid,
                c.oid.slotid,
                json_str(&c.name),
                c.system,
                c.re_encoded,
                heap_name(c.heap)
            )
        })
        .collect();
    let l = &p.lob;
    let outside: Vec<String> = l
        .outside_base
        .iter()
        .map(|f| {
            format!(
                "{{\"path\": {}, \"size\": {}}}",
                json_str(&f.rel_path.to_string_lossy()),
                f.size
            )
        })
        .collect();
    format!(
        "{{\n  \"version_pair\": {{\"source\": \"{}\", \"target\": \"{}\"}},\n  \"refusals\": {},\n  \"warnings\": {},\n  \"volumes\": [{}],\n  \"re_encoded_classes\": {},\n  \"deferred_indexes\": {},\n  \"system_name_rewrites\": {},\n  \"plcsql_routines\": {},\n  \"lob\": {{\"referenced_files\": {}, \"referenced_bytes\": {}, \"absolute_under_base\": {}, \"outside_base\": [{}], \"unsupported\": {}, \"orphan_files\": {}, \"orphan_bytes\": {}}},\n  \"java\": {{\"files\": {}, \"bytes\": {}}},\n  \"java_static\": {{\"files\": {}, \"bytes\": {}}},\n  \"target_space\": {{\"copy_bytes\": {}, \"log_bytes\": {}, \"unshare\": {}, \"unshare_bytes\": {}, \"transfer_bytes\": {}, \"listed_sectors\": {}, \"short_sectors\": {}, \"reserved_bytes\": {}, \"needed_bytes\": {}, \"available_bytes\": {}}},\n  \"treated_pages\": {},\n  \"resources\": {resources},\n  \"classes\": [\n    {}\n  ]\n}}\n",
        p.source_version.name(),
        line_name(p.version_pair.target),
        strings(p.refusals.iter().map(String::as_str)),
        strings(p.warnings.iter().map(String::as_str)),
        vols.join(", "),
        strings(p.re_encoded()),
        strings(p.deferred_indexes.iter().map(String::as_str)),
        strings(p.system_name_rewrites.iter().map(String::as_str)),
        p.plcsql_routines,
        l.referenced.len(),
        l.referenced_bytes(),
        l.absolute_under_base,
        outside.join(", "),
        l.unsupported.len(),
        l.orphans.len(),
        l.orphan_bytes(),
        p.java.files,
        p.java.bytes,
        p.java_static.files,
        p.java_static.bytes,
        t.copy_bytes,
        t.log_bytes,
        p.unshare,
        t.unshare_bytes,
        t.transfer_bytes,
        t.sectors.len(),
        t.short_sectors,
        t.extra_bytes,
        t.needed(),
        p.available_bytes,
        p.treated_pages,
        classes.join(",\n    ")
    )
}

pub fn table(p: &Plan, t: &TargetSpace) -> String {
    let mut s = format!(
        "version pair: {} -> {}\n",
        p.source_version.name(),
        line_name(p.version_pair.target)
    );
    s.push_str("volumes:\n");
    for v in &p.volumes {
        s.push_str(&format!(
            "  {:>4}  {:<5}  {:>14} B  {}\n",
            v.volid,
            v.copy_method.name(),
            v.bytes,
            v.path.display()
        ));
    }
    let count = |h: HeapTreatment| p.classes.iter().filter(|c| c.heap == h).count();
    s.push_str(&format!(
        "class heaps: {} patch, {} transform, {} freed, {} without heap\n",
        count(HeapTreatment::InPlace(PageTreatment::Patch)),
        count(HeapTreatment::InPlace(PageTreatment::Transform)),
        count(HeapTreatment::Freed),
        count(HeapTreatment::None)
    ));
    let re: Vec<&str> = p.re_encoded().collect();
    s.push_str(&format!(
        "re-encoded classes ({}): {}\n",
        re.len(),
        re.join(", ")
    ));
    s.push_str(&format!(
        "deferred indexes ({}): {}\n",
        p.deferred_indexes.len(),
        p.deferred_indexes.join(", ")
    ));
    s.push_str(&format!(
        "system name rewrites ({}):\n",
        p.system_name_rewrites.len()
    ));
    p.system_name_rewrites
        .iter()
        .for_each(|r| s.push_str(&format!("  {r}\n")));
    s.push_str(&format!(
        "LOB: {} referenced files ({} B), {} orphan files ({} B), {} outside the base\n",
        p.lob.referenced.len(),
        p.lob.referenced_bytes(),
        p.lob.orphans.len(),
        p.lob.orphan_bytes(),
        p.lob.outside_base.len()
    ));
    let mib = |b: u64| b.div_ceil(1 << 20);
    s.push_str(&format!(
        "target space taken at the start: {} MiB byte-copied volumes + {} MiB new log + {} MiB cloned pages unshared ({} sectors listed, {} short) + {} MiB reserved (upper bound of the rest, held until the end: needs {} MiB free); {} MiB free\n",
        mib(t.copy_bytes),
        mib(t.log_bytes),
        mib(t.unshare_bytes),
        t.sectors.len(),
        t.short_sectors,
        mib(t.extra_bytes),
        mib(t.needed()),
        mib(p.available_bytes)
    ));
    s.push_str(&format!(
        "unreserved writes ≤ {} MiB: {}LOB and Java files are written outside the space taken at the start; keep that much free of other writers during the run\n",
        mib(t.extra_bytes),
        if p.unshare {
            "sectors taken off the listed ones, a volume expansion, "
        } else if p.volumes.iter().any(|v| v.copy_method == fastmig_plan::CopyMethod::Clone) {
            "rewritten pages of the cloned volumes (this filesystem cannot unshare them at the start: not XFS), new B-trees, appended heap pages, "
        } else {
            "new B-trees, appended heap pages, "
        }
    ));
    s.push_str(&format!("warnings ({}):\n", p.warnings.len()));
    p.warnings
        .iter()
        .for_each(|w| s.push_str(&format!("  {w}\n")));
    s.push_str(&format!("refusals ({}):\n", p.refusals.len()));
    p.refusals
        .iter()
        .for_each(|r| s.push_str(&format!("  {r}\n")));
    s
}
