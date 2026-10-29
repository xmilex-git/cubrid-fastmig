//! The index keys of one heap page of a target class: every live row's (key, OID) for each of the class's plain
//! indexes, from records already in the target (develop) format.
//!
//! Input: the target volumes, a [`ClassKeys`] (the class's layouts by repid and its indexes, resolved once before
//! the scan) and a worker's page buffer. Output: `emit(index, oid, key)` calls for `heap_scan::scan_classes`.
//! Live row: a HOME, RELOCATION (→ its NEWHOME) or BIGONE (→ its overflow chain) slot past slot 0 whose header has
//! no committed DELID; NEWHOME slots are reached from their RELOCATION; bestspace pages hold no rows. The
//! migration's heap pass (patch, re-encode) runs before this, so every row is in the target format.
//! Source: engine `heap_file.c` record types and `heap_get_visible_version` on a cleanly shut down database (R2
//! § 3 R3: every DELID is committed), `btree_load.c` `btree_sort_get_next` (key per live row); the same rules as the
//! verifier's heap pass (workspace#495), without its defect checks.

use std::collections::HashMap;

use fastmig_ondisk::class_object::{ClassObject, Constraint};
use fastmig_ondisk::heap_rewrite::ReEncode;
use fastmig_ondisk::page::{self, PageType};
use fastmig_ondisk::record::{
    ExtractedKey, HeapChain, RecordHeader, RecordLayout, decode_record, extract_key, resolve_record,
};
use fastmig_ondisk::spage::{RecordType, SlottedPage};
use fastmig_ondisk::value::{DecodeResult, KeyDomain};
use fastmig_ondisk::volume_set::VolumeSet;
use fastmig_ondisk::{Hfid, Oid, VersionLine, Vpid};

use crate::build_error::BuildError;
use crate::heap_scan::RowKey;

/// One class's heap as the key extractor reads it: decided once per class, used for every row.
pub struct ClassKeys<'c> {
    pub class_oid: Oid,
    pub hfid: Hfid,
    layouts: HashMap<u32, RecordLayout>,
    indexes: Vec<(&'c Constraint, &'c KeyDomain)>,
    /// Records still in the 11.4 encoding go through this first (the plan's pair count on an 11.4 source of a
    /// re-encoded class, workspace#526); the build reads target records and has none.
    re_encode: Option<&'c dyn ReEncode>,
}

impl<'c> ClassKeys<'c> {
    /// Reads records through `r` into the develop encoding before their keys are extracted.
    pub fn with_re_encode(mut self, r: &'c dyn ReEncode) -> Self {
        self.re_encode = Some(r);
        self
    }

    /// `indexes`: the class's plain indexes in the order their runs come out.
    pub fn new(
        class_oid: Oid,
        class: &ClassObject,
        hfid: Hfid,
        indexes: Vec<(&'c Constraint, &'c KeyDomain)>,
    ) -> DecodeResult<Self> {
        let layouts = std::iter::once(class.repid)
            .chain(class.representations.iter().map(|r| r.id))
            .map(|id| {
                Ok((
                    id as u32,
                    RecordLayout::for_repid(VersionLine::Develop, class, id as u32)?,
                ))
            })
            .collect::<DecodeResult<_>>()?;
        Ok(ClassKeys {
            class_oid,
            hfid,
            layouts,
            indexes,
            re_encode: None,
        })
    }

    /// How many indexes this class feeds (the length of a per-index tally).
    pub fn indexes_len(&self) -> usize {
        self.indexes.len()
    }
}

fn bad(oid: Oid, what: impl Into<String>) -> BuildError {
    BuildError::Record {
        oid,
        what: what.into(),
    }
}

/// Reads page `vpid` of `ck`'s heap into `buf` (the worker's, one IO page) and emits its rows' keys.
pub fn page_keys(
    vols: &VolumeSet,
    ck: &ClassKeys,
    buf: &mut [u8],
    vpid: Vpid,
    emit: &mut dyn FnMut(usize, Oid, RowKey),
) -> Result<(), BuildError> {
    let page_oid = Oid::new(vpid.volid, vpid.pageid, 0);
    vols.read_page_into(vpid, buf)?;
    if page::ptype(buf) != Some(PageType::Heap) {
        return Err(bad(page_oid, "not a heap page"));
    }
    let sp = SlottedPage::new(page::db_area(buf))
        .ok_or_else(|| bad(page_oid, "unreadable slotted page header"))?;
    if vpid != ck.hfid.vpid() {
        let (_, slot0) = sp
            .record(0)
            .ok_or_else(|| bad(page_oid, "no chain record"))?;
        let chain = HeapChain::read(slot0).ok_or_else(|| bad(page_oid, "short chain record"))?;
        if chain.is_bestspace() {
            return Ok(());
        }
    }
    for slot in 1..sp.num_slots() {
        let Some((rtype, rec)) = sp.record(slot) else {
            continue;
        };
        let oid = Oid::new(vpid.volid, vpid.pageid, slot as i16);
        // a HOME record is read in place; the others are copied out of their NEWHOME or overflow pages
        let moved;
        let bytes: &[u8] = match rtype {
            RecordType::Home => rec,
            RecordType::Relocation | RecordType::BigOne => {
                moved = resolve_record(vols, oid, rtype, rec)?
                    .ok_or_else(|| bad(oid, format!("{rtype:?} without a record")))?;
                &moved.bytes
            }
            _ => continue,
        };
        let header = RecordHeader::read(bytes).map_err(|e| bad(oid, e.to_string()))?;
        if header.is_deleted() {
            continue;
        }
        let re_encoded;
        let (bytes, header) = match ck.re_encode {
            Some(r) => {
                re_encoded = r
                    .re_encode(oid, bytes)
                    .map_err(|e| bad(oid, e.to_string()))?;
                let h = RecordHeader::read(&re_encoded).map_err(|e| bad(oid, e.to_string()))?;
                (&re_encoded[..], h)
            }
            None => (bytes, header),
        };
        let layout = ck
            .layouts
            .get(&header.repid)
            .ok_or_else(|| bad(oid, format!("repid {} unknown to the class", header.repid)))?;
        let values = decode_record(VersionLine::Develop, layout, bytes)
            .map_err(|e| bad(oid, e.to_string()))?
            .values;
        for (ix, (constraint, kd)) in ck.indexes.iter().enumerate() {
            match extract_key(kd, layout, &values, constraint)
                .map_err(|e| bad(oid, e.to_string()))?
            {
                ExtractedKey::Key(k) => emit(ix, oid, RowKey::Key(&k)),
                ExtractedKey::AllNull => emit(ix, oid, RowKey::AllNull),
                ExtractedKey::NullInPrimaryKey(_) => emit(ix, oid, RowKey::NotNullViolation),
            }
        }
    }
    Ok(())
}
