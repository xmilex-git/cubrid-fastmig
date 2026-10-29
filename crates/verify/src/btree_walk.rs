//! Check 2 (structure): reads one B-tree top-down, checks the structural invariants, and streams its live
//! (key, OID) pairs in key-then-OID order for the index ↔ heap merge.
//!
//! Input: the database and the index ([`IndexTree::open`]: root header, key domain from the root's packed domain).
//! Output: [`IndexTree`] with the leaf list (from the non-leaf levels) and check 2 defects found on the way down;
//! [`LeafPairs`] streams the leaves and adds the leaf-level defects; at the end [`LeafPairs::finish`] compares the
//! root statistics.
//! Invariants (G3 Q2 (b)): node level per depth; non-leaf `max_key_len` ≥ its separators; every leaf reached once
//! from the root, leaves doubly linked in that order with NULL ends; keys strictly increasing within and across
//! leaves; a leaf's separator `sep` with `last key of the left leaf < sep ≤ first key`; leaf record flags match the
//! record tail (OVERFLOW_OIDS/KEY, CLASS_OID only on a unique record with overflow OIDs, in a target); overflow
//! OID pages (8 B legacy header in a unique index, 16 B + directory otherwise, in a target; directory entries =
//! data pages with their first OID); unique root statistics = counted objects and keys, non-unique -1;
//! `ovfid` set iff a key overflowed; in a target `rev_level`/`creator_mvccid` 0 and objects in the fixed form only
//! (INSID 3 / DELID 0) where the engine's form is fixed size. Fill rules of the builder are not checked; 11.4
//! fence keys and prefix-compressed leaves are read. In a source, objects with a delete MVCCID are not live; leaf
//! records written by DML keep OIDs in insertion order, so each key's OIDs are sorted before they are streamed.
//! Source: G3 workspace#483 Q2, Q3; R6 § 3 (layouts, ondisk::btree); scratch walker of workspace#493.

use std::cmp::Ordering;

use fastmig_ondisk::btree::be_field::LEAF_RECORD_CLASS_OID;
use fastmig_ondisk::btree::leaf_record::{BtObject, KeyRef, read_leaf_record, read_object};
use fastmig_ondisk::btree::node_header::NodeHeader;
use fastmig_ondisk::btree::nonleaf_record::read_nonleaf_record;
use fastmig_ondisk::btree::ovf_key::read_key;
use fastmig_ondisk::btree::ovf_oid_page::{OvfDirEntry, OvfHeader};
use fastmig_ondisk::btree::root_header::RootHeader;
use fastmig_ondisk::btree::{DISK_VPID_SIZE, MVCCID_ALL_VISIBLE};
use fastmig_ondisk::page;
use fastmig_ondisk::spage::SlottedPage;
use fastmig_ondisk::value::KeyDomain;
use fastmig_ondisk::{Btid, Oid, Vpid};

use crate::database::{Database, Role};
use crate::defect::{Check, Defect, OidText, VpidText};

/// One B-tree as its root page describes it.
pub struct IndexTree<'a> {
    pub db: &'a Database,
    pub class: String,
    pub name: String,
    pub btid: Btid,
    pub root: RootHeader,
    pub kd: KeyDomain,
    /// Leaves in level order, each with the separator its parent record carries.
    pub leaves: Vec<(Vpid, Option<Vec<u8>>)>,
    pub defects: Vec<Defect>,
}

fn slotted(page: &[u8]) -> Option<SlottedPage<'_>> {
    SlottedPage::new(page::db_area(page))
}

