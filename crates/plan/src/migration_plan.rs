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
use fastmig_catalog::owner_qualified_names;
use fastmig_catalog::source_classes::{SourceClass, read_classes};
use fastmig_catalog::sp_target_signature::{TARGET_CLASS_LEN, split_target_signature};
use fastmig_catalog::system_classes::SystemClassRole;
use fastmig_catalog::system_rows::{stored_procedure, stored_procedure_code, trigger};
use fastmig_catalog::system_table::SystemTable;
use fastmig_catalog::{attr_default, template_store};
use fastmig_ondisk::boot_parm::BootDbParm;
use fastmig_ondisk::class_object::{ClassType, class_properties};
use fastmig_ondisk::databases_txt::DatabaseEntry;
use fastmig_ondisk::file_header::{FileHeader, FileType};
use fastmig_ondisk::heap_rewrite::VacuumHorizon;
use fastmig_ondisk::log_header::LogHeader;
use fastmig_ondisk::value::{Collation, DbType};
use fastmig_ondisk::volume_set::VolumeSet;
use fastmig_ondisk::{Hfid, Oid, SourceVersion, VersionLine, file_tracker};

use crate::copy_method::{self, CopyMethod};
use crate::free_space;
use crate::lob_scan::{self, LobScan};
use crate::system_view_refs;
use crate::version_pair::VersionPair;

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
    /// The release that wrote the source; `version_pair.source` is the codec line it is read with (workspace#529).
    pub source_version: SourceVersion,
    pub version_pair: VersionPair,
    pub volumes: Vec<VolumePlan>,
    pub classes: Vec<ClassTreatment>,
    /// Pages the page-treatment phase walks (its progress total, workspace#508): every page of a heap whose chain
    /// is walked — a re-encoded user class always, any other treated heap only when the source had vacuum work
    /// pending ([`fastmig_ondisk::heap_rewrite::rewrite_heap`]).
    pub treated_pages: u64,
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
    /// Heap pages the run may rewrite in place, an upper bound (workspace#526): what a cloned volume must unshare
    /// besides the new B-trees — every heap page when the catalog is regenerated (11.4 → develop), else the
    /// treated pages and the root heap (class objects get the rebuilt BTIDs).
    pub rewritten_pages_bound: u64,
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

/// The refusals of a source the plan cannot lay out (its version has no codec line yet), as the error of
/// [`build`]; the CLI reports them like a plan's refusals.
#[derive(Debug)]
pub struct Refused(pub Vec<String>);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the plan refuses the migration:\n  {}",
            self.0.join("\n  ")
        )
    }
}

