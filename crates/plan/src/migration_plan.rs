//! The migration plan: everything decided before a byte is written — the version pair, each volume's copy method,
//! each class's page treatment grade, the deferred indexes, the LOB manifest, and the pre-check's refusals and
//! warnings. `plan` prints it; `migrate` builds the same plan, refuses on any refusal, and follows it.
//!
//! Input: [`PlanInputs`] (the open source, its log header, the target directory and choices). Output: [`Plan`].
//! Source: decision G1 workspace#481 (plan command, pre-check refusals and warnings, copy method × page treatment
//! grade), G2 workspace#482 (re-encoded classes, collations, NCHAR), G5 workspace#486 (deferred indexes), G6
//! workspace#488 (LOB scan, Java directories, lob-base-path rules), workspace#498 (`attr_default` refusals),
//! workspace#491 (LDML collations are not ordered here), workspace#505 (this assembly).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use fastmig_catalog::class_rewrite::{RewriteInputs, rewrite_user_class};
use fastmig_catalog::class_rows::{Row, read_rows};
use fastmig_catalog::classify::{ClassKind, classify};
use fastmig_catalog::collation_check::check_collations;
use fastmig_catalog::deferred_indexes::{deferred_indexes, report_name};
use fastmig_catalog::output_sql::{deferred_indexes_sql, plcsql_routines, recompile_plcsql_sql};
use fastmig_catalog::source_classes::{SourceClass, read_classes};
use fastmig_catalog::system_classes::SystemClassRole;
use fastmig_catalog::{attr_default, template_store};
use fastmig_ondisk::boot_parm::BootDbParm;
use fastmig_ondisk::class_object::{ClassType, class_properties};
use fastmig_ondisk::databases_txt::DatabaseEntry;
use fastmig_ondisk::file_header::{FileHeader, FileType};
use fastmig_ondisk::log_header::LogHeader;
use fastmig_ondisk::value::{Collation, DbType, Value};
use fastmig_ondisk::volume_set::VolumeSet;
use fastmig_ondisk::{Oid, VersionLine, file_tracker};

use crate::copy_method::{self, CopyMethod};
use crate::free_space;
use crate::lob_scan::{self, LobScan};
use crate::system_view_refs;
use crate::version_pair::VersionPair;

/// Engine `SM_CLASSFLAG_SYSTEM`.
const CLASSFLAG_SYSTEM: i32 = 1;

pub struct PlanInputs<'a> {
    pub source: &'a DatabaseEntry,
    /// Where the source is registered (other databases sharing its lob-base-path are named from it).
    pub source_dbtxt: &'a Path,
    pub vols: &'a VolumeSet,
    pub log: &'a LogHeader,
    pub target_line: VersionLine,
    pub target_dir: &'a Path,
    pub allow_clone: bool,
}

/// Engine names of the GLOSSARY's page treatment grade.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageTreatment {
    /// Pages kept byte for byte.
    AsIs,
    /// Fields fixed in place, vacuum residue removed (patch target pages only).
    Patch,
    /// Every record re-encoded or the structure rewritten.
    Transform,
}

/// What happens to one class's heap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeapTreatment {
    /// No heap (a view, or a class that never stored a row).
    None,
    /// The heap is freed and the table's rows are regenerated elsewhere (reallocated or dropped system table).
    Freed,
    /// The heap stays in place; its pages get at most this grade.
    InPlace(PageTreatment),
}

#[derive(Clone, Debug)]
pub struct ClassTreatment {
    pub oid: Oid,
    pub name: String,
    pub system: bool,
    /// A re-encoded class (GLOSSARY).
    pub re_encoded: bool,
    pub heap: HeapTreatment,
}

#[derive(Clone, Debug)]
pub struct VolumePlan {
    pub volid: i16,
    pub path: PathBuf,
    pub bytes: u64,
    pub copy_method: CopyMethod,
}

/// Files below one source directory (Java SP directories).
#[derive(Clone, Debug, Default)]
pub struct DirSize {
    pub files: u64,
    pub bytes: u64,
}

