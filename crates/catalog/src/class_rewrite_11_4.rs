//! Rewrites a 10.2, 11.2 or 11.3 user class object into the 11.4 class object that goes back into the same root-heap
//! slot.
//!
//! Input: the source class, its classification, the source line (10.2, or 11.4 for 11.2/11.3), the class's new
//! representation directory OID and the old → new BTID map of the rebuilt indexes. Output: the 11.4 [`ClassObject`]
//! (encode it with `class_object::encode(.., VersionLine::V11_4)`).
//! Rules: types, domains, the attribute order and the constraint info layout stay (an 11.4 target has NCHAR, CHAR and
//! NUMERIC as 10.2..11.3 write them, R8; an 11.3 DEDUPLICATE index keeps its hidden key, which the 11.4 B-tree builder
//! fills, workspace#530); the deferred indexes leave the properties and every BTID becomes the rebuilt one, as on the
//! same-version path; packed values (defaults, partition values) are read with the source line and written with
//! 11.4's (a 10.2 LZO1X string becomes LZ4); a re-encoded class loses its older representations, since every record
//! is rewritten in the current one; the master partition's expression stream gets 11.4 opcodes and language flags.
//! Source: workspace#552; R15 workspace#527 (an 11.4 target needs none of develop's type or layout changes);
//! workspace#482 decision 4 (a re-encoded class keeps its repid); workspace#486 (deferred indexes); ADR 0017
//! (partition stream: the source compile kept, only what the target cannot evaluate changed).

use fastmig_ondisk::class_object::{Attribute, ClassObject};
use fastmig_ondisk::partition_stream::transcode_partition_stream;
use fastmig_ondisk::value::{
    DecodeError, DecodeResult, OrReader, OrWriter, SetElement, Value, decode_tagged, encode_tagged,
};
use fastmig_ondisk::{Btid, Oid, VersionLine};

use crate::class_rewrite::err;
use crate::classify::ClassPlan;
use crate::same_version_class_rewrite::rewrite_properties;
use crate::source_classes::SourceClass;

const TO: VersionLine = VersionLine::V11_4;

/// A packed value (`or_put_value` with its domain) read as `from` wrote it and written in 11.4; empty stays empty.
fn map_packed_value(from: VersionLine, packed: &[u8]) -> DecodeResult<Vec<u8>> {
    if packed.is_empty() || from == TO {
        return Ok(packed.to_vec());
    }
    let (tag, v) = decode_tagged(from, &mut OrReader::new(packed), Some(packed.len()))?;
    let mut w = OrWriter::new();
    encode_tagged(TO, &mut w, &tag, &v);
    Ok(w.buf)
}

fn map_attribute(from: VersionLine, a: &Attribute) -> DecodeResult<Attribute> {
    let ctx = |e: DecodeError| {
        err(format!(
            "attribute {}: {}",
            String::from_utf8_lossy(&a.name),
            e.what
        ))
    };
    Ok(Attribute {
        default_value: map_packed_value(from, &a.default_value).map_err(ctx)?,
        original_value: map_packed_value(from, &a.original_value).map_err(ctx)?,
        ..a.clone()
    })
}

/// Partition values of one partition element: the master's expression stream (element 2) transcoded in its CHAR
/// container, every value written in 11.4.
fn map_pvalues(from: VersionLine, packed: &[u8], master: bool) -> DecodeResult<Vec<u8>> {
    if packed.is_empty() {
        return Ok(Vec::new());
    }
    let (tag, mut v) = decode_tagged(from, &mut OrReader::new(packed), Some(packed.len()))?;
    if master
        && let Value::Set(s) = &mut v
        && let Some(e) = s.elements.get_mut(2)
    {
        let raw = match &e.value {
            Value::Char(b) | Value::Varchar(b) => b,
            x => return Err(err(format!("partition stream container {x:?}"))),
        };
        // the stream is [int 0][int body_size] + body; a CHAR container pads it with spaces
        let body = raw
            .get(4..8)
            .and_then(|b| b.first_chunk::<4>())
            .map_or(0, |b| i32::from_be_bytes(*b));
        let len = 8 + usize::try_from(body).map_err(|_| err("negative stream body size"))?;
        let stream = raw
            .get(..len)
            .ok_or_else(|| err("partition stream shorter than its header"))?;
        let out = transcode_partition_stream(from, TO, stream)?;
        // the engine stores the stream as CHAR(length) (`db_make_char`, release/11.4 execute_schema.c:15511)
        let tag = e.tag.clone().map(|mut t| {
            if !t.builtin {
                t.precision = out.len() as i32;
            }
            t
        });
        *e = SetElement {
            tag,
            value: Value::Char(out),
        };
    }
    let mut w = OrWriter::new();
    encode_tagged(TO, &mut w, &tag, &v);
    Ok(w.buf)
}

/// The 11.4 class object of a 10.2..11.3 user class (see the module header).
pub fn rewrite_user_class_11_4(
    src: &SourceClass,
    plan: &ClassPlan,
    from: VersionLine,
    rep_dir: Oid,
    new_btid: &dyn Fn(Btid) -> Option<Btid>,
) -> DecodeResult<ClassObject> {
    let c = &src.class;
    let mut o = c.clone();
    o.rep_dir = rep_dir;
    for (out, atts) in [
        (&mut o.attributes, &c.attributes),
        (&mut o.shared_attributes, &c.shared_attributes),
        (&mut o.class_attributes, &c.class_attributes),
    ] {
        *out = atts
            .iter()
            .map(|a| map_attribute(from, a))
            .collect::<DecodeResult<_>>()?;
    }
    if plan.re_encoded {
        o.representations.clear();
    }
    o.properties = rewrite_properties(c, from, TO, new_btid)?;
    for p in &mut o.partitions {
        p.pvalues = map_pvalues(from, &p.pvalues, p.pname.is_none())?;
    }
    Ok(o)
}