impl<'a> IndexTree<'a> {
    /// Reads the root and the non-leaf levels. `Err` when the root itself is unreadable (one defect).
    pub fn open(
        db: &'a Database,
        class: String,
        name: String,
        btid: Btid,
    ) -> Result<Self, Box<Defect>> {
        let d = |code: &'static str| {
            Defect::new(Check::IndexHeap, code)
                .class(class.clone())
                .index(name.clone())
                .object(VpidText(btid.vpid()))
        };
        let page = db
            .vols
            .read_page(btid.vpid())
            .map_err(|e| Box::new(d("root-unreadable").actual(e)))?;
        let rec = slotted(&page)
            .and_then(|s| s.record(0).map(|r| r.1.to_vec()))
            .filter(|r| r.len() > RootHeader::FIXED_SIZE)
            .ok_or_else(|| Box::new(d("root-header").expected("a B-tree root header record")))?;
        let root = RootHeader::read(&rec);
        let kd = KeyDomain::from_packed(db.line, &root.packed_key_domain)
            .map_err(|e| Box::new(d("key-domain").actual(e)))?;
        let mut t = IndexTree {
            db,
            class,
            name,
            btid,
            root,
            kd,
            leaves: Vec::new(),
            defects: Vec::new(),
        };
        t.walk_upper();
        Ok(t)
    }

    fn d(&self, code: &'static str, at: Vpid) -> Defect {
        Defect::new(Check::IndexHeap, code)
            .class(self.class.clone())
            .index(self.name.clone())
            .object(VpidText(at))
    }

    /// A key as stored, overflow keys read from the overflow key file.
    pub fn key_of(&self, k: KeyRef) -> Result<Vec<u8>, String> {
        match k {
            KeyRef::Inline(b) => Ok(b.to_vec()),
            KeyRef::Overflow(first) => {
                let mut pages = Vec::new();
                let mut at = first;
                while !at.is_null() {
                    if pages.len() > 1 << 16 {
                        return Err("overflow key chain does not end".into());
                    }
                    let p = self.db.vols.read_page(at).map_err(|e| e.to_string())?;
                    at = Vpid::read(page::db_area(&p), 0);
                    pages.push((at, p));
                }
                let mut i = 0;
                read_key(first, |_| {
                    let a = pages.get(i).map(|(_, p)| page::db_area(p));
                    i += 1;
                    a
                })
                .map_err(str::to_string)
            }
        }
    }

    fn walk_upper(&mut self) {
        if self.db.role == Role::Target && (self.root.off68 != 0 || self.root.creator_mvccid != 0) {
            let d = self
                .d("root-mvcc-fields", self.btid.vpid())
                .expected("rev_level 0, creator_mvccid 0")
                .actual(format!(
                    "{:#x} {}",
                    self.root.off68, self.root.creator_mvccid
                ));
            self.defects.push(d);
        }
        let mut level: Vec<(Vpid, Option<Vec<u8>>)> = vec![(self.btid.vpid(), None)];
        let mut lvl = self.root.node.node_level;
        let mut seen = std::collections::HashSet::new();
        while lvl > 1 {
            let mut next = Vec::new();
            for (v, _) in &level {
                if !seen.insert(*v) {
                    self.defects.push(self.d("node-reached-twice", *v));
                    continue;
                }
                let Ok(p) = self.db.vols.read_page(*v) else {
                    self.defects.push(self.d("node-unreadable", *v));
                    continue;
                };
                let Some(s) = slotted(&p) else {
                    self.defects.push(self.d("node-not-slotted", *v));
                    continue;
                };
                let Some((_, hrec)) = s.record(0) else {
                    self.defects.push(self.d("node-no-header", *v));
                    continue;
                };
                let h = NodeHeader::read(hrec);
                if h.node_level != lvl {
                    let d = self.d("node-level", *v).expected(lvl).actual(h.node_level);
                    self.defects.push(d);
                }
                for i in 1..s.num_slots() {
                    let Some((_, rec)) = s.record(i) else {
                        continue;
                    };
                    match read_nonleaf_record(rec) {
                        Ok(r) => {
                            let stored = match r.key {
                                KeyRef::Inline(b) => b.len(),
                                KeyRef::Overflow(_) => DISK_VPID_SIZE,
                            };
                            if stored as i64 > h.max_key_len as i64 {
                                let d = self
                                    .d("max-key-len", *v)
                                    .expected(format!(">= {stored}"))
                                    .actual(h.max_key_len);
                                self.defects.push(d);
                            }
                            match self.key_of(r.key) {
                                Ok(k) => next.push((r.child, Some(k))),
                                Err(e) => {
                                    let d = self.d("overflow-key", *v).actual(e);
                                    self.defects.push(d);
                                    next.push((r.child, None));
                                }
                            }
                        }
                        Err(e) => {
                            let d = self
                                .d("nonleaf-record", *v)
                                .at(format!("slot {i}"))
                                .actual(e);
                            self.defects.push(d);
                        }
                    }
                }
            }
            level = next;
            lvl -= 1;
        }
        // the leftmost separator of a level bounds nothing (engine DML trees leave any key there)
        if let Some(first) = level.first_mut() {
            first.1 = None;
        }
        self.leaves = level;
    }
}

