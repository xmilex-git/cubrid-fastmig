//! Derives the rows of the 11 ct tables (`_db_class`, `_db_attribute`, `_db_domain`, `_db_method`, `_db_meth_sig`,
//! `_db_meth_arg`, `_db_meth_file`, `_db_query_spec`, `_db_index`, `_db_index_key`, `_db_partition`) from the
//! target class objects, as the develop or 11.4 engine writes them when it creates each class.
//!
//! Input: every target class (develop class objects with their class OIDs), the migrate start time, and the
//! caller's row allocator (`table name -> new row OID`; the real page placement is P1 integration, workspace#484).
//! Output: the rows as `(table, Row)`; encode each with [`crate::row_encode::encode_row`].
//! Source: engine develop @682b080b2 `catalog_class.c`: `catcls_get_or_value_from_class` and the readers it calls
//! (attribute, domain, method, signature, argument, method file, resolution, query spec, partition),
//! `catcls_insert_instance` / `catcls_insert_subset` (each subset row's column 0 is its parent row; a parent column
//! holds its children's OIDs in order; in a direct child of a `_db_class` row an OID equal to that row is NULL; a
//! `DB_TYPE_VARIABLE` domain class becomes the tree's `_db_class` row), `catcls_convert_class_oid_to_oid` (a class
//! OID in a ct row is that class's `_db_class` row), `catcls_set_or_value_timestamps`; `transform.c` `ct_*_atts`
//! (column names); R4 § 6, § 7 R4-R5, R7; workspace#482 decision 8 (created/updated time = migrate start). An 11.4
//! target (release/11.4 `catalog_class.c`, `transform.c`): the same rows without the columns develop added, and
//! `_db_class.is_system_class` holding every class flag (workspace#552).

use std::collections::HashMap;

use fastmig_ondisk::Oid;
use fastmig_ondisk::VersionLine;
use fastmig_ondisk::class_object::{
    Attribute, ClassObject, ClassType, Method, MethodArg, MethodSig, SchemaDomain,
};
use fastmig_ondisk::value::{
    DbType, DecodeError, DecodeResult, Domain, OrReader, SetElement, SetValue, Value, decode_tagged,
};

use crate::attr_default::default_value;
use crate::class_rows::Row;
use crate::ct_index_rows::index_rows;
use crate::system_table::SystemTable;
use crate::target_classes::TargetClass;

use fastmig_ondisk::class_object::CLASSFLAG_SYSTEM;
/// The ct and `_db_auth` columns develop added: an 11.4 target's tables lack them (`transform.c` `ct_*_atts`, release/11.4
/// against develop 682b080b2; the 11.4 and develop catalog templates compared, workspace#552).
const DEVELOP_ONLY_COLUMNS: [(SystemTable, &str); 18] = [
    (SystemTable::DbClass, "flags"),
    (SystemTable::DbClass, "created_time"),
    (SystemTable::DbClass, "updated_time"),
    (SystemTable::DbAttribute, "flags"),
    (SystemTable::DbIndex, "referential_class"),
    (SystemTable::DbIndex, "delete_rule"),
    (SystemTable::DbIndex, "update_rule"),
    (SystemTable::DbIndex, "referential_match_option"),
    (SystemTable::DbIndex, "index_type"),
    (SystemTable::DbIndex, "options"),
    (SystemTable::DbIndex, "created_time"),
    (SystemTable::DbIndex, "updated_time"),
    (SystemTable::DbPartition, "class_partition_type"),
    (SystemTable::DbQuerySpec, "invalidated_time"),
    (SystemTable::DbClass, "checked_time"),
    (SystemTable::DbClass, "statistics_strategy"),
    (SystemTable::DbAuth, "created_time"),
    (SystemTable::DbAuth, "updated_time"),
];
/// Engine `SM_ATTFLAG_*` and the `DB_ATTOPT_*` bits `catcls_filter_attflag` maps them to.
const ATTFLAG_NON_NULL: i32 = 4;
const ATTFLAG_AUTO_INCREMENT: i32 = 256;
const ATTFLAG_PARTITION_KEY: i32 = 1024;
const ATTFLAG_INVISIBLE_COLUMN: i32 = 2048;
/// `catcls_apply_component_type`: instance 0, class 1, shared 2.
const INSTANCE: i32 = 0;
const CLASS: i32 = 1;
const SHARED: i32 = 2;

