//! The migration plan: everything decided before a byte is written — the version pair, each volume's copy method,
//! each class's page treatment grade, the deferred indexes, the LOB manifest, and the pre-check's refusals and
//! warnings and the system name rewrites. `plan` prints it; `migrate` builds the same plan, refuses on any refusal, and follows it.
//!
//! Input: [`PlanInputs`] (the open source, its log header, the target directory and choices). Output: [`Plan`].
//! Source: decision G1 workspace#481 (plan command, pre-check refusals and warnings, copy method × page treatment
//! grade), G2 workspace#482 (re-encoded classes, collations, NCHAR), G5 workspace#486 (deferred indexes), G6
//! workspace#488 (LOB scan, Java directories, lob-base-path rules), workspace#498 (`attr_default` refusals),
//! workspace#491 (LDML collations are not ordered here), workspace#505 (this assembly).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use fastmig_catalog::class_rewrite::{RewriteInputs, rewrite_user_class};
use fastmig_catalog::class_rewrite_11_4::rewrite_user_class_11_4;
use fastmig_catalog::class_rows::{Row, read_rows};
use fastmig_catalog::classify::{ClassKind, classify};
use fastmig_catalog::collation_check::check_collations;
use fastmig_catalog::deferred_indexes::{deferred_indexes, report_name};
use fastmig_catalog::identifier_lower::IdentifierCasing;
use fastmig_catalog::output_sql::{deferred_indexes_sql, plcsql_routines, recompile_plcsql_sql};
use fastmig_catalog::owner_qualification::qualify_names;
use fastmig_catalog::owner_qualified_names;
use fastmig_catalog::source_classes::{SourceClass, read_classes};
use fastmig_catalog::sp_target_signature::target_refusal;
use fastmig_catalog::stored_texts::map_stored_texts;
use fastmig_catalog::system_classes::SystemClassRole;
use fastmig_catalog::system_name_mapping::judge;
use fastmig_catalog::system_rows::{stored_procedure, stored_procedure_code, text, user};
use fastmig_catalog::system_table::SystemTable;
use fastmig_catalog::{attr_default, template_store};
use fastmig_ondisk::boot_parm::BootDbParm;
use fastmig_ondisk::class_object::{ClassType, class_properties};
use fastmig_ondisk::databases_txt::DatabaseEntry;
use fastmig_ondisk::file_header::{FileDescriptor, FileHeader, FileType};
use fastmig_ondisk::file_table::FileTable;
use fastmig_ondisk::heap_rewrite::VacuumHorizon;
use fastmig_ondisk::log_header::LogHeader;
use fastmig_ondisk::record::bestspace_page;
use fastmig_ondisk::sector_table::SECTOR_NPAGES;
use fastmig_ondisk::value::{Collation, DbType};
use fastmig_ondisk::volume_set::VolumeSet;
use fastmig_ondisk::{Hfid, Oid, SourceVersion, VersionLine, Vfid, Vpid, Vsid, file_tracker};

use crate::compare_rule_mismatch::{self, OldRules, OwnerNames};
use crate::copy_method::{self, CopyMethod};
use crate::execute_grants::{ExecuteGrants, execute_grants};
use crate::free_space;
use crate::keyinfo_heap_room::Volume0Room;
use crate::lob_scan::{self, LobScan};
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
    /// The source's server ran with `ignore_trailing_space=yes` (its `cubrid.conf`, workspace#541).
    pub source_ignores_trailing_space: bool,
    /// The target's server would run with `ignore_trailing_space=yes`.
    pub target_ignores_trailing_space: bool,
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

