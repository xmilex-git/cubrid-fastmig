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

use std::borrow::Cow;
use std::cmp::Ordering;

use fastmig_ondisk::btree::be_field::LEAF_RECORD_CLASS_OID;
use fastmig_ondisk::btree::leaf_record::{BtObject, KeyRef, read_leaf_record, read_object};
use fastmig_ondisk::btree::node_header::NodeHeader;
use fastmig_ondisk::btree::nonleaf_record::read_nonleaf_record;
use fastmig_ondisk::btree::ovf_key::read_key;
use fastmig_ondisk::btree::ovf_oid_page::{OvfDirEntry, OvfHeader};
use fastmig_ondisk::btree::root_header::RootHeader;
use fastmig_ondisk::btree::{
    DISK_VPID_SIZE, MVCCID_ALL_VISIBLE, max_oidlen_inpage, object_fixed_size,
};
use fastmig_ondisk::page;
use fastmig_ondisk::source_version::HeaderLayout;
use fastmig_ondisk::spage::SlottedPage;
use fastmig_ondisk::value::KeyDomain;
use fastmig_ondisk::{Btid, Oid, Vpid, db_pagesize};

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
            .ok_or_else(|| Box::new(d("root-header").expected("a B-tree root header record")))?;
        let root = RootHeader::read_layout(&rec, db.version.header_layout())
            .map_err(|e| Box::new(d("root-header").actual(e.what)))?;
        let kd = KeyDomain::from_packed(db.line, &root.packed_key_domain)
            .map(|k| k.in_layout(db.version.header_layout()))
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

    /// The separator of leaf `i` (empty for a leaf whose parent record carries none: the first, or unreadable).
    pub fn separator(&self, i: usize) -> &[u8] {
        self.leaves[i].1.as_deref().unwrap_or_default()
    }

    /// The common prefix of a leaf written before 11.4, which kept none in its header: a MIDXKEY leaf with at least
    /// two keys whose first and last records are fences is compressed, and its prefix is the columns the two fence keys
    /// share (release/11.3 btree.c `btree_node_is_compressed`, `btree_node_common_prefix` → `pr_midxkey_common_prefix`;
    /// 11.4 c1e7266a3 #4789 moved it into the header; workspace#529). 0 when the page does not qualify.
    fn common_prefix_before_11_4(&self, page: &[u8]) -> i32 {
        let Some(s) = slotted(page) else { return 0 };
        let key_cnt = s.num_slots().saturating_sub(1);
        if !self.kd.midxkey || key_cnt < 2 {
            return 0;
        }
        let unique = self.root.is_unique();
        let fence_key = |slot| {
            let (_, rec) = s.record(slot)?;
            let r = read_leaf_record(rec, unique, &self.kd).ok()?;
            match r.key {
                KeyRef::Inline(b) if r.is_fence() => Some(b),
                _ => None,
            }
        };
        match (fence_key(1), fence_key(key_cnt)) {
            (Some(lower), Some(upper)) => self.kd.common_columns(lower, upper) as i32,
            _ => 0,
        }
    }

    fn d(&self, code: &'static str, at: Vpid) -> Defect {
        Defect::new(Check::IndexHeap, code)
            .class(self.class.clone())
            .index(self.name.clone())
            .object(VpidText(at))
    }

    /// A key as stored, overflow keys read from the overflow key file. A key that does not reach the record
    /// reader's `image_len` check (an overflow key, a non-leaf separator: `check_inline`) is checked here, so
    /// every key later compared splits. Errors carry the defect code: `overflow-key` or `key-image`.
    pub fn key_of(&self, k: KeyRef, check_inline: bool) -> Result<Vec<u8>, (&'static str, String)> {
        let key = match k {
            KeyRef::Inline(b) if !check_inline => return Ok(b.to_vec()),
            KeyRef::Inline(b) => b.to_vec(),
            KeyRef::Overflow(first) => {
                let ovf = |e: String| ("overflow-key", e);
                let mut pages = Vec::new();
                let mut at = first;
                while !at.is_null() {
                    if pages.len() > 1 << 16 {
                        return Err(ovf("overflow key chain does not end".into()));
                    }
                    let p = self.db.vols.read_page(at).map_err(|e| ovf(e.to_string()))?;
                    at = Vpid::read(page::db_area(&p), 0);
                    pages.push((at, p));
                }
                let mut i = 0;
                read_key(first, |_| {
                    let a = pages.get(i).map(|(_, p)| page::db_area(p));
                    i += 1;
                    a
                })
                .map_err(|e| ovf(e.to_string()))?
            }
        };
        self.kd
            .try_image_len(&key)
            .map_err(|e| ("key-image", e.to_string()))?;
        Ok(key)
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
                let Ok(h) = NodeHeader::read_layout(hrec, self.db.version.header_layout()) else {
                    self.defects.push(self.d("node-no-header", *v));
                    continue;
                };
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
                            match self.key_of(r.key, true) {
                                Ok(k) => next.push((r.child, Some(k))),
                                Err((code, e)) => {
                                    let d = self.d(code, *v).actual(e);
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

/// What the leaves held, summed over the shards of one tree for the root statistics.
#[derive(Default, Clone, Copy)]
pub struct LeafCounts {
    pub keys: i64,
    pub objects: i64,
    pub live_objects: i64,
    pub ovf_keys: usize,
    /// Keys held by more than one live object of a unique index.
    pub unique_violations: usize,
    /// Leaf records, a key of only deleted objects included (workspace#526: an upper bound of the keys a rebuild
    /// of the live rows holds).
    pub records: i64,
    /// Records holding more objects, deleted ones included, than a rebuilt leaf record takes
    /// (`max_oidlen_inpage / object_fixed_size`), and the objects past that count: upper bounds of the rebuild's
    /// overflow OID chains and their objects.
    pub wide_records: i64,
    pub objects_past_record: i64,
}

impl std::ops::AddAssign for LeafCounts {
    fn add_assign(&mut self, o: Self) {
        self.keys += o.keys;
        self.objects += o.objects;
        self.live_objects += o.live_objects;
        self.ovf_keys += o.ovf_keys;
        self.unique_violations += o.unique_violations;
        self.records += o.records;
        self.wide_records += o.wide_records;
        self.objects_past_record += o.objects_past_record;
    }
}

/// A shard's first loaded leaf: its links, checked against the leaf loaded before it once the shards are joined.
pub struct FirstLeaf {
    pub vpid: Vpid,
    pub prev: Vpid,
    pub next: Vpid,
    pub level: i16,
    pub want_next: Vpid,
}

/// A shard's first ordered key: compared with the last key before the shard once the shards are joined.
pub struct FirstKey {
    pub vpid: Vpid,
    pub slot: usize,
    pub key: Vec<u8>,
    /// The separator checked at this key (it starts its leaf) and whether `separator <= key` already failed.
    pub sep: Option<Vec<u8>>,
    pub sep_failed: bool,
}

/// What a shard leaves to the join: its edges and the last leaf and key it read.
#[derive(Default)]
pub struct ShardEnd {
    pub first_leaf: Option<FirstLeaf>,
    pub first_key: Option<FirstKey>,
    pub last_leaf: Option<Vpid>,
    pub last_key: Option<Vec<u8>>,
}

/// Streams a tree's live (key, OID) pairs, leaf by leaf, checking the leaf-level invariants — of the whole leaf
/// list, or of one shard (a leaf range; the checks across its edges are left to [`join_shards`]).
pub struct LeafPairs<'t, 'a> {
    t: &'t IndexTree<'a>,
    next_leaf: usize,
    end_leaf: usize,
    /// The leaf loaded before; `None` at a shard's start (its first leaf's links are checked by the join).
    prev_leaf: Option<Vpid>,
    shard: bool,
    edge: ShardEnd,
    defects: Vec<Defect>,
    /// The last key read; while `pending` is not empty, the key being streamed (its buffer reused per record).
    last_key: Option<Vec<u8>>,
    /// The live OIDs of `last_key`, sorted, still to hand out (reversed).
    pending: Vec<Oid>,
    /// The current record's objects, reused from record to record (no allocation per row).
    objects_buf: Vec<BtObject>,
    /// The current leaf and the next slot to read.
    page: Option<LeafCursor>,
    sep: Option<Vec<u8>>,
    first_in_leaf: bool,
    pub counts: LeafCounts,
}

impl<'t, 'a> LeafPairs<'t, 'a> {
    /// Every leaf of `t`.
    pub fn new(t: &'t IndexTree<'a>) -> Self {
        Self::over(t, 0..t.leaves.len(), false)
    }

    /// The leaves `leaves` of `t`, one shard of a merge split by leaf range (workspace#509).
    pub fn shard(t: &'t IndexTree<'a>, leaves: std::ops::Range<usize>) -> Self {
        Self::over(t, leaves, true)
    }

    fn over(t: &'t IndexTree<'a>, leaves: std::ops::Range<usize>, shard: bool) -> Self {
        LeafPairs {
            t,
            next_leaf: leaves.start,
            end_leaf: leaves.end,
            prev_leaf: (!shard).then_some(Vpid::NULL),
            shard,
            edge: ShardEnd::default(),
            defects: Vec::new(),
            last_key: None,
            pending: Vec::new(),
            objects_buf: Vec::new(),
            page: None,
            sep: None,
            first_in_leaf: true,
            counts: LeafCounts::default(),
        }
    }

    pub fn tree(&self) -> &IndexTree<'a> {
        self.t
    }

    fn push(&mut self, d: Defect) {
        self.defects.push(d);
    }

    /// The next live pair, `None` after the last leaf.
    pub fn next_pair(&mut self) -> Option<(&[u8], Oid)> {
        loop {
            if let Some(o) = self.pending.pop() {
                return Some((self.last_key.as_deref().unwrap_or_default(), o));
            }
            if !self.next_record() {
                return None;
            }
        }
    }

    /// Loads the next leaf; false after the last.
    fn load_leaf(&mut self) -> bool {
        loop {
            if self.next_leaf >= self.end_leaf {
                return false;
            }
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
            let layout = self.t.db.version.header_layout();
            let Some(mut h) = slotted(&p).and_then(|s| {
                s.record(0)
                    .and_then(|r| NodeHeader::read_layout(r.1, layout).ok())
            }) else {
                self.push(self.t.d("leaf-no-header", v));
                continue;
            };
            if layout == HeaderLayout::Before11_4 {
                h.common_prefix = self.t.common_prefix_before_11_4(&p);
            }
            let links = FirstLeaf {
                vpid: v,
                prev: h.prev,
                next: h.next,
                level: h.node_level,
                want_next,
            };
            match self.prev_leaf {
                Some(prev) => {
                    if let Some(d) = self.t.check_links(&links, prev) {
                        self.push(d);
                    }
                }
                None => self.edge.first_leaf = Some(links),
            }
            self.prev_leaf = Some(v);
            self.edge.last_leaf = Some(v);
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
                    self.counts.ovf_keys += 1;
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
            let mut key: Cow<[u8]> = match r.key {
                // an inline key is read in place: no copy per record
                KeyRef::Inline(b) => Cow::Borrowed(b),
                k => match self.t.key_of(k, false) {
                    Ok(k) => Cow::Owned(k),
                    Err((code, e)) => {
                        let d = self.t.d(code, v).at(format!("slot {slot}")).actual(e);
                        self.push(d);
                        return false;
                    }
                },
            };
            if r.is_fence() {
                if cur.fence.is_none() {
                    cur.fence = Some(key.into_owned());
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
                    Ok(k) => key = Cow::Owned(k),
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
            self.counts.objects += objects.len() as i64;
            self.counts.records += 1;
            let per_record = max_oidlen_inpage(db_pagesize(self.t.db.vols.io_pagesize()))
                / object_fixed_size(self.t.root.is_unique());
            if objects.len() > per_record {
                self.counts.wide_records += 1;
                self.counts.objects_past_record += (objects.len() - per_record) as i64;
            }
            let target = self.t.db.role == Role::Target;
            let mut live = std::mem::take(&mut self.pending);
            live.clear();
            live.extend(
                objects
                    .iter()
                    .filter(|o| target || o.delid.is_none_or(|d| d == 0))
                    .map(|o| o.oid),
            );
            self.objects_buf = objects;
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
                self.defects.push(d);
            }
            live.dedup();
            // the engine counts only keys with a live object (T1 dirty: keys of deleted rows wait for vacuum)
            if !live.is_empty() {
                self.counts.keys += 1;
            }
            if unique && live.len() > 1 {
                self.counts.unique_violations += 1;
                let d = self
                    .t
                    .d("unique-violation", v)
                    .object(format!("{:?}", self.t.kd.decode_key(&key)))
                    .at(format!("vpid {} slot {slot}", VpidText(v)))
                    .actual(format!("{} live objects", live.len()));
                self.push(d);
            }
            self.counts.live_objects += live.len() as i64;
            live.reverse();
            self.pending = live;
            match &mut self.last_key {
                Some(l) => {
                    l.clear();
                    l.extend_from_slice(&key);
                }
                None => self.last_key = Some(key.into_owned()),
            }
            !self.pending.is_empty()
        }
    }

    fn check_order(&mut self, v: Vpid, slot: usize, key: &[u8]) {
        let t = self.t;
        let kd = &t.kd;
        let (mut sep_checked, mut sep_failed) = (None, false);
        if self.first_in_leaf {
            self.first_in_leaf = false;
            if let Some(sep) = self.sep.take() {
                let left_ok = self
                    .last_key
                    .as_ref()
                    .is_none_or(|l| kd.compare(l, &sep) == Ordering::Less);
                sep_failed = kd.compare(&sep, key) == Ordering::Greater;
                if !left_ok || sep_failed {
                    self.defects.push(t.separator_defect(v));
                }
                sep_checked = Some(sep);
            }
        }
        match &self.last_key {
            Some(l) => {
                if kd.compare(l, key) != Ordering::Less {
                    self.defects.push(t.order_defect(v, slot));
                }
            }
            // a shard's first key: the key before it is in another shard, compared by `join_shards`
            None if self.shard => {
                self.edge.first_key = Some(FirstKey {
                    vpid: v,
                    slot,
                    key: key.to_vec(),
                    sep: sep_checked,
                    sep_failed,
                })
            }
            None => {}
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
        // defect text only when a defect is found: this runs once per leaf record
        let at = || format!("vpid {} slot {slot}", VpidText(v));
        if target && (r.flags & LEAF_RECORD_CLASS_OID != 0) != (unique && has_ovf) {
            let d = self.t.d("class-oid-flag", v).at(at());
            self.push(d);
        }
        let mut objs = std::mem::take(&mut self.objects_buf);
        objs.clear();
        objs.push(r.first);
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
                let d = self.t.d("ovf-oid-cycle", o).at(at());
                self.push(d);
                break;
            }
            let Some((hdr, objs_rec)) = self.ovf_page(o) else {
                let d = self.t.d("ovf-oid-page", o).at(at());
                self.push(d);
                break;
            };
            if target && hdr.dir.is_some() == unique {
                let d = self
                    .t
                    .d("ovf-oid-header", o)
                    .expected(if unique { "8 B header" } else { "16 B header" })
                    .at(at());
                self.push(d);
            }
            if data_pages.is_empty() {
                dir_head = hdr.dir;
            } else if hdr.dir != dir_head {
                let d = self.t.d("ovf-dir-head", o).at(at());
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
                        let d = self.t.d("ovf-oid-object", o).at(at()).actual(e);
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
                    .at(at())
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
    fn check_form(
        &mut self,
        o: &BtObject,
        fixed: bool,
        class_oid: bool,
        v: Vpid,
        at: &dyn Fn() -> String,
    ) {
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
                .at(at())
                .expected(if fixed {
                    "INSID 3, DELID 0"
                } else {
                    "no MVCCID"
                })
                .actual(format!("{:?} {:?}", o.insid, o.delid));
            self.push(d);
        }
    }

    /// This walk's defects plus the root statistics against what the leaves held; `nulls` = all-NULL rows the heap
    /// pass counted (`None` when the heap side was not built: a deferred index). The tree's own defects (from
    /// [`IndexTree::open`]) stay in the tree.
    pub fn finish(mut self, nulls: Option<u64>) -> Vec<Defect> {
        self.defects.extend(self.t.check_stats(&self.counts, nulls));
        self.defects
    }

    /// A shard's defects, counts and edges, for [`join_shards`] and [`IndexTree::check_stats`].
    pub fn into_shard(self) -> (Vec<Defect>, LeafCounts, ShardEnd) {
        let mut edge = self.edge;
        edge.last_key = self.last_key;
        (self.defects, self.counts, edge)
    }
}

/// The checks across the shards' edges, in leaf order: each shard's first leaf links to the last leaf loaded before
/// it, its first key is above the last key read before it and that key is below the separator — exactly what one
/// walk over every leaf checks at those points.
pub fn join_shards(t: &IndexTree, ends: &[ShardEnd]) -> Vec<Defect> {
    let mut out = Vec::new();
    let mut prev_leaf = Vpid::NULL;
    let mut last_key: Option<&[u8]> = None;
    for e in ends {
        if let Some(l) = &e.first_leaf
            && let Some(d) = t.check_links(l, prev_leaf)
        {
            out.push(d);
        }
        if let (Some(f), Some(l)) = (&e.first_key, last_key) {
            if let Some(sep) = &f.sep
                && !f.sep_failed
                && t.kd.compare(l, sep) != Ordering::Less
            {
                out.push(t.separator_defect(f.vpid));
            }
            if t.kd.compare(l, &f.key) != Ordering::Less {
                out.push(t.order_defect(f.vpid, f.slot));
            }
        }
        prev_leaf = e.last_leaf.unwrap_or(prev_leaf);
        last_key = e.last_key.as_deref().or(last_key);
    }
    out
}

impl IndexTree<'_> {
    /// A leaf's links: `prev` is the leaf loaded before it, `want_next` the next in the leaf list.
    fn check_links(&self, l: &FirstLeaf, prev: Vpid) -> Option<Defect> {
        (l.prev != prev || l.next != l.want_next || l.level != 1).then(|| {
            self.d("leaf-links", l.vpid)
                .expected(format!(
                    "prev {} next {} level 1",
                    VpidText(prev),
                    VpidText(l.want_next)
                ))
                .actual(format!(
                    "prev {} next {} level {}",
                    VpidText(l.prev),
                    VpidText(l.next),
                    l.level
                ))
        })
    }

    fn separator_defect(&self, v: Vpid) -> Defect {
        self.d("separator", v)
            .expected("left leaf's last key < separator <= first key")
    }

    fn order_defect(&self, v: Vpid, slot: usize) -> Defect {
        self.d("keys-not-increasing", v).at(format!("slot {slot}"))
    }

    /// Root statistics against what the leaves held (summed over the shards); `nulls` as [`LeafPairs::finish`].
    pub fn check_stats(&self, c: &LeafCounts, nulls: Option<u64>) -> Vec<Defect> {
        let mut out = Vec::new();
        let rh = &self.root;
        let root = self.btid.vpid();
        if rh.is_unique() {
            let got = (c.live_objects, c.keys);
            if rh.num_oids - rh.num_nulls != got.0 || rh.num_keys != got.1 {
                out.push(
                    self.d("unique-stats", root)
                        .expected(format!("num_oids - num_nulls {} num_keys {}", got.0, got.1))
                        .actual(format!(
                            "{} {} (num_oids {} num_nulls {})",
                            rh.num_oids - rh.num_nulls,
                            rh.num_keys,
                            rh.num_oids,
                            rh.num_nulls
                        )),
                );
            }
            if let Some(n) = nulls
                && rh.num_nulls != n as i64
            {
                out.push(
                    self.d("unique-num-nulls", root)
                        .expected(n)
                        .actual(rh.num_nulls),
                );
            }
        } else if (rh.num_oids, rh.num_nulls, rh.num_keys) != (-1, -1, -1) {
            out.push(
                self.d("nonunique-stats", root)
                    .expected("-1 -1 -1")
                    .actual(format!("{} {} {}", rh.num_oids, rh.num_nulls, rh.num_keys)),
            );
        }
        if rh.ovfid.is_null() != (c.ovf_keys == 0) {
            out.push(
                self.d("ovfid", root)
                    .expected(format!("{} overflow keys", c.ovf_keys))
                    .actual(format!("{:?}", rh.ovfid)),
            );
        }
        out
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