/// Where generated rows go: the target line, the allocator and the rows so far.
pub(crate) struct Sink<'a> {
    pub line: VersionLine,
    pub rows: Vec<(SystemTable, Row)>,
    pub alloc: &'a mut dyn FnMut(SystemTable) -> Oid,
}

impl Sink<'_> {
    /// Keeps the columns the target line's table has.
    pub fn emit(&mut self, table: SystemTable, oid: Oid, values: Vec<(&str, Value)>) {
        let develop = self.line == VersionLine::Develop;
        let values = values
            .into_iter()
            .filter(|(n, _)| develop || !DEVELOP_ONLY_COLUMNS.contains(&(table, n)))
            .map(|(n, v)| (n.to_string(), v))
            .collect();
        self.rows.push((table, Row { oid, values }));
    }
}

pub(crate) fn int(i: i32) -> Value {
    Value::Integer(i)
}

pub(crate) fn text(s: &[u8]) -> Value {
    Value::Varchar(s.to_vec())
}

pub(crate) fn opt_text(s: Option<&[u8]>) -> Value {
    s.map_or(Value::Null, text)
}

pub(crate) fn obj(o: Oid) -> Value {
    if o.is_null() {
        Value::Null
    } else {
        Value::Object(o)
    }
}

/// A sequence column the way catcls writes it: the set carries its built-in domain and tags each element (the
/// built-in SEQUENCE, OBJECT, VARCHAR and INTEGER domains are the same in 11.4 and develop).
pub(crate) fn seq(ty: DbType, values: Vec<Value>) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    Value::Set(SetValue {
        ty: DbType::Sequence,
        domain: Some(Domain::builtin(VersionLine::Develop, DbType::Sequence)),
        elements: values
            .into_iter()
            .map(|value| SetElement {
                tag: Some(Domain::builtin(VersionLine::Develop, ty)),
                value,
            })
            .collect(),
    })
}

pub(crate) fn oid_seq(oids: &[Oid]) -> Value {
    seq(
        DbType::Object,
        oids.iter().map(|o| Value::Object(*o)).collect(),
    )
}

fn err(what: String) -> DecodeError {
    DecodeError { at: 0, what }
}

/// One class's tree: its `_db_class` row and the class-OID → `_db_class`-row map of the whole catalog.
struct Tree<'a> {
    class_row: &'a HashMap<Oid, Oid>,
    /// This class's `_db_class` row, the root of every row below it.
    root: Oid,
}

