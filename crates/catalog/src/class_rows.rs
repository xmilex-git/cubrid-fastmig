//! Reads the live rows of one class as attribute-name → value maps, for the catalog rows regeneration takes
//! values from (`_db_stored_procedure`, `_db_serial`, `_db_user`, ...).
//!
//! Input: the volume set, the release line and the class. Output: per live row its OID and its values by name.
//! Source: P1b record codec (`RecordLayout::for_repid`, `decode_record`); a committed DELID is a deleted row
//! (the source is shut down cleanly, R2 § 3 R3).

use std::collections::HashMap;
use std::io;

use fastmig_ondisk::record::{RecordHeader, RecordLayout, decode_record, heap_records};
use fastmig_ondisk::value::Value;
use fastmig_ondisk::volume_set::VolumeSet;
use fastmig_ondisk::{Oid, VersionLine};

use crate::source_classes::SourceClass;

pub struct Row {
    pub oid: Oid,
    pub values: HashMap<String, Value>,
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

pub fn read_rows(vs: &VolumeSet, line: VersionLine, c: &SourceClass) -> io::Result<Vec<Row>> {
    let name = String::from_utf8_lossy(&c.class.name).into_owned();
    let mut out = Vec::new();
    for r in heap_records(vs, c.class.heap) {
        let r = r?;
        let h = RecordHeader::read(&r.bytes)
            .map_err(|e| invalid(format!("{name} {:?}: {e}", r.oid)))?;
        if h.is_deleted() {
            continue;
        }
        let layout = RecordLayout::for_repid(line, &c.class, h.repid)
            .map_err(|e| invalid(format!("{name} repid {}: {e}", h.repid)))?;
        let d = decode_record(line, &layout, &r.bytes)
            .map_err(|e| invalid(format!("{name} {:?}: {e}", r.oid)))?;
        let values = layout
            .attrs
            .iter()
            .zip(d.values)
            .map(|(a, v)| {
                let n = c
                    .class
                    .attributes
                    .iter()
                    .find(|x| x.id == a.id)
                    .map_or_else(
                        || format!("#{}", a.id),
                        |x| String::from_utf8_lossy(&x.name).into_owned(),
                    );
                (n, v)
            })
            .collect();
        out.push(Row { oid: r.oid, values });
    }
    Ok(out)
}
