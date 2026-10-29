//! Writes a class object as develop's `tf_class_to_disk` would: the record that regeneration puts back in the
//! class's root-heap slot.
//!
//! Input: a [`ClassObject`] already in develop shape (develop type ids, constraint sequences, partition type).
//! Output: the record bytes.
//! Source: engine develop @5b1867174 `transform_cl.c` `tf_class_to_disk`, `put_class_varinfo`,
//! `put_class_attributes`, `put_substructure_set` (header `SEQUENCE | VARIABLE | DOMAIN | COMMON_SUB`, element
//! offsets from the set start, domain length 4, `DB_TYPE_SUB` word, sub header = metaclass OID
//! {`META_PAGE_*`, 0, `META_VOLUME` 256}, repid 0, flags 0), `put_object_set` (header `SEQUENCE | DOMAIN`, domain
//! length 4, the built-in OBJECT domain), and the `*_to_disk` writers named in each substructure's module.
//! Strings are written by `value::write_string` (develop's compression rule; LZ4 bytes may differ from the
//! engine's, the decompressed string is the same, workspace#482 decision 9).

use super::attribute::Attribute;
use super::class_record::{ClassObject, ClassType};
use super::domain::SchemaDomain;
use super::method::{Method, MethodArg};
use super::partition::Partition;
use super::representation::Representation;
use crate::value::{DbType, Domain, OrWriter, encode_packed_domain, write_string};
use crate::{Oid, VersionLine};

/// `transform.h` `META_PAGE_*`: the page id of each metaclass OID.
mod meta_page {
    pub const REPRESENTATION: i32 = 2;
    pub const RESOLUTION: i32 = 3;
    pub const DOMAIN: i32 = 4;
    pub const ATTRIBUTE: i32 = 5;
    pub const METHARG: i32 = 6;
    pub const METHSIG: i32 = 7;
    pub const METHOD: i32 = 8;
    pub const METHFILE: i32 = 9;
    pub const REPATTRIBUTE: i32 = 10;
    pub const QUERY_SPEC: i32 = 11;
    pub const PARTITION: i32 = 12;
}
const META_VOLUME: i16 = 256;
const SET_SEQUENCE: u32 = 8;
const OR_SET_VARIABLE_BIT: u32 = 0x20000;
const OR_SET_DOMAIN_BIT: u32 = 0x40000;
const OR_SET_COMMON_SUB_BIT: u32 = 0x100000;
const DB_TYPE_SUB: u32 = 15;
/// `OR_OFFSET_SIZE_4BYTE` in the header word's offset-size bits.
const OFFSET_SIZE_4BYTE: u32 = 0x6000_0000;

fn oid(w: &mut OrWriter, o: Oid) {
    w.i32(o.pageid);
    w.i16(o.slotid);
    w.i16(o.volid);
}

fn string(s: &Option<Vec<u8>>) -> Vec<u8> {
    let mut w = OrWriter::new();
    if let Some(s) = s {
        write_string(&mut w, s);
    }
    w.buf
}

/// A structure: variable offset table, the fixed area, then the variable fields.
fn structure(fixed: OrWriter, vars: &[Vec<u8>]) -> Vec<u8> {
    let mut w = OrWriter::new();
    let mut off = 4 * (vars.len() + 1) + fixed.len();
    for v in vars {
        w.i32(off as i32);
        off += v.len();
    }
    w.i32(off as i32);
    w.bytes(&fixed.buf);
    for v in vars {
        w.bytes(v);
    }
    w.buf
}

/// A substructure set; an empty list is stored as nothing.
fn sub_set(elems: Vec<Vec<u8>>, meta_page: i32) -> Vec<u8> {
    if elems.is_empty() {
        return Vec::new();
    }
    let mut w = OrWriter::new();
    w.u32(SET_SEQUENCE | OR_SET_VARIABLE_BIT | OR_SET_DOMAIN_BIT | OR_SET_COMMON_SUB_BIT);
    w.i32(elems.len() as i32);
    let table = w.len();
    w.zeros(4 * (elems.len() + 1));
    w.i32(4);
    w.u32(DB_TYPE_SUB);
    oid(&mut w, Oid::new(META_VOLUME, meta_page, 0));
    w.i32(0);
    w.i32(0);
    for (i, e) in elems.iter().enumerate() {
        let at = w.len() as i32;
        w.patch_i32(table + 4 * i, at);
        w.bytes(e);
    }
    let end = w.len() as i32;
    w.patch_i32(table + 4 * elems.len(), end);
    w.buf
}