impl Tree<'_> {
    /// `catcls_convert_class_oid_to_oid`; NULL stays NULL.
    fn conv(&self, class: Oid) -> DecodeResult<Value> {
        if class.is_null() {
            return Ok(Value::Null);
        }
        self.class_row
            .get(&class)
            .map(|r| Value::Object(*r))
            .ok_or_else(|| err(format!("class {class:?} has no _db_class row")))
    }

    /// A class reference in a direct child of the `_db_class` row: the row's own class becomes NULL.
    fn origin_class(&self, class: Oid) -> DecodeResult<Value> {
        Ok(match self.conv(class)? {
            Value::Object(o) if o == self.root => Value::Null,
            v => v,
        })
    }

    fn domains(&self, s: &mut Sink, parent: Oid, ds: &[SchemaDomain]) -> DecodeResult<Value> {
        let mut oids = Vec::with_capacity(ds.len());
        for d in ds {
            let me = (s.alloc)(SystemTable::DbDomain);
            oids.push(me);
            // a class missing from the catalog (self reference while being created, or deleted) is the root
            let class_of = if d.class_oid.is_null() {
                Value::Null
            } else {
                Value::Object(
                    self.class_row
                        .get(&d.class_oid)
                        .copied()
                        .unwrap_or(self.root),
                )
            };
            let enumeration = seq(
                DbType::Varchar,
                d.enumeration.iter().map(|e| text(e)).collect(),
            );
            let set_domains = self.domains(s, me, &d.set_domain)?;
            s.emit(
                SystemTable::DbDomain,
                me,
                vec![
                    ("object_of", Value::Object(parent)),
                    ("data_type", int(d.type_id)),
                    ("prec", int(d.precision)),
                    ("scale", int(d.scale)),
                    ("code_set", int(d.codeset)),
                    ("collation_id", int(d.collation_id)),
                    ("class_of", class_of),
                    ("enumeration", enumeration),
                    ("set_domains", set_domains),
                    ("json_schema", opt_text(d.json_schema.as_deref())),
                ],
            );
        }
        Ok(oid_seq(&oids))
    }

    fn attributes(
        &self,
        s: &mut Sink,
        c: &ClassObject,
        atts: &[Attribute],
        kind: i32,
    ) -> DecodeResult<Value> {
        let mut oids = Vec::with_capacity(atts.len());
        for a in atts {
            let me = (s.alloc)(SystemTable::DbAttribute);
            oids.push(me);
            let ctx = |e: DecodeError| {
                err(format!(
                    "attribute {}: {}",
                    String::from_utf8_lossy(&a.name),
                    e.what
                ))
            };
            let default = default_value(a, s.line).map_err(ctx)?;
            let domains = self.domains(s, me, &a.domain)?;
            let flags = (a.flags & ATTFLAG_AUTO_INCREMENT != 0) as i32
                | ((a.flags & ATTFLAG_INVISIBLE_COLUMN != 0) as i32) << 1
                | ((a.flags & ATTFLAG_PARTITION_KEY != 0) as i32) << 2;
            s.emit(
                SystemTable::DbAttribute,
                me,
                vec![
                    ("class_of", Value::Object(self.root)),
                    ("attr_name", text(&a.name)),
                    ("attr_type", int(kind)),
                    ("from_attr_name", resolved_name(c, &a.name, kind)),
                    ("data_type", int(a.type_id)),
                    ("def_order", int(a.order)),
                    ("from_class_of", self.origin_class(a.class_oid)?),
                    ("is_nullable", int((a.flags & ATTFLAG_NON_NULL == 0) as i32)),
                    (
                        "default_value",
                        opt_text(default.as_deref().map(str::as_bytes)),
                    ),
                    ("domains", domains),
                    ("flags", int(flags)),
                    ("comment", opt_text(a.comment.as_deref())),
                ],
            );
        }
        Ok(oid_seq(&oids))
    }

    fn method_args(&self, s: &mut Sink, sig: Oid, args: &[MethodArg]) -> DecodeResult<Value> {
        let mut oids = Vec::with_capacity(args.len());
        for a in args {
            let me = (s.alloc)(SystemTable::DbMethArg);
            oids.push(me);
            let domains = self.domains(s, me, &a.domain)?;
            s.emit(
                SystemTable::DbMethArg,
                me,
                vec![
                    ("meth_sig_of", Value::Object(sig)),
                    ("data_type", int(a.type_id)),
                    ("index_of", int(a.index)),
                    ("domains", domains),
                ],
            );
        }
        Ok(oid_seq(&oids))
    }

    fn signatures(&self, s: &mut Sink, meth: Oid, sigs: &[MethodSig]) -> DecodeResult<Value> {
        let mut oids = Vec::with_capacity(sigs.len());
        for g in sigs {
            let me = (s.alloc)(SystemTable::DbMethSig);
            oids.push(me);
            let return_value = self.method_args(s, me, &g.return_value)?;
            let arguments = self.method_args(s, me, &g.arguments)?;
            s.emit(
                SystemTable::DbMethSig,
                me,
                vec![
                    ("meth_of", Value::Object(meth)),
                    ("arg_count", int(g.num_args)),
                    ("func_name", opt_text(g.function_name.as_deref())),
                    ("return_value", return_value),
                    ("arguments", arguments),
                ],
            );
        }
        Ok(oid_seq(&oids))
    }

    fn methods(
        &self,
        s: &mut Sink,
        c: &ClassObject,
        ms: &[Method],
        kind: i32,
    ) -> DecodeResult<Value> {
        let mut oids = Vec::with_capacity(ms.len());
        for m in ms {
            let me = (s.alloc)(SystemTable::DbMethod);
            oids.push(me);
            let signatures = self.signatures(s, me, &m.signatures)?;
            s.emit(
                SystemTable::DbMethod,
                me,
                vec![
                    ("class_of", Value::Object(self.root)),
                    ("meth_name", text(&m.name)),
                    ("meth_type", int(kind)),
                    ("from_meth_name", resolved_name(c, &m.name, kind)),
                    ("from_class_of", self.origin_class(m.class_oid)?),
                    ("signatures", signatures),
                ],
            );
        }
        Ok(oid_seq(&oids))
    }

    fn class_row(&self, s: &mut Sink, oid: Oid, c: &ClassObject, now: &Value) -> DecodeResult<()> {
        let name = &c.name;
        let class_name = match name.iter().position(|b| *b == b'.') {
            Some(dot) => &name[dot + 1..],
            None => name,
        };
        let class_type = match c.class_type {
            ClassType::Class => 0,
            ClassType::Vclass => 1,
            ClassType::Adt => 2,
            ClassType::Other(v) => v,
        };
        let conv_all = |list: &[Oid]| -> DecodeResult<Value> {
            let mut v = Vec::with_capacity(list.len());
            for o in list {
                v.push(self.conv(*o)?);
            }
            Ok(seq(DbType::Object, v))
        };
        // children in the column order of ct_class_atts, each allocated before its own children
        let inst_attrs = self.attributes(s, c, &c.attributes, INSTANCE)?;
        let shared_attrs = self.attributes(s, c, &c.shared_attributes, SHARED)?;
        let class_attrs = self.attributes(s, c, &c.class_attributes, CLASS)?;
        let inst_meths = self.methods(s, c, &c.methods, INSTANCE)?;
        let class_meths = self.methods(s, c, &c.class_methods, CLASS)?;
        let mut files = Vec::new();
        for f in &c.method_files {
            let me = (s.alloc)(SystemTable::DbMethFile);
            files.push(me);
            s.emit(
                SystemTable::DbMethFile,
                me,
                vec![
                    ("class_of", Value::Object(self.root)),
                    ("from_class_of", self.origin_class(f.class_oid)?),
                    ("path_name", opt_text(f.name.as_deref())),
                ],
            );
        }
        let mut specs = Vec::new();
        for q in &c.query_specs {
            let me = (s.alloc)(SystemTable::DbQuerySpec);
            specs.push(me);
            s.emit(
                SystemTable::DbQuerySpec,
                me,
                vec![("class_of", Value::Object(self.root)), ("spec", text(q))],
            );
        }
        let indexes = index_rows(s, self.root, c, now)?;
        let mut parts = Vec::new();
        for p in &c.partitions {
            let me = (s.alloc)(SystemTable::DbPartition);
            parts.push(me);
            let pvalues = if p.pvalues.is_empty() {
                Value::Null
            } else {
                decode_tagged(
                    s.line,
                    &mut OrReader::new(&p.pvalues),
                    Some(p.pvalues.len()),
                )?
                .1
            };
            s.emit(
                SystemTable::DbPartition,
                me,
                vec![
                    ("class_of", Value::Object(self.root)),
                    ("ptype", int(p.ptype)),
                    (
                        "class_partition_type",
                        p.class_partition_type.map_or(Value::Null, int),
                    ),
                    ("pname", opt_text(p.pname.as_deref())),
                    ("pexpr", opt_text(p.pexpr.as_deref())),
                    ("pvalues", pvalues),
                    ("comment", opt_text(p.comment.as_deref())),
                ],
            );
        }
        s.emit(
            SystemTable::DbClass,
            self.root,
            vec![
                ("class_of", Value::Object(oid)),
                ("inst_attr_count", int(c.att_count)),
                ("shared_attr_count", int(c.shared_count)),
                ("inst_meth_count", int(c.method_count)),
                ("class_meth_count", int(c.class_method_count)),
                ("class_attr_count", int(c.class_attribute_count)),
                // develop splits the flags: the system bit here, the rest in `flags`; 11.4 keeps them all here
                (
                    "is_system_class",
                    int(match s.line {
                        VersionLine::Develop => c.flags & CLASSFLAG_SYSTEM,
                        VersionLine::V10_2 | VersionLine::V11_4 => c.flags,
                    }),
                ),
                ("class_type", int(class_type)),
                ("owner", obj(c.owner)),
                ("collation_id", int(c.collation_id)),
                ("tde_algorithm", int(c.tde_algorithm)),
                ("flags", int(c.flags & !CLASSFLAG_SYSTEM)),
                ("created_time", now.clone()),
                ("updated_time", now.clone()),
                ("unique_name", text(name)),
                ("class_name", text(class_name)),
                ("sub_classes", conv_all(&c.sub_classes)?),
                ("super_classes", conv_all(&c.super_classes)?),
                ("inst_attrs", inst_attrs),
                ("shared_attrs", shared_attrs),
                ("class_attrs", class_attrs),
                ("inst_meths", inst_meths),
                ("class_meths", class_meths),
                ("meth_files", oid_seq(&files)),
                ("query_specs", oid_seq(&specs)),
                ("indexes", indexes),
                ("comment", opt_text(c.comment.as_deref())),
                ("partition", oid_seq(&parts)),
            ],
        );
        Ok(())
    }
}