impl std::error::Error for Refused {}

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
    table: SystemTable,
) -> io::Result<Vec<Row>> {
    let name = table.source_name().map(str::as_bytes);
    match classes
        .iter()
        .find(|c| Some(c.class.name.as_slice()) == name)
    {
        Some(c) => read_rows(vs, line, c),
        None => Ok(Vec::new()),
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
    let version =
        SourceVersion::identify(i.log.db_compatibility, &i.log.db_release).ok_or_else(|| {
            invalid(format!(
                "unknown source: db_compatibility {} db_release {:?}",
                i.log.db_compatibility, i.log.db_release
            ))
        })?;
    // names that stop being valid once owner-qualified (workspace#518): checked before the version gate, so a source
    // whose version is not opened yet still shows every refusal in one plan
    let mut refusals = owner_qualified_names::refusals(i.vols, version).unwrap_or_else(|e| {
        vec![format!(
            "source names unreadable for the owner-qualified name pre-check: {e} (workspace#518)"
        )]
    });
    let line = match crate::version_pair::source_line(version) {
        Ok(l) => l,
        Err(e) => {
            refusals.insert(0, e);
            return Err(io::Error::other(Refused(refusals)));
        }
    };
    let pair = VersionPair {
        source: line,
        target: i.target_line,
    };
    let regenerates = pair.regenerates_catalog();
    refusals.extend(pair.refusal());
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

    // files: TDE refuses (G1; not supported, user decision 2026-10-30); B-tree pages are rebuilt, so they count
    // toward the target's space. The file header flag covers every encrypted permanent page: the engine sets a
    // page's TDE pflag only from it (file_manager.c file_alloc, file_apply_tde_algorithm), so no page scan is needed.
    let (_, boot) = BootDbParm::read(i.vols)?;
    let (items, _) = file_tracker::read(i.vols, boot.trk_vfid)?;
    let mut btree_pages = 0u64;
    let mut heap_pages = std::collections::HashMap::new();
    for it in &items {
        let h = FileHeader::read(&i.vols.read_page(it.vfid.header_vpid())?);
        if h.is_tde_encrypted() {
            refusals.push(format!(
                "TDE-encrypted source is not supported: file {}|{} is encrypted (workspace#510)",
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
    if matches!(version, SourceVersion::V11_2 | SourceVersion::V11_3) {
        // a pre-11.4 Java target must split into develop's target_class/target_method (R12 § 5.1 2)
        for r in rows_of(i.vols, line, &classes, SystemTable::DbStoredProcedure)? {
            let name =
                String::from_utf8_lossy(stored_procedure::sp_name(&r.values).unwrap_or_default())
                    .into_owned();
            let target = stored_procedure::target(&r.values).unwrap_or_default();
            match split_target_signature(target) {
                None => refusals.push(format!(
                    "stored procedure {name}: target {:?} has no Class.method(..) (workspace#529)",
                    String::from_utf8_lossy(target)
                )),
                Some((c, _)) if c.len() > TARGET_CLASS_LEN => refusals.push(format!(
                    "stored procedure {name}: target class over {TARGET_CLASS_LEN} bytes (workspace#529)"
                )),
                Some(_) => {}
            }
        }
    }
    let class_plans = regenerates.then(|| classify(&classes));
    let mut treatments = Vec::new();
    for (n, c) in classes.iter().enumerate() {
        let name = String::from_utf8_lossy(&c.class.name).into_owned();
        let system = c.class.is_system_class();
        if c.class.tde_algorithm != 0 {
            // a class created ENCRYPT=AES|ARIA, even one whose files are already gone (R9 workspace#510)
            refusals.push(format!(
                "TDE-encrypted source is not supported: class {name} has tde_algorithm {} (workspace#510)",
                c.class.tde_algorithm
            ));
        }
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
    for c in classes.iter().filter(|c| !c.class.is_system_class()) {
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
                let c = check_collations(
                    &rows_of(i.vols, line, &classes, SystemTable::DbCollation)?,
                    &t,
                )
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
        for c in classes.iter().filter(|c| !c.class.is_system_class()) {
            for q in &c.class.query_specs {
                texts.push((
                    format!("view {}", String::from_utf8_lossy(&c.class.name)),
                    q.clone(),
                ));
            }
        }
        for r in rows_of(i.vols, line, &classes, SystemTable::DbTrigger)? {
            let name = trigger::name(&r.values)
                .map(String::from_utf8_lossy)
                .unwrap_or_default();
            let columns = [
                ("condition", trigger::condition(&r.values)),
                ("action_definition", trigger::action_definition(&r.values)),
            ];
            for (col, text) in columns {
                if let Some(t) = text {
                    texts.push((format!("trigger {name} {col}"), t.to_vec()));
                }
            }
        }
        for r in rows_of(i.vols, line, &classes, SystemTable::DbStoredProcedureCode)? {
            let name = stored_procedure_code::name(&r.values)
                .map(String::from_utf8_lossy)
                .unwrap_or_default();
            if let Some(t) = stored_procedure_code::scode(&r.values) {
                texts.push((format!("stored procedure code {name}"), t.to_vec()));
            }
        }
        let v = system_view_refs::check(texts.iter().map(|(o, t)| (o.clone(), t.as_slice())));
        refusals.extend(v.refusals);
        warnings.extend(v.warnings);
    }

    // PL/CSQL routines keep their compiled code; the user may recompile (G6 6)
    let routines = plcsql_routines(&rows_of(
        i.vols,
        line,
        &classes,
        SystemTable::DbStoredProcedure,
    )?);
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
    let lob = lob_scan::scan(i.vols, line, &classes, lob_base.as_deref(), regenerates)?;
    // 11.4 → develop re-encodes LOB classes and rewrites these locators (workspace#507); the same-version path
    // copies pages as they are
    if lob.absolute_under_base > 0 && !regenerates {
        // shortcut: the same-version path has no record rewrite; 11.4 written by unloaddb/loaddb from 11.3 or
        // older has these (workspace#507), a develop source would need it
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

    // page-treatment progress total: the heaps whose chains rewrite_heap walks (11.4 path: user heaps only, the
    // system ones belong to the catalog regeneration; same-version path: every class heap and the root heap)
    let pending = VacuumHorizon::read(i.vols, i.log, boot.vacuum_data_vfid)?.pending;
    let pages_of = |h: Hfid| heap_pages.get(&(h.vfid.volid, h.vfid.fileid)).copied();
    let rewritten_pages_bound = if regenerates {
        heap_pages.values().sum()
    } else {
        root_heap_pages(&heap_pages, boot.rootclass_hfid)
    };
    let mut treated_pages: u64 = classes
        .iter()
        .zip(&treatments)
        .filter(|(_, t)| matches!(t.heap, HeapTreatment::InPlace(_)))
        .filter(|(_, t)| {
            if regenerates {
                !t.system && (t.re_encoded || pending)
            } else {
                pending
            }
        })
        .filter_map(|(c, _)| pages_of(c.class.heap))
        .sum();
    if !regenerates && pending {
        treated_pages += pages_of(boot.rootclass_hfid).unwrap_or(0);
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
        source_version: version,
        version_pair: pair,
        volumes,
        classes: treatments,
        treated_pages,
        deferred_indexes: deferred_names,
        deferred_indexes_sql,
        recompile_plcsql_sql: recompile_plcsql_sql(&routines),
        plcsql_routines: routines.len(),
        lob,
        java,
        java_static,
        rewritten_pages_bound: rewritten_pages_bound + if regenerates { 0 } else { treated_pages },
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

/// Pages of the root heap (its class objects are rewritten with the rebuilt BTIDs on every path).
fn root_heap_pages(heap_pages: &std::collections::HashMap<(i16, i32), u64>, root: Hfid) -> u64 {
    heap_pages
        .get(&(root.vfid.volid, root.vfid.fileid))
        .copied()
        .unwrap_or(0)
}