/// Streams a tree's live (key, OID) pairs, leaf by leaf, checking the leaf-level invariants.
pub struct LeafPairs<'t, 'a> {
    t: &'t mut IndexTree<'a>,
    next_leaf: usize,
    prev_leaf: Vpid,
    last_key: Option<Vec<u8>>,
    /// The key being streamed and its live OIDs, sorted, still to hand out (reversed).
    key: Vec<u8>,
    pending: Vec<Oid>,
    /// The current leaf and the next slot to read.
    page: Option<LeafCursor>,
    sep: Option<Vec<u8>>,
    first_in_leaf: bool,
    pub keys: i64,
    pub objects: i64,
    pub live_objects: i64,
    pub ovf_keys: usize,
    /// Keys held by more than one live object of a unique index.
    pub unique_violations: usize,
}

impl<'t, 'a> LeafPairs<'t, 'a> {
    pub fn new(t: &'t mut IndexTree<'a>) -> Self {
        LeafPairs {
            t,
            next_leaf: 0,
            prev_leaf: Vpid::NULL,
            last_key: None,
            key: Vec::new(),
            pending: Vec::new(),
            page: None,
            sep: None,
            first_in_leaf: true,
            keys: 0,
            objects: 0,
            live_objects: 0,
            ovf_keys: 0,
            unique_violations: 0,
        }
    }