/// `catcls_apply_resolutions`: the component's `from_*_name` when a resolution aliases it in its name space.
fn resolved_name(c: &ClassObject, name: &[u8], kind: i32) -> Value {
    // `catcls_resolution_space`: class components resolve in ID_CLASS (6), the rest in ID_INSTANCE (5)
    let space = if kind == CLASS { 6 } else { 5 };
    c.resolutions
        .iter()
        .rev()
        .find(|r| r.alias.as_deref() == Some(name) && r.name_space == space)
        .map_or(Value::Null, |r| opt_text(r.name.as_deref()))
}

/// The ct rows of every class, for a target of `line` (the class objects' line). `now` is the migrate start time (a
/// `Value::Datetime`).
pub fn ct_rows(
    classes: &[TargetClass],
    line: VersionLine,
    now: &Value,
    alloc: &mut dyn FnMut(SystemTable) -> Oid,
) -> DecodeResult<Vec<(SystemTable, Row)>> {
    let mut s = Sink {
        line,
        rows: Vec::new(),
        alloc,
    };
    // every _db_class row first: a class's rows name other classes by their _db_class row
    let class_row: HashMap<Oid, Oid> = classes
        .iter()
        .map(|t| (t.oid, (s.alloc)(SystemTable::DbClass)))
        .collect();
    for t in classes {
        let tree = Tree {
            class_row: &class_row,
            root: class_row[&t.oid],
        };
        tree.class_row(&mut s, t.oid, &t.class, now).map_err(|e| {
            err(format!(
                "{}: {}",
                String::from_utf8_lossy(&t.class.name),
                e.what
            ))
        })?;
    }
    Ok(s.rows)
}
