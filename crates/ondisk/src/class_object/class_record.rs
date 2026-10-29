//! The top level of a class object record: non-MVCC header, 17 variable fields, 88 fixed bytes (84 in 10.2).
//!
//! Input: the full bytes of one root-heap record other than the root class's own record, and the release line.
//! Output: [`ClassObject`].
//! Source: engine `transform_cl.c:put_class_varinfo` / `put_class_attributes` / `disk_to_class`, `tf_class_to_disk`
//! (header = repid word | `OR_OFFSET_SIZE_4BYTE`, then CHN; the root class is not MVCC, so 8 B),
//! `object_representation.h` ORC_*_INDEX / ORC_*_OFFSET; same layout in release/11.4 @46212adcd and develop
//! @5b1867174. R3 § 2.1 offsets (record start: var table @8, fixed @80, rep_dir @88, HFID @96, owner @152,
//! variable area @168) checked on T1 pages of both lines (workspace#492).

use super::attribute::{self, Attribute};
use super::method::{self, Method, MethodFile, Resolution};
use super::or_be::{Structure, opt_string, read_oid, within};
use super::partition::{self, Partition};
use super::representation::{self, Representation};
use super::sub_set;
use crate::value::{DecodeResult, OrReader, StringCompression};
use crate::{Hfid, Oid, VersionLine, Vfid};

/// `OR_NON_MVCC_HEADER_SIZE`: repid word and CHN.
const HEADER_SIZE: usize = 8;
/// `ORC_CLASS_VAR_ATT_COUNT`.
const NVARS: usize = 17;
/// `OR_MVCC_REPID_MASK`.
const REPID_MASK: u32 = 0x00FF_FFFF;

/// Engine `SM_CLASS_TYPE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClassType {
    Class,
    Vclass,
    Adt,
    Other(i32),
}

impl ClassType {
    fn from_raw(v: i32) -> Self {
        match v {
            0 => ClassType::Class,
            1 => ClassType::Vclass,
            2 => ClassType::Adt,
            v => ClassType::Other(v),
        }
    }
}

/// One class object, every field the engine stores. Packed values (properties) stay packed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassObject {
    /// Current representation id (header word, low 24 bits).
    pub repid: i32,
    pub chn: i32,
    pub att_ids: i32,
    pub method_ids: i32,
    /// OID of the class's representation directory record in the catalog file.
    pub rep_dir: Oid,
    pub heap: Hfid,
    pub fixed_count: i32,
    pub variable_count: i32,
    pub fixed_size: i32,
    pub att_count: i32,
    pub shared_count: i32,
    pub method_count: i32,
    pub class_method_count: i32,
    pub class_attribute_count: i32,
    /// `SM_CLASSFLAG_*`.
    pub flags: i32,
    pub class_type: ClassType,
    /// Row of the owning user in `db_user` (11.4) / `_db_user` (develop).
    pub owner: Oid,
    pub collation_id: i32,
    pub tde_algorithm: i32,
    /// Unique name: `owner.class` for user classes, the bare name for system classes.
    pub name: Vec<u8>,
    pub loader_commands: Option<Vec<u8>>,
    pub representations: Vec<Representation>,
    pub sub_classes: Vec<Oid>,
    pub super_classes: Vec<Oid>,
    pub attributes: Vec<Attribute>,
    pub shared_attributes: Vec<Attribute>,
    pub class_attributes: Vec<Attribute>,
    pub methods: Vec<Method>,
    pub class_methods: Vec<Method>,
    pub method_files: Vec<MethodFile>,
    pub resolutions: Vec<Resolution>,
    /// View query specifications.
    pub query_specs: Vec<Vec<u8>>,
    pub triggers: Vec<Oid>,
    /// Packed property sequence (constraints, partition info, ...); empty for none.
    pub properties: Vec<u8>,
    pub comment: Option<Vec<u8>>,
    pub partitions: Vec<Partition>,
}

/// Engine `SM_CLASSFLAG_SYSTEM`: bit of a class object's `flags` set on system classes.
pub const CLASSFLAG_SYSTEM: i32 = 0x1;

impl ClassObject {
    pub fn is_system_class(&self) -> bool {
        self.flags & CLASSFLAG_SYSTEM != 0
    }
}

impl ClassHead {
    pub fn is_system_class(&self) -> bool {
        self.flags & CLASSFLAG_SYSTEM != 0
    }
}

/// The fields of a class object that name it: enough for a pre-check that must not decode the rest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassHead {
    pub name: Vec<u8>,
    pub flags: i32,
    pub class_type: ClassType,
    pub owner: Oid,
    pub partitions: Vec<Partition>,
}