/// Pages `first .. first + pages` of volume `volid`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageRange {
    pub volid: i16,
    pub first: i32,
    pub pages: u32,
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
    /// `<dest>/fastmig/post-migration-fixes.sql`: the target-side fixes of the compare rule mismatches (workspace#541)
    /// and the EXECUTE grants other-owner procedure calls need (workspace#545); empty when there is none.
    pub post_migration_fixes_sql: String,
    /// `<dest>/fastmig/recompile-plcsql.sql`.
    pub recompile_plcsql_sql: String,
    pub plcsql_routines: usize,
    pub lob: LobScan,
    pub java: DirSize,
    pub java_static: DirSize,
    /// Pages the run may rewrite in place, an upper bound (workspace#526): what a cloned volume must unshare besides
    /// the new B-trees — every live heap page when the catalog is regenerated (11.4 → develop), else the treated
    /// pages and the root heap (class objects get the rebuilt BTIDs); and the pages single-page deallocations retype
    /// (every overflow heap page, the file tracker, the vacuum data file).
    pub rewritten_pages_bound: u64,
    /// The target can unshare a clone's blocks (XFS) and a volume is cloned: `unshare_ranges` are taken at the
    /// start and the rewritten pages leave the reservation (workspace#526).
    pub unshare: bool,
    /// Pages of cloned volumes the run rewrites in place: the allocated pages of the files above, whole sectors of the
    /// files that take new pages inside their sectors (heaps of re-encoded and system classes, the root heap, their
    /// overflow heaps, the tracker), merged.
    pub unshare_ranges: Vec<PageRange>,
    /// Pages of cloned volumes whose content the run does not need but may write: the unallocated pages in the
    /// sectors of the files above that take new pages; given blocks without a copy at the start, merged.
    pub zero_ranges: Vec<PageRange>,
    /// Sectors of the files the run frees before it allocates: every B-tree, overflow key and catalog file; with a
    /// regenerated catalog also the heaps and overflow heaps of the reallocated and dropped system classes.
    pub freed_sectors: Vec<Vsid>,
    /// Sectors the new catalog file and system heaps take at most: one per new file (the catalog file; with a
    /// regenerated catalog a heap per develop system class whose slot is not kept), plus the sectors the files they
    /// replace held past their first.
    pub catalog_sectors: u64,
    /// Free bytes of the target filesystem (the need is `cubrid-fastmig` `target_space`, with the resource estimate).
    pub available_bytes: u64,
    /// Non-empty means migrate does not start.
    /// Old system class names replaced in view and trigger text: `<object>: <from> -> <to> at byte <n>`
    /// (workspace#542).
    pub system_name_rewrites: Vec<String>,
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