/// An OID set; an empty list is stored as nothing.
fn oid_set(oids: &[Oid]) -> Vec<u8> {
    if oids.is_empty() {
        return Vec::new();
    }
    let mut w = OrWriter::new();
    w.u32(SET_SEQUENCE | OR_SET_DOMAIN_BIT);
    w.i32(oids.len() as i32);
    w.i32(4);
    encode_packed_domain(
        VersionLine::Develop,
        &mut w,
        &[Domain::builtin(VersionLine::Develop, DbType::Object)],
        false,
        false,
    );
    for o in oids {
        oid(&mut w, *o);
    }
    w.buf
}

fn domain(d: &SchemaDomain) -> Vec<u8> {
    let mut f = OrWriter::new();
    f.i32(d.type_id);
    f.i32(d.precision);
    f.i32(d.scale);
    f.i32(d.codeset);
    f.i32(d.collation_id);
    oid(&mut f, d.class_oid);
    let mut enumeration = OrWriter::new();
    if !d.enumeration.is_empty() {
        enumeration.u32(SET_SEQUENCE);
        enumeration.i32(d.enumeration.len() as i32);
        for s in &d.enumeration {
            write_string(&mut enumeration, s);
        }
    }
    structure(
        f,
        &[
            domains(&d.set_domain),
            enumeration.buf,
            string(&d.json_schema),
        ],
    )
}

fn domains(ds: &[SchemaDomain]) -> Vec<u8> {
    sub_set(ds.iter().map(domain).collect(), meta_page::DOMAIN)
}

fn attribute(a: &Attribute) -> Vec<u8> {
    let mut f = OrWriter::new();
    f.i32(a.id);
    f.i32(a.type_id);
    f.i32(0); // memory offset, computed after loading
    f.i32(a.order);
    oid(&mut f, a.class_oid);
    f.i32(a.flags);
    f.i32(-1); // NULL_FILEID
    f.i32(-1); // NULL_PAGEID
    f.i32(0);
    structure(
        f,
        &[
            string(&Some(a.name.clone())),
            a.default_value.clone(),
            a.original_value.clone(),
            domains(&a.domain),
            oid_set(&a.triggers),
            a.properties.clone(),
            string(&a.comment),
        ],
    )
}

fn attributes(atts: &[Attribute]) -> Vec<u8> {
    sub_set(atts.iter().map(attribute).collect(), meta_page::ATTRIBUTE)
}

fn representation(r: &Representation) -> Vec<u8> {
    let mut f = OrWriter::new();
    f.i32(r.id);
    f.i32(r.fixed_count);
    f.i32(r.variable_count);
    f.i32(0);
    let atts = r
        .attributes
        .iter()
        .map(|a| {
            let mut f = OrWriter::new();
            f.i32(a.id);
            f.i32(a.type_id);
            structure(f, &[domains(&a.domain)])
        })
        .collect();
    structure(f, &[sub_set(atts, meta_page::REPATTRIBUTE), Vec::new()])
}

fn method_args(args: &[MethodArg]) -> Vec<u8> {
    let elems = args
        .iter()
        .map(|a| {
            let mut f = OrWriter::new();
            f.i32(a.type_id);
            f.i32(a.index);
            structure(f, &[domains(&a.domain)])
        })
        .collect();
    sub_set(elems, meta_page::METHARG)
}

