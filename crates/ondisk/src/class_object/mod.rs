//! Class objects: the root-heap records that define one class each, decoded into owned structs.
//!
//! Input: the full bytes of one root-heap record (relocation and overflow already resolved) and its release line.
//! Output: [`ClassObject`] and its substructures, or a [`crate::value::DecodeError`] naming the field; and the
//! develop record of a [`ClassObject`] ([`encode_develop`]).
//! Source: engine `src/object/transform_cl.c:disk_to_class` and the `disk_to_*` substructure readers,
//! `src/object/transform.c` metaclass tables, `src/base/object_representation.h` ORC_* (release/11.4 @46212adcd,
//! develop @5b1867174); R3 § 2.

mod attribute;
mod class_record;
mod constraint;
mod domain;
mod encode;
mod method;
mod or_be;
mod partition;
mod representation;
mod sub_set;

pub use attribute::Attribute;
pub use class_record::{ClassHead, ClassObject, ClassType, decode, decode_10_2, decode_head};
pub use constraint::{
    ClassProperties, Constraint, ConstraintKind, ForeignKeyInfo, class_properties, format_btid,
    parse_btid,
};
pub use domain::SchemaDomain;
pub use encode::encode_develop;
pub use method::{Method, MethodArg, MethodFile, MethodSig, Resolution};
pub use partition::Partition;
pub use representation::{RepAttribute, Representation};