    pub fn tree(&self) -> &IndexTree<'a> {
        self.t
    }

    fn push(&mut self, d: Defect) {
        self.t.defects.push(d);
    }

    /// The next live pair, `None` after the last leaf.
    pub fn next_pair(&mut self) -> Option<(&[u8], Oid)> {
        loop {
            if let Some(o) = self.pending.pop() {
                return Some((&self.key, o));
            }
            if !self.next_record() {
                return None;
            }
        }
    }

    /// Loads the next leaf; false after the last.
    fn load_leaf(&mut self) -> bool {
        loop {
            let Some((v, sep)) = self.t.leaves.get(self.next_leaf).cloned() else {
                return false;
            };
            let want_next = self
                .t
                .leaves
                .get(self.next_leaf + 1)
                .map_or(Vpid::NULL, |l| l.0);
            self.next_leaf += 1;
            let Some(p) = self.t.db.vols.read_page(v).ok() else {
                self.push(self.t.d("leaf-unreadable", v));
                continue;
            };
            let Some(h) = slotted(&p).and_then(|s| s.record(0).map(|r| NodeHeader::read(r.1)))
            else {
                self.push(self.t.d("leaf-no-header", v));
                continue;
            };
            if h.prev != self.prev_leaf || h.next != want_next || h.node_level != 1 {
                let d = self
                    .t
                    .d("leaf-links", v)
                    .expected(format!(
                        "prev {} next {} level 1",
                        VpidText(self.prev_leaf),
                        VpidText(want_next)
                    ))
                    .actual(format!(
                        "prev {} next {} level {}",
                        VpidText(h.prev),
                        VpidText(h.next),
                        h.node_level
                    ));
                self.push(d);
            }
            self.prev_leaf = v;
            self.sep = sep;
            self.first_in_leaf = true;
            self.page = Some(LeafCursor {
                vpid: v,
                page: p,
                header: h,
                slot: 1,
                fence: None,
            });
            return true;
        }
    }

    /// Reads records until one with live objects fills `key`/`pending`; false when the tree is exhausted.
    fn next_record(&mut self) -> bool {
        loop {
            if self.page.is_none() && !self.load_leaf() {
                return false;
            }
            let mut cur = self.page.take().unwrap();
            if cur.slot >= slotted(&cur.page).unwrap().num_slots() {
                continue;
            }
            cur.slot += 1;
            let produced = self.read_record(&mut cur);
            self.page = Some(cur);
            if produced {
                return true;
            }
        }
    }

    /// Reads slot `cur.slot - 1` of the current leaf; true when it streamed live objects.
    fn read_record(&mut self, cur: &mut LeafCursor) -> bool {
        let (v, h, slot) = (cur.vpid, cur.header, cur.slot - 1);
        let s = slotted(&cur.page).unwrap();
        {
            let Some((_, rec)) = s.record(slot) else {
                return false;
            };
            let unique = self.t.root.is_unique();
            let r = match read_leaf_record(rec, unique, &self.t.kd) {
                Ok(r) => r,
                Err(e) => {
                    let d = self
                        .t
                        .d("leaf-record", v)
                        .at(format!("slot {slot}"))
                        .actual(e);
                    self.push(d);
                    return false;
                }
            };
            let stored_len = match r.key {
                KeyRef::Inline(b) => b.len(),
                KeyRef::Overflow(_) => {
                    self.ovf_keys += 1;
                    DISK_VPID_SIZE
                }
            };
            if stored_len as i64 > h.max_key_len as i64 {
                let d = self
                    .t
                    .d("max-key-len", v)
                    .at(format!("slot {slot}"))
                    .expected(format!(">= {stored_len}"))
                    .actual(h.max_key_len);
                self.push(d);
            }
            let mut key = match self.t.key_of(r.key) {
                Ok(k) => k,
                Err(e) => {
                    let d = self
                        .t
                        .d("overflow-key", v)
                        .at(format!("slot {slot}"))
                        .actual(e);
                    self.push(d);
                    return false;
                }
            };
            if r.is_fence() {
                if cur.fence.is_none() {
                    cur.fence = Some(key);
                }
                return false;
            }
            // only a MIDXKEY with 0 < common_prefix < columns is compressed (the engine ignores it otherwise)
            let cp = h.common_prefix.max(0) as usize;
            if self.t.kd.midxkey && cp > 0 && cp < self.t.kd.n_columns() {
                match self
                    .t
                    .kd
                    .try_restore_prefix(cur.fence.as_deref().unwrap_or(&[]), &key, cp)
                {
                    Ok(k) => key = k,
                    Err(e) => {
                        let d = self
                            .t
                            .d("prefix-restore", v)
                            .at(format!("slot {slot}"))
                            .actual(e);
                        self.push(d);
                        return false;
                    }
                }
            }
            self.check_order(v, slot, &key);
            let objects = self.objects_of(v, slot, &r);
            self.objects += objects.len() as i64;
            let mut live: Vec<Oid> = objects
                .iter()
                .filter(|o| self.t.db.role == Role::Target || o.delid.is_none_or(|d| d == 0))
                .map(|o| o.oid)
                .collect();
            if self.t.db.role == Role::Target && live.windows(2).any(|w| w[0] >= w[1]) {
                let d = self.t.d("oids-not-ascending", v).at(format!("slot {slot}"));
                self.push(d);
            }
            live.sort_unstable();
            for w in live.windows(2).filter(|w| w[0] == w[1]) {
                let d = self
                    .t
                    .d("duplicate-oid", v)
                    .object(OidText(w[0]))
                    .at(format!("vpid {} slot {slot}", VpidText(v)));
                self.t.defects.push(d);
            }
            live.dedup();
            // the engine counts only keys with a live object (T1 dirty: keys of deleted rows wait for vacuum)
            if !live.is_empty() {
                self.keys += 1;
            }
            if unique && live.len() > 1 {
                self.unique_violations += 1;
                let d = self
                    .t
                    .d("unique-violation", v)
                    .object(format!("{:?}", self.t.kd.decode_key(&key)))
                    .at(format!("vpid {} slot {slot}", VpidText(v)))
                    .actual(format!("{} live objects", live.len()));
                self.push(d);
            }
            self.live_objects += live.len() as i64;
            live.reverse();
            self.pending = live;
            self.last_key = Some(key.clone());
            self.key = key;
            !self.pending.is_empty()
        }
    }

    fn check_order(&mut self, v: Vpid, slot: usize, key: &[u8]) {
        let kd = &self.t.kd;
        if self.first_in_leaf {
            self.first_in_leaf = false;
            if let Some(sep) = self.sep.take() {
                let left_ok = self
                    .last_key
                    .as_ref()
                    .is_none_or(|l| kd.compare(l, &sep) == Ordering::Less);
                if !left_ok || kd.compare(&sep, key) == Ordering::Greater {
                    let d = self
                        .t
                        .d("separator", v)
                        .expected("left leaf's last key < separator <= first key");
                    self.t.defects.push(d);
                }
            }
        }
        if let Some(l) = &self.last_key
            && kd.compare(l, key) != Ordering::Less
        {
            let d = self
                .t
                .d("keys-not-increasing", v)
                .at(format!("slot {slot}"));
            self.t.defects.push(d);
        }
    }

    /// Every object of the record: first, the others, and the overflow OID pages.
    fn objects_of(
        &mut self,
        v: Vpid,
        slot: usize,
        r: &fastmig_ondisk::btree::leaf_record::LeafRecord,
    ) -> Vec<BtObject> {
        let unique = self.t.root.is_unique();
        let target = self.t.db.role == Role::Target;
        let has_ovf = r.ovf_oids.is_some();
        let at = format!("vpid {} slot {slot}", VpidText(v));
        if target && (r.flags & LEAF_RECORD_CLASS_OID != 0) != (unique && has_ovf) {
            let d = self.t.d("class-oid-flag", v).at(at.clone());
            self.push(d);
        }
        let mut objs = vec![r.first];
        self.check_form(&r.first, has_ovf, unique && has_ovf, v, &at);
        for o in &r.others {
            self.check_form(o, unique, unique, v, &at);
            objs.push(*o);
        }
        let mut next = r.ovf_oids;
        let mut data_pages: Vec<(Vpid, Oid)> = Vec::new();
        let mut dir_head = None;
        while let Some(o) = next.filter(|o| !o.is_null()) {
            if data_pages.len() > 1 << 20 {
                let d = self.t.d("ovf-oid-cycle", o).at(at.clone());
                self.push(d);
                break;
            }
            let Some((hdr, objs_rec)) = self.ovf_page(o) else {
                let d = self.t.d("ovf-oid-page", o).at(at.clone());
                self.push(d);
                break;
            };
            if target && hdr.dir.is_some() == unique {
                let d = self
                    .t
                    .d("ovf-oid-header", o)
                    .expected(if unique { "8 B header" } else { "16 B header" })
                    .at(at.clone());
                self.push(d);
            }
            if data_pages.is_empty() {
                dir_head = hdr.dir;
            } else if hdr.dir != dir_head {
                let d = self.t.d("ovf-dir-head", o).at(at.clone());
                self.push(d);
            }
            let mut q = 0;
            let mut first = None;
            while q < objs_rec.len() {
                match read_object(&objs_rec, q, unique) {
                    Ok((ob, n)) => {
                        self.check_form(&ob, true, unique, o, &at);
                        first.get_or_insert(ob.oid);
                        objs.push(ob);
                        q = n;
                    }
                    Err(e) => {
                        let d = self.t.d("ovf-oid-object", o).at(at.clone()).actual(e);
                        self.push(d);
                        break;
                    }
                }
            }
            data_pages.push((o, first.unwrap_or(Oid::NULL)));
            next = Some(hdr.next);
        }
        if let Some(head) = dir_head.filter(|d| !d.is_null()) {
            let mut entries = Vec::new();
            let mut dv = head;
            while !dv.is_null() && entries.len() <= data_pages.len() {
                let Some((hdr, rec)) = self.ovf_page(dv) else {
                    break;
                };
                entries.extend(
                    OvfDirEntry::read_all(&rec)
                        .into_iter()
                        .map(|e| (e.vpid, e.sep_oid)),
                );
                dv = hdr.next;
            }
            if entries != data_pages {
                let d = self
                    .t
                    .d("ovf-directory", head)
                    .at(at)
                    .expected(format!("{} entries (page, first OID)", data_pages.len()))
                    .actual(format!("{} entries", entries.len()));
                self.push(d);
            }
        }
        objs
    }

    fn ovf_page(&self, v: Vpid) -> Option<(OvfHeader, Vec<u8>)> {
        let p = self.t.db.vols.read_page(v).ok()?;
        let s = slotted(&p)?;
        let hdr = OvfHeader::read(s.record(0)?.1).ok()?;
        Some((hdr, s.record(1)?.1.to_vec()))
    }

    /// Target only: a fixed-size object carries INSID = all visible and DELID 0 (and the class OID where the unique
    /// form has one); any other object carries no MVCCID.
    fn check_form(&mut self, o: &BtObject, fixed: bool, class_oid: bool, v: Vpid, at: &str) {
        if self.t.db.role != Role::Target {
            return;
        }
        let ok = if fixed {
            o.insid == Some(MVCCID_ALL_VISIBLE)
                && o.delid == Some(0)
                && o.class_oid.is_some() == class_oid
        } else {
            o.insid.is_none() && o.delid.is_none()
        };
        if !ok {
            let d = self
                .t
                .d("object-mvcc-form", v)
                .object(OidText(o.oid))
                .at(at.to_string())
                .expected(if fixed {
                    "INSID 3, DELID 0"
                } else {
                    "no MVCCID"
                })
                .actual(format!("{:?} {:?}", o.insid, o.delid));
            self.push(d);
        }
    }

    /// Root statistics against what the leaves held; `nulls` = all-NULL rows the heap pass counted (`None` when the
    /// heap side was not built: a deferred index).
    pub fn finish(mut self, nulls: Option<u64>) -> Vec<Defect> {
        let rh = self.t.root.clone();
        let root = self.t.btid.vpid();
        if rh.is_unique() {
            let got = (self.live_objects, self.keys);
            if rh.num_oids - rh.num_nulls != got.0 || rh.num_keys != got.1 {
                let d = self
                    .t
                    .d("unique-stats", root)
                    .expected(format!("num_oids - num_nulls {} num_keys {}", got.0, got.1))
                    .actual(format!(
                        "{} {} (num_oids {} num_nulls {})",
                        rh.num_oids - rh.num_nulls,
                        rh.num_keys,
                        rh.num_oids,
                        rh.num_nulls
                    ));
                self.push(d);
            }
            if let Some(n) = nulls
                && rh.num_nulls != n as i64
            {
                let d = self
                    .t
                    .d("unique-num-nulls", root)
                    .expected(n)
                    .actual(rh.num_nulls);
                self.push(d);
            }
        } else if (rh.num_oids, rh.num_nulls, rh.num_keys) != (-1, -1, -1) {
            let d = self
                .t
                .d("nonunique-stats", root)
                .expected("-1 -1 -1")
                .actual(format!("{} {} {}", rh.num_oids, rh.num_nulls, rh.num_keys));
            self.push(d);
        }
        if rh.ovfid.is_null() != (self.ovf_keys == 0) {
            let d = self
                .t
                .d("ovfid", root)
                .expected(format!("{} overflow keys", self.ovf_keys))
                .actual(format!("{:?}", rh.ovfid));
            self.push(d);
        }
        std::mem::take(&mut self.t.defects)
    }
}

/// One leaf being streamed: its page, header, the next slot, and its lower fence key (11.4 prefix compression).
struct LeafCursor {
    vpid: Vpid,
    page: Box<[u8]>,
    header: NodeHeader,
    slot: usize,
    fence: Option<Vec<u8>>,
}
