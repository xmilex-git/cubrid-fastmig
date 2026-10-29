//! Puts a new version of a record under its existing OID: in the home slot, as REC_RELOCATION + REC_NEWHOME, or
//! as REC_BIGONE + an overflow chain; the record's previous NEWHOME slot or overflow chain is released first.
//!
//! Input: a [`HeapState`], the object's OID (its home slot holds REC_HOME, REC_RELOCATION or REC_BIGONE) and the
//! new record bytes. Output: the [`HomeKind`] it ended up as.
//! Source: workspace#482 decision 6 (NEWHOME placement); engine develop @5b1867174 `heap_file.c`
//! `heap_manager_initialize` (`heap_Maxslotted_reclength = DB_ALIGN_BELOW (spage_max_record_size () -
//! DB_ALIGN (sizeof (HEAP_CHAIN), 4) - 8 * SPAGE_SLOT_SIZE, 4)`, :3742-3744), `heap_is_big_length`,
//! `heap_insert_adjust_recdes_header` (a big MVCC record gets the maximum header), `heap_ovf_insert` (home slot =
//! first overflow page as an OID); R2 § 1.8, § 1.9, § 3 R7.4. An old NEWHOME slot is deleted as
//! REC_DELETED_WILL_REUSE as vacuum does (`vacuum.c` :2403-2431).

use std::io;

use crate::Oid;
use crate::heap_rewrite::heap_state::{HeapState, editor, invalid, spage_err};
use crate::heap_rewrite::overflow;
use crate::heap_rewrite::record_patch::{residue_free_overflow_header, with_header};
use crate::page::db_area;
use crate::record::HomeKind;
use crate::record::heap_chain::HeapChain;
use crate::record::mvcc_header::RecordHeader;
use crate::spage::{RecordType, SPAGE_HEADER_SIZE, SPAGE_SLOT_SIZE, SlottedPage, SpageError};
use crate::value::DecodeError;

/// develop `heap_Maxslotted_reclength`: longer records go to overflow pages (16276 − 40 at 16 KiB: 16236).
pub fn max_slotted_len(db_pagesize: usize) -> usize {
    let max_record = db_pagesize - SPAGE_HEADER_SIZE - SPAGE_SLOT_SIZE;
    (max_record - HeapChain::SIZE - 8 * SPAGE_SLOT_SIZE) / 4 * 4
}

fn decode_err(oid: Oid, e: DecodeError) -> io::Error {
    invalid(format!("{oid:?}: {e}"))
}

/// The record in slot `oid` with its type; `None` for an empty (deleted) slot.
pub(crate) fn slot_record(st: &HeapState, oid: Oid) -> io::Result<Option<(RecordType, Vec<u8>)>> {
    st.peek(oid.vpid(), |p| {
        let sp = SlottedPage::new(db_area(p))
            .ok_or_else(|| invalid(format!("{oid:?}: not a slotted page")))?;
        Ok(sp.record(oid.slotid as usize).map(|(t, b)| (t, b.to_vec())))
    })
}

/// The home slot of `oid`: its record type and bytes.
pub(crate) fn home_slot(st: &HeapState, oid: Oid) -> io::Result<(RecordType, Vec<u8>)> {
    slot_record(st, oid)?.ok_or_else(|| invalid(format!("{oid:?}: empty home slot")))
}

/// Releases what the home slot of `oid` points at: the NEWHOME slot of a REC_RELOCATION, the overflow chain of a
/// REC_BIGONE. Returns overflow pages released.
pub(crate) fn release_target(st: &mut HeapState, oid: Oid) -> io::Result<usize> {
    let (t, body) = home_slot(st, oid)?;
    match t {
        RecordType::Home => Ok(0),
        RecordType::Relocation => {
            let nh = Oid::read(&body, 0);
            st.edit(nh.vpid(), |p| {
                editor(p, nh.vpid())?
                    .vacuum_slot(nh.slotid as usize, true)
                    .map_err(|e| spage_err(nh.vpid(), e))
            })?;
            Ok(0)
        }
        RecordType::BigOne => {
            overflow::release_chain(st.space, st.ovf_vfid, Oid::read(&body, 0).vpid())
        }
        t => Err(invalid(format!("{oid:?}: home slot holds {t:?}"))),
    }
}

/// Rewrites the home slot of `oid` with `body` and record type `rtype`.
fn set_home(
    st: &mut HeapState,
    oid: Oid,
    rtype: RecordType,
    body: &[u8],
) -> io::Result<Result<(), SpageError>> {
    let vpid = oid.vpid();
    st.edit(vpid, |p| {
        let mut ed = editor(p, vpid)?;
        let slot = oid.slotid as usize;
        Ok(ed
            .update(slot, body)
            .and_then(|()| ed.set_record_type(slot, rtype)))
    })
}

/// Places `rec` (whole record, header as it should be stored for a slotted record) under `oid`. `mvcc` says the
/// class is MVCC, which decides the overflow header form.
pub(crate) fn place(
    st: &mut HeapState,
    oid: Oid,
    rec: &[u8],
    mvcc: bool,
) -> io::Result<(HomeKind, usize)> {
    let released = release_target(st, oid)?;
    if rec.len() > max_slotted_len(st.db_pagesize()) {
        let big = if mvcc {
            let h = RecordHeader::read(rec).map_err(|e| decode_err(oid, e))?;
            with_header(rec, &h, &residue_free_overflow_header(&h))
        } else {
            rec.to_vec()
        };
        if st.ovf_vfid.is_null() {
            st.ovf_vfid = overflow::create_file(st.space, st.hfid, st.class_oid)?;
        }
        let first = overflow::write_chain(st.space, st.ovf_vfid, &big)?;
        set_home(st, oid, RecordType::BigOne, &overflow::home_pointer(first))?
            .map_err(|e| spage_err(oid.vpid(), e))?;
        return Ok((
            HomeKind::Big {
                first_overflow: first,
            },
            released,
        ));
    }
    match set_home(st, oid, RecordType::Home, rec)? {
        Ok(()) => return Ok((HomeKind::Home, released)),
        Err(SpageError::DoesntFit) => {}
        Err(e) => return Err(spage_err(oid.vpid(), e)),
    }
    let newhome = st.insert_newhome(rec, oid.vpid())?;
    let mut ptr = [0u8; 8];
    newhome.write(&mut ptr, 0);
    set_home(st, oid, RecordType::Relocation, &ptr)?.map_err(|e| spage_err(oid.vpid(), e))?;
    Ok((HomeKind::Relocated { newhome }, released))
}