/// `post-migration-fixes.sql` from its non-empty sections, under one head.
fn post_migration_fixes_sql(sections: &[&str]) -> String {
    let sections: Vec<&str> = sections.iter().copied().filter(|s| !s.is_empty()).collect();
    if sections.is_empty() {
        return String::new();
    }
    let mut sql = String::from(
        "-- cubrid-fastmig post-migration fixes. Run on the target as DBA after reviewing each statement.\n",
    );
    for s in sections {
        sql.push('\n');
        sql.push_str(s);
    }
    sql
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// The refusals of a source the plan cannot lay out (its version has no codec line yet), as the error of
/// [`build`], with the warnings found before the plan stopped; the CLI reports them like a plan's.
#[derive(Debug)]
pub struct Refused {
    pub refusals: Vec<String>,
    pub warnings: Vec<String>,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the plan refuses the migration:\n  {}",
            self.refusals.join("\n  ")
        )?;
        if !self.warnings.is_empty() {
            write!(f, "\nwarnings:\n  {}", self.warnings.join("\n  "))?;
        }
        Ok(())
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

/// Owner name prefixes by `db_user` row OID, lowercased as the engine does: the target names of a source before user
/// schema start with them (workspace#511, #544).
fn owner_names(
    vs: &VolumeSet,
    line: VersionLine,
    classes: &[SourceClass],
) -> io::Result<OwnerNames> {
    let lang = rows_of(vs, line, classes, SystemTable::DbRoot)?
        .into_iter()
        .find_map(|r| text(r.values.get("lang")).map(<[u8]>::to_vec))
        .unwrap_or_default();
    let casing = IdentifierCasing::of(vs.volume(0)?.header.db_charset as u8, &lang);
    Ok(rows_of(vs, line, classes, SystemTable::DbUser)?
        .into_iter()
        .filter_map(|r| user::name(&r.values).map(|n| (r.oid, casing.lower(n))))
        .collect())
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
            return Err(io::Error::other(Refused {
                refusals,
                warnings: Vec::new(),
            }));
        }
    };
    let pair = VersionPair {
        source: line,
        target: i.target_line,
    };
    let regenerates = pair.regenerates_catalog(version);
    refusals.extend(pair.refusal(version));
    // the tool orders VARCHAR keys with trailing spaces counted; a target comparing without them reads its indexes
    // out of order (G10 workspace#541: refused, not a warning)
    if i.target_ignores_trailing_space {
        refusals.push(format!(
            "the target cubrid.conf sets {}=yes: the rebuilt VARCHAR indexes count trailing spaces, so that server \
             would read them out of order; set it to no for the target (workspace#541)",
            crate::cubrid_conf::PARAMETER
        ));
    }
    // classes: roles and re-encoding only differ across lines (workspace#482)
    let classes = read_classes(i.vols, line)?;
    // rows placed or matched under the source's older string rules: warned before the version gate, so a 10.2 plan
    // shows them too (G10 workspace#541)
    let owners = match version {
        SourceVersion::V10_2 | SourceVersion::V11_0 => owner_names(i.vols, line, &classes)?,
        _ => OwnerNames::new(),
    };
    let mismatch = compare_rule_mismatch::check(
        i.vols,
        line,
        &classes,
        &owners,
        OldRules {
            hash_space_weight_0: version == SourceVersion::V10_2,
            trailing_space_ignored: version == SourceVersion::V10_2
                || i.source_ignores_trailing_space,
        },
    )?;
    let mut warnings = mismatch.warnings;
    // the TDE keyinfo heap a 10.2 source gets must find a sector in volume 0 (workspace#515, #550)
    if version == SourceVersion::V10_2 {
        let room = Volume0Room::read(i.vols)?;
        let line = format!(
            "TDE keyinfo heap: volume 0 has {} free sectors now and {} more once the old B-trees are freed; it takes 1 \
             (workspace#550)",
            room.free, room.freed_btree
        );
        if room.total() == 0 {
            refusals.push(line);
        } else {
            warnings.push(line);
        }
    }
    let mut system_name_rewrites = Vec::new();

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

    // files: TDE refuses (G1; not supported, user decision 2026-10-30); heap page counts bound the rewritten pages.
    // The file header flag covers every encrypted permanent page: the engine sets a page's TDE pflag only from it
    // (file_manager.c file_alloc, file_apply_tde_algorithm), so no page scan is needed.
    let (_, boot) = BootDbParm::read(i.vols)?;
    let (items, _) = file_tracker::read(i.vols, boot.trk_vfid)?;
    let mut heap_pages = std::collections::HashMap::new();
    // pages the run's single-page deallocations retype (`perm_dealloc`: the overflow chains a rewritten record
    // leaves, the tracker pages a removed entry merges): every overflow heap page and the tracker's (workspace#526;
    // `destroy_file` leaves a freed file's pages as they are, workspace#536)
    let mut retyped_pages = 0u64;
    let mut freed_files: Vec<Vfid> = Vec::new();
    // heap and overflow heap files by class (overflow: true): freed when the class's heap is (`free_old_files`)
    let mut class_files: Vec<(Vfid, Oid, bool)> = Vec::new();
    // the catalog file and the freed class heaps: the regenerated catalog writes their content into new files
    let mut replaced_files: Vec<Vfid> = Vec::new();
    for it in &items {
        let h = FileHeader::read(&i.vols.read_page(it.vfid.header_vpid())?);
        if h.is_tde_encrypted() {
            refusals.push(format!(
                "TDE-encrypted source is not supported: file {}|{} is encrypted (workspace#510)",
                it.vfid.volid, it.vfid.fileid
            ));
        }
        if matches!(
            h.file_type(),
            Some(FileType::Heap | FileType::HeapReuseSlots)
        ) {
            heap_pages.insert((it.vfid.volid, it.vfid.fileid), h.n_page_user.max(0) as u64);
        }
        let (user, ftab) = (h.n_page_user.max(0) as u64, h.n_page_ftab.max(0) as u64);
        match (h.file_type(), h.descriptor()) {
            (Some(FileType::Btree | FileType::BtreeOverflowKey), _) => freed_files.push(it.vfid),
            (Some(FileType::Catalog), _) => replaced_files.push(it.vfid),
            (
                Some(FileType::Heap | FileType::HeapReuseSlots),
                FileDescriptor::Heap { class_oid, .. },
            ) => class_files.push((it.vfid, class_oid, false)),
            (
                Some(FileType::MultipageObjectHeap),
                FileDescriptor::HeapOverflow { class_oid, .. },
            ) => {
                class_files.push((it.vfid, class_oid, true));
                retyped_pages += user + ftab;
            }
            _ => {}
        }
    }
    let trk = FileHeader::read(&i.vols.read_page(boot.trk_vfid.header_vpid())?);
    retyped_pages += (trk.n_page_user.max(0) + trk.n_page_ftab.max(0)) as u64;
    // the vacuum data file: its first page is emptied in place and every later page deallocated (workspace#551)
    let vacuum_data = FileHeader::read(&i.vols.read_page(boot.vacuum_data_vfid.header_vpid())?);
    retyped_pages += (vacuum_data.n_page_user.max(0) + vacuum_data.n_page_ftab.max(0)) as u64;

    if matches!(
        version,
        SourceVersion::V10_2 | SourceVersion::V11_2 | SourceVersion::V11_3
    ) {
        // a pre-11.4 Java target must split into the target's target_class/target_method (R12 § 5.1 2); an 11.4
        // target stops its schema load on a method still over 1024 after shortening (T15 V3, workspace#534)
        for r in rows_of(i.vols, line, &classes, SystemTable::DbStoredProcedure)? {
            let name =
                String::from_utf8_lossy(stored_procedure::sp_name(&r.values).unwrap_or_default())
                    .into_owned();
            let target = stored_procedure::target(&r.values).unwrap_or_default();
            if let Some(why) = target_refusal(target, i.target_line) {
                refusals.push(format!(
                    "stored procedure {name}: target {:?} {why} (workspace#529, workspace#534)",
                    String::from_utf8_lossy(target)
                ));
            }
        }
    }
    let class_plans = regenerates.then(|| classify(&classes, line, i.target_line));
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

    let mut new_heaps_bound = 0usize;
    let mut grants = ExecuteGrants::default();
    // catalog regeneration inputs that refuse: collations, default values (regenerates-line only)
    if regenerates {
        let charset = i.vols.volume(0)?.header.db_charset as u8;
        match template_store::builtin(i.target_line, charset) {
            Some(t) => {
                // reallocated and new system tables get new heaps: every template class but those keeping their slot
                let same_slot = class_plans
                    .iter()
                    .flatten()
                    .filter(|p| {
                        matches!(p.kind, ClassKind::System(SystemClassRole::SameSlot { .. }))
                    })
                    .count();
                new_heaps_bound = t.classes.len().saturating_sub(same_slot);
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
            let rewritten = match i.target_line {
                VersionLine::Develop => rewrite_user_class(
                    c,
                    p,
                    &RewriteInputs {
                        from: line,
                        rep_dir: Oid::NULL,
                        new_btid: &|_| None,
                    },
                ),
                VersionLine::V10_2 | VersionLine::V11_4 => {
                    rewrite_user_class_11_4(c, p, line, Oid::NULL, &|_| None)
                }
            };
            match rewritten {
                Ok(o) => {
                    for a in o
                        .attributes
                        .iter()
                        .chain(&o.shared_attributes)
                        .chain(&o.class_attributes)
                    {
                        if let Err(e) = attr_default::default_value(a, i.target_line) {
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
        // user SQL text naming old system classes: views and triggers get the develop names, the rest refuses or
        // warns (workspace#542); the engine recompiles view and trigger text, so only that text is rewritten
        let mut mapped = classes.clone();
        let users = rows_of(i.vols, line, &classes, SystemTable::DbUser)?;
        let mut triggers = rows_of(i.vols, line, &classes, SystemTable::DbTrigger)?;
        for (object, j) in
            map_stored_texts(version, i.target_line, &mut mapped, &users, &mut triggers)
        {
            for r in &j.renames {
                system_name_rewrites
                    .push(format!("{object}: {} -> {} at byte {}", r.from, r.to, r.at));
            }
            refusals.extend(j.refusals.iter().map(|r| {
                format!("{object} {r}; fix: recreate it on the source without that name, then migrate again")
            }));
            warnings.extend(j.warnings.iter().map(|w| {
                format!(
                    "{object} {w}; fix: recreate it on the target if it must read the renamed table"
                )
            }));
        }
        // a source before user schema: its texts get owner-qualified names; a call of another owner's procedure needs
        // EXECUTE on develop (workspace#544, #545)
        let mut owned: HashMap<SystemTable, Vec<Row>> = HashMap::new();
        for t in [
            SystemTable::DbRoot,
            SystemTable::DbSerial,
            SystemTable::DbStoredProcedure,
        ] {
            owned.insert(t, rows_of(i.vols, line, &classes, t)?);
        }
        owned.insert(SystemTable::DbUser, users);
        owned.insert(SystemTable::DbTrigger, triggers);
        let charset = i.vols.volume(0)?.header.db_charset as u8;
        grants = execute_grants(
            &qualify_names(version, &mut mapped, &mut owned, charset)
                .map_err(|e| invalid(format!("owner-qualified names: {}", e.what)))?,
        );
        warnings.extend(grants.warning.clone());
        // PL/CSQL keeps its compiled code, which develop cannot recompile from a rewritten source (workspace#542)
        for r in rows_of(i.vols, line, &classes, SystemTable::DbStoredProcedureCode)? {
            let name = stored_procedure_code::name(&r.values)
                .map(String::from_utf8_lossy)
                .unwrap_or_default();
            let Some(t) = stored_procedure_code::scode(&r.values) else {
                continue;
            };
            let j = judge(version, i.target_line, t, false);
            let object = format!("stored procedure code {name}");
            refusals.extend(j.renames.iter().map(|r| {
                format!(
                    "{object} names system class {}, which develop names {}; its compiled code is copied as is; \
                     fix: drop it on the source, migrate, then create it on the target with {}",
                    r.from, r.to, r.to
                )
            }));
            refusals.extend(j.refusals.iter().map(|r| {
                format!("{object} {r}; fix: drop it on the source, migrate, then create it on the target")
            }));
            warnings.extend(j.warnings.iter().map(|w| {
                format!(
                    "{object} {w}; fix: on the target, edit its source before running fastmig/recompile-plcsql.sql"
                )
            }));
        }
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
    // develop keeps LOB files per (heap, attribute) directory; an 11.4 target keeps them at their path below the base
    let develop_dirs = regenerates && i.target_line == VersionLine::Develop;
    let lob = lob_scan::scan(i.vols, line, &classes, lob_base.as_deref(), develop_dirs)?;
    // a regenerating run re-encodes LOB classes and rewrites these locators (workspace#507, #552); the same-version path
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
    let freed_classes: HashSet<Oid> = treatments
        .iter()
        .filter(|t| t.heap == HeapTreatment::Freed)
        .map(|t| t.oid)
        .collect();
    let live_heap = |(_, c, _): &&(Vfid, Oid, bool)| !freed_classes.contains(c);
    replaced_files.extend(
        class_files
            .iter()
            .filter(|f| !live_heap(f))
            .map(|(v, _, _)| *v),
    );
    // the heaps the run rewrites in place: every live heap when the catalog is regenerated, else every heap but the
    // user classes' (catalog rows, boot parameters), the root heap and, with vacuum work pending, the treated heaps; and every live overflow heap,
    // the tracker and the vacuum data file (retyped pages)
    let user: HashSet<Oid> = treatments
        .iter()
        .filter(|t| !t.system)
        .map(|t| t.oid)
        .collect();
    let mut rewritten_files: Vec<Vfid> = vec![boot.trk_vfid, boot.vacuum_data_vfid];
    rewritten_files.extend(
        class_files
            .iter()
            .filter(live_heap)
            .filter(|(_, c, ovf)| regenerates || *ovf || !user.contains(c))
            .map(|(v, _, _)| *v),
    );
    let rewritten_pages_bound = if regenerates {
        // every live heap page, and the bestspace pages each heap gets after its header
        let bestspace = bestspace_page::page_count(i.vols.io_pagesize()) as u64;
        class_files
            .iter()
            .filter(live_heap)
            .filter_map(|(v, _, _)| heap_pages.get(&(v.volid, v.fileid)))
            .map(|p| p + bestspace)
            .sum()
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
    if !regenerates {
        rewritten_files.push(boot.rootclass_hfid.vfid);
        if pending {
            rewritten_files.extend(
                classes
                    .iter()
                    .zip(&treatments)
                    .filter(|(_, t)| matches!(t.heap, HeapTreatment::InPlace(_)))
                    .map(|(c, _)| c.class.heap.vfid),
            );
        }
    }
    // the pages they hold on cloned volumes, as ranges: taken at the start when the target can unshare
    let cloned: Vec<i16> = volumes
        .iter()
        .filter(|v| v.copy_method == CopyMethod::Clone)
        .map(|v| v.volid)
        .collect();
    let unshare = !cloned.is_empty() && free_space::can_unshare(i.target_dir)?;
    // sectors the run frees before it allocates: free to the sector sequence the target space bound previews
    let mut freed_sectors: Vec<Vsid> = Vec::new();
    // the new catalog file and heaps: one sector each new file, plus what the replaced files held past their first
    let mut catalog_sectors = 1 + new_heaps_bound as u64;
    for vfid in &replaced_files {
        let s = FileTable::read(i.vols, *vfid)?.sectors;
        catalog_sectors += (s.len() as u64).saturating_sub(1);
        freed_sectors.extend(s.iter().map(|s| s.vsid));
    }
    for vfid in &freed_files {
        freed_sectors.extend(
            FileTable::read(i.vols, *vfid)?
                .sectors
                .iter()
                .map(|s| s.vsid),
        );
    }
    let mut pages: Vec<Vpid> = Vec::new();
    // pages whose content the run does not need: given blocks of their own without a copy (`preallocate::zero`)
    let mut zero_pages: Vec<Vpid> = Vec::new();
    if unshare {
        rewritten_files.sort();
        rewritten_files.dedup();
        // files that take new pages in their partial sectors, where no sector is reserved to claim: on the 11.x path
        // every live heap (develop bestspace pages after its header, re-encoded rows, catalog rows), else the heaps of
        // system classes and the root class (catalog rows); their overflow heaps, and the tracker; whole sectors
        let fixed: HashSet<Oid> = treatments
            .iter()
            .filter(|t| !regenerates && !t.system)
            .map(|t| t.oid)
            .collect();
        let mut appending: HashSet<Vfid> = class_files
            .iter()
            .filter(|(_, c, _)| !fixed.contains(c))
            .map(|(v, _, _)| *v)
            .collect();
        appending.insert(boot.trk_vfid);
        for vfid in &rewritten_files {
            let t = FileTable::read(i.vols, *vfid)?;
            if appending.contains(vfid) {
                let allocated = t.allocated_pages();
                for s in t.sectors.iter().filter(|s| cloned.contains(&s.vsid.volid)) {
                    let first = s.vsid.sectid * SECTOR_NPAGES;
                    for p in (first..first + SECTOR_NPAGES).map(|p| Vpid::new(s.vsid.volid, p)) {
                        if allocated.contains(&p) {
                            pages.push(p);
                        } else {
                            zero_pages.push(p);
                        }
                    }
                }
            } else {
                pages.extend(
                    t.allocated_pages()
                        .into_iter()
                        .filter(|p| cloned.contains(&p.volid)),
                );
            }
        }
    }
    let unshare_ranges = page_ranges(pages);
    let zero_ranges = page_ranges(zero_pages);

    // what the target needs is bounded with the resource estimate (cli target_space, workspace#526)
    let available = free_space::available_bytes(i.target_dir)?;

    refusals.dedup();
    Ok(Plan {
        source_version: version,
        version_pair: pair,
        volumes,
        classes: treatments,
        treated_pages,
        deferred_indexes: deferred_names,
        deferred_indexes_sql,
        post_migration_fixes_sql: post_migration_fixes_sql(&[
            &mismatch.post_migration_fixes_sql,
            &grants.fix_sql,
        ]),
        recompile_plcsql_sql: recompile_plcsql_sql(&routines),
        plcsql_routines: routines.len(),
        lob,
        java,
        java_static,
        unshare,
        unshare_ranges,
        zero_ranges,
        freed_sectors,
        catalog_sectors,
        rewritten_pages_bound: rewritten_pages_bound
            + if regenerates { 0 } else { treated_pages }
            + retyped_pages,
        available_bytes: available,
        system_name_rewrites,
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

/// `pages` sorted, as runs of consecutive pages of a volume.
fn page_ranges(mut pages: Vec<Vpid>) -> Vec<PageRange> {
    pages.sort();
    pages.dedup();
    let mut out: Vec<PageRange> = Vec::new();
    for p in pages {
        match out.last_mut() {
            Some(r) if r.volid == p.volid && r.first + r.pages as i32 == p.pageid => r.pages += 1,
            _ => out.push(PageRange {
                volid: p.volid,
                first: p.pageid,
                pages: 1,
            }),
        }
    }
    out
}
