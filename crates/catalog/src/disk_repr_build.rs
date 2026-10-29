//! Builds a class's disk-representation record from its develop class object (R3 rule E1), the record the new
//! catalog file holds for the class's current repid.
//!
//! Input: the develop class object and, per attribute id, the BTIDs of the indexes whose first key is that
//! attribute (with the function-index flag). Output: [`DiskRepr`] with empty B-tree statistics (rule E2).
//! Source: engine develop @5b1867174 `object_representation_sr.c:orc_diskrep_from_record` and
//! `or_get_current_representation` (attributes in stored order; the first `fixed_count` are fixed, located at the
//! running sum of `tp_domain_disk_size`; the rest are located by index; `position` = stored order; the default
//! value is the original value's bytes after its packed domain, `or_get_default_value`); R3 § 5 E1-E2.

use fastmig_ondisk::catalog_dir::{BtStats, DiskAttr, DiskRepr};
use fastmig_ondisk::class_object::ClassObject;
use fastmig_ondisk::value::{DecodeError, DecodeResult, OrReader, decode_value_tag};
use fastmig_ondisk::{Btid, VersionLine};

/// `btids(attr_id)` gives that attribute's indexes as `(btid, is function index on the first key)`.
pub fn build(c: &ClassObject, btids: impl Fn(i32) -> Vec<(Btid, bool)>) -> DecodeResult<DiskRepr> {
    let line = VersionLine::Develop;
    let n_fixed = usize::try_from(c.fixed_count).unwrap_or(0);
    let mut fixed = Vec::new();
    let mut variable = Vec::new();
    let mut offset = 0i32;
    for (i, a) in c.attributes.iter().enumerate() {
        let location = if i < n_fixed {
            let d = a
                .domain
                .first()
                .and_then(|d| d.to_value_domain(line))
                .ok_or_else(|| err(&a.name, "no develop domain"))?;
            let size = d
                .disk_size(line)
                .ok_or_else(|| err(&a.name, "a fixed attribute with a variable-size domain"))?;
            let at = offset;
            offset += size as i32;
            at
        } else {
            (i - n_fixed) as i32
        };
        let value = default_value(&a.original_value, line)?;
        let attr = DiskAttr {
            id: a.id,
            location,
            type_id: a.type_id,
            position: i as i32,
            class_oid: a.class_oid,
            ndv: 0,
            value,
            bt_stats: btids(a.id)
                .into_iter()
                .map(|(b, f)| BtStats::empty(b, f))
                .collect(),
        };
        if i < n_fixed {
            fixed.push(attr);
        } else {
            variable.push(attr);
        }
    }
    Ok(DiskRepr {
        id: c.repid,
        fixed_length: c.fixed_size,
        fixed,
        variable,
    })
}

/// The bytes after the packed domain; none for an empty or NULL value.
fn default_value(packed: &[u8], line: VersionLine) -> DecodeResult<Vec<u8>> {
    if packed.is_empty() {
        return Ok(Vec::new());
    }
    let mut r = OrReader::new(packed);
    let is_null = decode_value_tag(line, &mut r)?.is_null;
    Ok(if is_null {
        Vec::new()
    } else {
        packed[r.pos()..].to_vec()
    })
}

fn err(name: &[u8], what: &str) -> DecodeError {
    DecodeError {
        at: 0,
        what: format!("attribute {}: {what}", String::from_utf8_lossy(name)),
    }
}