fn methods(ms: &[Method]) -> Vec<u8> {
    let elems = ms
        .iter()
        .map(|m| {
            let mut f = OrWriter::new();
            oid(&mut f, m.class_oid);
            f.i32(m.id);
            let sigs = m
                .signatures
                .iter()
                .map(|s| {
                    let mut f = OrWriter::new();
                    f.i32(s.num_args);
                    structure(
                        f,
                        &[
                            string(&s.function_name),
                            string(&s.sql_definition),
                            method_args(&s.return_value),
                            method_args(&s.arguments),
                        ],
                    )
                })
                .collect();
            structure(
                f,
                &[
                    string(&Some(m.name.clone())),
                    sub_set(sigs, meta_page::METHSIG),
                    m.properties.clone(),
                ],
            )
        })
        .collect();
    sub_set(elems, meta_page::METHOD)
}

fn partition(p: &Partition) -> Vec<u8> {
    let mut f = OrWriter::new();
    f.i32(p.ptype);
    // DB_PARTITIONED_CLASS (1) on the partitioned class, DB_PARTITION_CLASS (2) on a partition (R3 § 2.3)
    f.i32(
        p.class_partition_type
            .unwrap_or(if p.pname.is_none() { 1 } else { 2 }),
    );
    structure(
        f,
        &[
            string(&p.pname),
            string(&p.pexpr),
            p.pvalues.clone(),
            string(&p.comment),
        ],
    )
}

/// The develop record of `c`.
pub fn encode_develop(c: &ClassObject) -> Vec<u8> {
    let mut f = OrWriter::new();
    f.i32(c.att_ids);
    f.i32(c.method_ids);
    oid(&mut f, c.rep_dir);
    f.i32(c.heap.vfid.fileid);
    f.i32(c.heap.vfid.volid as i32);
    f.i32(c.heap.pageid);
    f.i32(c.fixed_count);
    f.i32(c.variable_count);
    f.i32(c.fixed_size);
    f.i32(c.att_count);
    f.i32(0); // object_size
    f.i32(c.shared_count);
    f.i32(c.method_count);
    f.i32(c.class_method_count);
    f.i32(c.class_attribute_count);
    f.i32(c.flags);
    f.i32(match c.class_type {
        ClassType::Class => 0,
        ClassType::Vclass => 1,
        ClassType::Adt => 2,
        ClassType::Other(v) => v,
    });
    oid(&mut f, c.owner);
    f.i32(c.collation_id);
    f.i32(c.tde_algorithm);

    let query_specs = c
        .query_specs
        .iter()
        .map(|q| structure(OrWriter::new(), &[string(&Some(q.clone()))]))
        .collect();
    let method_files = c
        .method_files
        .iter()
        .map(|m| {
            let mut f = OrWriter::new();
            oid(&mut f, m.class_oid);
            structure(f, &[string(&m.name), Vec::new()])
        })
        .collect();
    let resolutions = c
        .resolutions
        .iter()
        .map(|r| {
            let mut f = OrWriter::new();
            oid(&mut f, r.class_oid);
            f.i32(r.name_space);
            structure(f, &[string(&r.name), string(&r.alias)])
        })
        .collect();
    let body = structure(
        f,
        &[
            string(&Some(c.name.clone())),
            string(&c.loader_commands),
            sub_set(
                c.representations.iter().map(representation).collect(),
                meta_page::REPRESENTATION,
            ),
            oid_set(&c.sub_classes),
            oid_set(&c.super_classes),
            attributes(&c.attributes),
            attributes(&c.shared_attributes),
            attributes(&c.class_attributes),
            methods(&c.methods),
            methods(&c.class_methods),
            sub_set(method_files, meta_page::METHFILE),
            sub_set(resolutions, meta_page::RESOLUTION),
            sub_set(query_specs, meta_page::QUERY_SPEC),
            oid_set(&c.triggers),
            c.properties.clone(),
            string(&c.comment),
            sub_set(
                c.partitions.iter().map(partition).collect(),
                meta_page::PARTITION,
            ),
        ],
    );
    let mut w = OrWriter::new();
    w.u32(c.repid as u32 | OFFSET_SIZE_4BYTE);
    w.i32(c.chn);
    w.bytes(&body);
    w.buf
}
