//! Reads every class object of a database and the rows of one class, the input of catalog regeneration.
//!
//! Input: an open volume set and its release line. Output: each class object with its OID, decoded; or the raw
//! records of one class's heap.
//! Source: engine `boot_sr.c` (`BOOT_DB_PARM.rootclass_hfid` / `rootclass_oid`: the root heap holds one record
//! per class plus the root class's own record, which is not a class object), `transform_cl.c:tf_disk_to_class`;
//! R3 § 2. Root-heap records are not MVCC (`mvcc.c:687-691`), so their bytes go to the decoder whole.

use std::io;

use fastmig_ondisk::boot_parm::BootDbParm;
use fastmig_ondisk::class_object::{self, ClassObject};
use fastmig_ondisk::record::{HeapRecord, heap_records};
use fastmig_ondisk::volume_set::VolumeSet;
use fastmig_ondisk::{Hfid, Oid, VersionLine};

/// One class object and the OID of its root-heap record (the class OID).
#[derive(Clone, Debug)]
pub struct SourceClass {
    pub oid: Oid,
    pub class: ClassObject,
    /// The record bytes as stored.
    pub record: Vec<u8>,
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Every class object, in root-heap order.
pub fn read_classes(vs: &VolumeSet, line: VersionLine) -> io::Result<Vec<SourceClass>> {
    let (_, boot) = BootDbParm::read(vs)?;
    let mut out = Vec::new();
    for r in heap_records(vs, boot.rootclass_hfid) {
        let r = r?;
        if r.oid == boot.rootclass_oid {
            continue;
        }
        let class = class_object::decode(&r.bytes, line)
            .map_err(|e| invalid(format!("class object {:?}: {e}", r.oid)))?;
        out.push(SourceClass {
            oid: r.oid,
            class,
            record: r.bytes,
        });
    }
    Ok(out)
}

/// The records of one heap, in heap order. MVCC visibility is the caller's.
pub fn read_rows(vs: &VolumeSet, hfid: Hfid) -> io::Result<Vec<HeapRecord>> {
    heap_records(vs, hfid).collect()
}