pub struct Plan {
    pub version_pair: VersionPair,
    pub volumes: Vec<VolumePlan>,
    pub classes: Vec<ClassTreatment>,
    /// `class.index` per deferred index (partitions' copies left out).
    pub deferred_indexes: Vec<String>,
    /// `<dest>/fastmig/deferred-indexes.sql`.
    pub deferred_indexes_sql: String,
    /// `<dest>/fastmig/recompile-plcsql.sql`.
    pub recompile_plcsql_sql: String,
    pub plcsql_routines: usize,
    pub lob: LobScan,
    pub java: DirSize,
    pub java_static: DirSize,
    /// Bytes the target filesystem needs and has (pre-check).
    pub needed_bytes: u64,
    pub available_bytes: u64,
    /// Non-empty means migrate does not start.
    pub refusals: Vec<String>,
    pub warnings: Vec<String>,
}

impl Plan {
    /// The re-encoded classes by name.
    pub fn re_encoded(&self) -> impl Iterator<Item = &str> {
        self.classes
            .iter()
            .filter(|c| c.re_encoded)
            .map(|c| c.name.as_str())
    }
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn dir_size(dir: &Path) -> io::Result<DirSize> {
    let mut s = DirSize::default();
    if !dir.is_dir() {
        return Ok(s);
    }
    for e in fs::read_dir(dir)? {
        let e = e?;
        let m = e.metadata()?;
        if m.is_dir() {
            let d = dir_size(&e.path())?;
            s.files += d.files;
            s.bytes += d.bytes;
        } else {
            s.files += 1;
            s.bytes += m.len();
        }
    }
    Ok(s)
}

fn rows_of(
    vs: &VolumeSet,
    line: VersionLine,
    classes: &[SourceClass],
    name: &[u8],
) -> io::Result<Vec<Row>> {
    match classes.iter().find(|c| c.class.name == name) {
        Some(c) => read_rows(vs, line, c),
        None => Ok(Vec::new()),
    }
}

fn text(v: Option<&Value>) -> Option<&[u8]> {
    match v? {
        Value::Varchar(s) | Value::Char(s) => Some(s),
        _ => None,
    }
}

/// The lob-base-path as a directory: `file:<dir>` only (G6 4: `owfs:` has no path in 11.4 or develop builds).
pub fn lob_base_dir(source: &DatabaseEntry) -> Result<Option<PathBuf>, String> {
    match &source.lob_base_path {
        Some(l) => match l.strip_prefix("file:") {
            Some(d) => Ok(Some(PathBuf::from(d))),
            None => Err(format!("lob-base-path {l} is not file: (workspace#488)")),
        },
        None => Ok(None),
    }
}

pub fn build(i: &PlanInputs) -> io::Result<Plan> {
    let line = fastmig_ondisk::VersionLine::from_db_compatibility(i.log.db_compatibility)
        .ok_or_else(|| {
            invalid(format!(
                "unsupported source db_compatibility {}",
                i.log.db_compatibility
            ))
        })?;
    let pair = VersionPair {
        source: line,
        target: i.target_line,
    };
    let regenerates = pair.regenerates_catalog();
    let mut refusals: Vec<String> = pair.refusal().into_iter().collect();
    let mut warnings = Vec::new();

    // the log and the volumes: one cleanly shut down database
    if !i.log.is_shutdown {
        refusals.push("the source was not shut down cleanly (log header is_shutdown = 0)".into());
    }
    if i.log.db_iopagesize as usize != i.vols.io_pagesize() {
        refusals.push("log header and volume iopagesize differ".into());
    }
    let mut volumes = Vec::new();
    for v in i.vols.volumes() {
        if v.header.db_creation != i.log.db_creation {
            refusals.push(format!(
                "{}: db_creation {} differs from the log's {}; not one database",
                v.path.display(),
                v.header.db_creation,
                i.log.db_creation
            ));
        }
        volumes.push(VolumePlan {
            volid: v.volid,
            bytes: fs::metadata(&v.path)?.len(),
            copy_method: copy_method::predict(&v.path, i.target_dir, i.allow_clone)?,
            path: v.path.clone(),
        });
    }

    // files: TDE refuses (G1); B-tree pages are rebuilt, so they count toward the target's space
    let (_, boot) = BootDbParm::read(i.vols)?;
    let (items, _) = file_tracker::read(i.vols, boot.trk_vfid)?;
    let mut btree_pages = 0u64;
    let mut heap_pages = std::collections::HashMap::new();
    for it in &items {
        let h = FileHeader::read(&i.vols.read_page(it.vfid.header_vpid())?);
        if h.is_tde_encrypted() {
            refusals.push(format!(
                "file {}|{} is TDE-encrypted (not supported, workspace#481)",
                it.vfid.volid, it.vfid.fileid
            ));
        }
        match h.file_type() {
            Some(FileType::Btree | FileType::BtreeOverflowKey) => {
                btree_pages += h.n_page_user.max(0) as u64
            }
            Some(FileType::Heap | FileType::HeapReuseSlots) => {
                heap_pages.insert((it.vfid.volid, it.vfid.fileid), h.n_page_user.max(0) as u64);
            }
            _ => {}
        }
    }

    // classes: roles and re-encoding only differ across lines (workspace#482)
    let classes = read_classes(i.vols, line)?;
    let class_plans = regenerates.then(|| classify(&classes));
    let mut treatments = Vec::new();
    for (n, c) in classes.iter().enumerate() {
        let name = String::from_utf8_lossy(&c.class.name).into_owned();
        let system = c.class.flags & CLASSFLAG_SYSTEM != 0;
        let p = class_plans.as_ref().map(|ps| &ps[n]);
        let re_encoded = p.is_some_and(|p| p.re_encoded);
        let heap = if c.class.heap.is_null() || c.class.class_type == ClassType::Vclass {
            HeapTreatment::None
        } else {
            match p.map(|p| &p.kind) {
                Some(ClassKind::System(
                    SystemClassRole::Reallocated | SystemClassRole::Dropped,
                )) => HeapTreatment::Freed,
                Some(ClassKind::System(SystemClassRole::SameSlot { .. })) => {
                    HeapTreatment::InPlace(PageTreatment::Transform)
                }
                _ if re_encoded => HeapTreatment::InPlace(PageTreatment::Transform),
                _ => HeapTreatment::InPlace(PageTreatment::Patch),
            }
        };
        if let Some(p) = p {
            refusals.extend(p.refusals.iter().cloned());
            warnings.extend(p.warnings.iter().cloned());
        }
        treatments.push(ClassTreatment {
            oid: c.oid,
            name,
            system,
            re_encoded,
            heap,
        });
    }

    // indexes: plain indexes need a collation this tool orders (workspace#491); function/filter ones are deferred
    for c in classes
        .iter()
        .filter(|c| c.class.flags & CLASSFLAG_SYSTEM == 0)
    {
        let props = class_properties(&c.class.properties, line).map_err(|e| {
            invalid(format!(
                "{} properties: {e}",
                String::from_utf8_lossy(&c.class.name)
            ))
        })?;
        for k in props
            .constraints
            .iter()
            .filter(|k| !(k.has_filter || k.has_function))
        {
            for (id, _) in &k.keys {
                let Some(d) = c
                    .class
                    .attributes
                    .iter()
                    .find(|a| a.id == *id)
                    .and_then(|a| a.domain.first())
                else {
                    continue;
                };
                let string = [
                    DbType::Char,
                    DbType::Varchar,
                    DbType::Nchar,
                    DbType::Varnchar,
                ]
                .iter()
                .any(|t| *t as i32 == d.type_id);
                if string && !Collation::is_supported(d.collation_id) {
                    refusals.push(format!(
                        "index {}.{} has a key of collation id {} (a locale collation this tool cannot order, workspace#491)",
                        String::from_utf8_lossy(&c.class.name),
                        String::from_utf8_lossy(&k.name),
                        d.collation_id
                    ));
                }
            }
        }
    }
    let deferred = deferred_indexes(&classes, line)?;
    let deferred_names: Vec<String> = deferred
        .iter()
        .filter_map(|(c, k)| report_name(c, k))
        .collect();
    let items: Vec<_> = deferred.iter().map(|(c, k)| (*c, k)).collect();
    let deferred_indexes_sql = deferred_indexes_sql(&items, line).map_err(invalid)?;
    for n in &deferred_names {
        warnings.push(format!(
            "deferred index {n}: not built; run fastmig/deferred-indexes.sql on the target (workspace#486)"
        ));
    }

    // catalog regeneration inputs that refuse: collations, default values (regenerates-line only)
    if regenerates {
        let charset = i.vols.volume(0)?.header.db_charset as u8;
        match template_store::builtin(i.target_line, charset) {
            Some(t) => {
                let c = check_collations(&rows_of(i.vols, line, &classes, b"_db_collation")?, &t)
                    .map_err(|e| invalid(format!("_db_collation: {e}")))?;
                refusals.extend(c.refusals);
                warnings.extend(c.warnings);
            }
            None => refusals.push(format!(
                "no catalog template for the target line and charset {charset}"
            )),
        }
        for (c, p) in classes.iter().zip(class_plans.iter().flatten()) {
            if p.kind != ClassKind::User {
                continue;
            }
            let rewritten = rewrite_user_class(
                c,
                p,
                &RewriteInputs {
                    rep_dir: Oid::NULL,
                    new_btid: &|_| None,
                },
            );
            match rewritten {
                Ok(o) => {
                    for a in o
                        .attributes
                        .iter()
                        .chain(&o.shared_attributes)
                        .chain(&o.class_attributes)
                    {
                        if let Err(e) = attr_default::default_value(a) {
                            refusals.push(format!(
                                "{}.{}: default value: {} (workspace#498)",
                                p.name,
                                String::from_utf8_lossy(&a.name),
                                e.what
                            ));
                        }
                    }
                }
                Err(e) => refusals.push(format!("{}: class object rewrite: {}", p.name, e.what)),
            }
        }
        // user SQL text naming 11.4 system views (G1)
        let mut texts: Vec<(String, Vec<u8>)> = Vec::new();
        for c in classes
            .iter()
            .filter(|c| c.class.flags & CLASSFLAG_SYSTEM == 0)
        {
            for q in &c.class.query_specs {
                texts.push((
                    format!("view {}", String::from_utf8_lossy(&c.class.name)),
                    q.clone(),
                ));
            }
        }
        for r in rows_of(i.vols, line, &classes, b"db_trigger")? {
            let name = text(r.values.get("name"))
                .map(String::from_utf8_lossy)
                .unwrap_or_default();
            for col in ["condition", "action_definition"] {
                if let Some(t) = text(r.values.get(col)) {
                    texts.push((format!("trigger {name} {col}"), t.to_vec()));
                }
            }
        }
        for r in rows_of(i.vols, line, &classes, b"_db_stored_procedure_code")? {
            let name = text(r.values.get("name"))
                .map(String::from_utf8_lossy)
                .unwrap_or_default();
            if let Some(t) = text(r.values.get("scode")) {
                texts.push((format!("stored procedure code {name}"), t.to_vec()));
            }
        }
        let v = system_view_refs::check(texts.iter().map(|(o, t)| (o.clone(), t.as_slice())));
        refusals.extend(v.refusals);
        warnings.extend(v.warnings);
    }

    // PL/CSQL routines keep their compiled code; the user may recompile (G6 6)
    let routines = plcsql_routines(&rows_of(i.vols, line, &classes, b"_db_stored_procedure")?);
    if !routines.is_empty() {
        warnings.push(format!(
            "{} PL/CSQL routines keep their compiled code; fastmig/recompile-plcsql.sql recompiles them (workspace#488)",
            routines.len()
        ));
    }

    // out-of-volume assets (G6)
    let lob_base = match lob_base_dir(i.source) {
        Ok(b) => b,
        Err(r) => {
            refusals.push(r);
            None
        }
    };
    let lob = lob_scan::scan(i.vols, line, &classes, lob_base.as_deref())?;
    if lob.absolute_under_base > 0 {
        // shortcut: G6 3 rewrites these records as relative; no source line this tool reads writes them, so the
        // record rewrite is not built until a source has them
        refusals.push(format!(
            "{} LOB locators are absolute paths under the lob-base-path; their rewrite is not built (workspace#488 3)",
            lob.absolute_under_base
        ));
    }
    if !lob.unsupported.is_empty() {
        refusals.push(format!(
            "{} LOB locators of another scheme than file:, e.g. {}",
            lob.unsupported.len(),
            lob.unsupported[0]
        ));
    }
    for f in &lob.outside_base {
        warnings.push(format!(
            "LOB file {} is outside the lob-base-path: neither copied nor deleted, the target opens the same path",
            f.rel_path.display()
        ));
    }
    if !lob.orphans.is_empty() {
        warnings.push(format!(
            "{} orphan LOB files ({} B) under the lob-base-path: not copied, not deleted",
            lob.orphans.len(),
            lob.orphan_bytes()
        ));
    }
    if let Some(base) = &lob_base {
        for other in sharing_lob_base(i.source_dbtxt, &i.source.name, base)? {
            warnings.push(format!(
                "database {other} uses the same lob-base-path {}",
                base.display()
            ));
        }
    }
    let java = dir_size(&i.source.vol_path.join("java"))?;
    let java_static = dir_size(&i.source.vol_path.join("java_static"))?;
    if java.files + java_static.files > 0 {
        warnings.push(format!(
            "Java SP directories: {} files, {} B copied as is (workspace#488 5)",
            java.files + java_static.files,
            java.bytes + java_static.bytes
        ));
    }

    // target space: a copied volume is written whole; a cloned one only where pages change (G1)
    let io_page = i.vols.io_pagesize() as u64;
    let transformed_pages: u64 = classes
        .iter()
        .zip(&treatments)
        .filter(|(_, t)| t.heap == HeapTreatment::InPlace(PageTreatment::Transform))
        .filter_map(|(c, _)| heap_pages.get(&(c.class.heap.vfid.volid, c.class.heap.vfid.fileid)))
        .sum();
    let any_clone = volumes.iter().any(|v| v.copy_method == CopyMethod::Clone);
    let mut needed: u64 = volumes
        .iter()
        .filter(|v| v.copy_method == CopyMethod::Copy)
        .map(|v| v.bytes)
        .sum();
    if any_clone {
        // shortcut: patch target pages are not counted before the heap pass reads them; the rebuilt B-trees and the
        // transformed heaps stand in for the pages a clone has to unshare
        needed += (btree_pages + transformed_pages) * io_page;
    }
    needed += lob.referenced_bytes() + java.bytes + java_static.bytes;
    let available = free_space::available_bytes(i.target_dir)?;
    if needed > available {
        refusals.push(format!(
            "the target filesystem has {available} B free, the migration needs about {needed} B"
        ));
    }

    refusals.dedup();
    Ok(Plan {
        version_pair: pair,
        volumes,
        classes: treatments,
        deferred_indexes: deferred_names,
        deferred_indexes_sql,
        recompile_plcsql_sql: recompile_plcsql_sql(&routines),
        plcsql_routines: routines.len(),
        lob,
        java,
        java_static,
        needed_bytes: needed,
        available_bytes: available,
        refusals,
        warnings,
    })
}

/// Other databases of `dbtxt` whose lob-base-path is `base` (G6 4).
fn sharing_lob_base(dbtxt: &Path, me: &str, base: &Path) -> io::Result<Vec<String>> {
    let text = fs::read_to_string(dbtxt)?;
    Ok(text
        .lines()
        .map(|l| {
            l.split('#')
                .next()
                .unwrap_or("")
                .split_whitespace()
                .collect::<Vec<_>>()
        })
        .filter(|f| f.len() >= 5 && f[0] != me)
        .filter(|f| f[4].strip_prefix("file:").map(Path::new) == Some(base))
        .map(|f| f[0].to_string())
        .collect())
}