/// Decodes only the name, flags, class type, owner and partitions of a class object. Those fields precede
/// `tde_algorithm`, so one reader serves 10.2 (84 B fixed area) and later; the other variable fields (view query
/// specs and other strings) are not read.
pub fn decode_head(record: &[u8], line: VersionLine) -> DecodeResult<ClassHead> {
    let c = StringCompression::of(line);
    let s = Structure::new(&record[HEADER_SIZE..], NVARS)?;
    let mut f = s.fixed();
    f.skip(64)?; // att_ids .. class_attribute_count (offsets as in `decode`)
    let flags = f.i32()?;
    let class_type = ClassType::from_raw(f.i32()?);
    let owner = read_oid(&mut f)?;
    Ok(ClassHead {
        name: within(opt_string(s.var(0), c), "name")?.unwrap_or_default(),
        flags,
        class_type,
        owner,
        partitions: within(partition::list(s.var(16), line), "partition")?,
    })
}

/// Decodes one class object record. `line` selects the per-line differences: the partition fixed area (develop), and
/// for 10.2 an 84 B fixed area that ends at `collation_id` (no `tde_algorithm`, read as 0 = NONE) and LZO1X strings.
/// Source: release/10.2 @416bb667c `transform_cl.c:3671-3695` (`put_class_attributes` fixed part ends with
/// `or_put_int (buf, (int) class_->collation_id)`; 11.0 added `tde_algorithm`, 0cf60dd26); R8 § 2.8, § 2.9
/// (workspace#513, #520).
pub fn decode(record: &[u8], line: VersionLine) -> DecodeResult<ClassObject> {
    let c = StringCompression::of(line);
    let mut h = OrReader::new(record);
    let repid = (h.u32()? & REPID_MASK) as i32;
    let chn = h.i32()?;
    let s = Structure::new(&record[HEADER_SIZE..], NVARS)?;
    if HEADER_SIZE + s.end() != record.len() {
        return Err(h.error(format!(
            "variable table ends at {} but the record is {} bytes",
            HEADER_SIZE + s.end(),
            record.len()
        )));
    }
    let mut f = s.fixed();
    let att_ids = f.i32()?;
    let method_ids = f.i32()?;
    let rep_dir = read_oid(&mut f)?;
    let fileid = f.i32()?;
    let volid = f.i32()?;
    let hpgid = f.i32()?;
    let heap = Hfid {
        vfid: Vfid::new(volid as i16, fileid),
        pageid: hpgid,
    };
    let fixed_count = f.i32()?;
    let variable_count = f.i32()?;
    let fixed_size = f.i32()?;
    let att_count = f.i32()?;
    f.skip(4)?; // object_size, always 0 on disk
    let shared_count = f.i32()?;
    let method_count = f.i32()?;
    let class_method_count = f.i32()?;
    let class_attribute_count = f.i32()?;
    let flags = f.i32()?;
    let class_type = ClassType::from_raw(f.i32()?);
    let owner = read_oid(&mut f)?;
    let collation_id = f.i32()?;
    let tde_algorithm = match line {
        VersionLine::V10_2 => 0,
        VersionLine::V11_4 | VersionLine::Develop => f.i32()?,
    };

    let name = within(opt_string(s.var(0), c), "name")?.unwrap_or_default();
    let ctx = |field: &str| format!("class {} {field}", String::from_utf8_lossy(&name));
    let query_specs = sub_set::elements(s.var(12)).and_then(|es| {
        es.into_iter()
            .map(|e| opt_string(Structure::new(e, 1)?.var(0), c).map(Option::unwrap_or_default))
            .collect()
    });
    Ok(ClassObject {
        repid,
        chn,
        att_ids,
        method_ids,
        rep_dir,
        heap,
        fixed_count,
        variable_count,
        fixed_size,
        att_count,
        shared_count,
        method_count,
        class_method_count,
        class_attribute_count,
        flags,
        class_type,
        owner,
        collation_id,
        tde_algorithm,
        loader_commands: within(opt_string(s.var(1), c), &ctx("loader_commands"))?,
        representations: within(representation::list(s.var(2), c), &ctx("representations"))?,
        sub_classes: within(sub_set::oids(s.var(3)), &ctx("sub_classes"))?,
        super_classes: within(sub_set::oids(s.var(4)), &ctx("super_classes"))?,
        attributes: within(attribute::list(s.var(5), c), &ctx("attributes"))?,
        shared_attributes: within(attribute::list(s.var(6), c), &ctx("shared_attributes"))?,
        class_attributes: within(attribute::list(s.var(7), c), &ctx("class_attributes"))?,
        methods: within(method::methods(s.var(8), c), &ctx("methods"))?,
        class_methods: within(method::methods(s.var(9), c), &ctx("class_methods"))?,
        method_files: within(method::method_files(s.var(10), c), &ctx("method_files"))?,
        resolutions: within(method::resolutions(s.var(11), c), &ctx("resolutions"))?,
        query_specs: within(query_specs, &ctx("query_spec"))?,
        triggers: within(sub_set::oids(s.var(13)), &ctx("triggers"))?,
        properties: s.var(14).to_vec(),
        comment: within(opt_string(s.var(15), c), &ctx("comment"))?,
        partitions: within(partition::list(s.var(16), line), &ctx("partition"))?,
        name,
    })
}
