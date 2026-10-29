//! Which heap names the develop LOB directory a class's LOB files go to.
//!
//! Input: a class and every source class. Output: the [`Hfid`] for `develop_lob_dir`: the class's own heap, or for a
//! partition the heap of its partitioned class.
//! Source: develop `heap_attrinfo_transform_variable_to_disk` (heap_file.c:12636, `heap_get_class_hfid` of the
//! class the row is written through); checked on develop 682b080b2: rows inserted into a HASH-partitioned table keep
//! their LOB files under the partitioned class's directory, the partitions' own directories stay empty
//! (workspace#507).

use fastmig_ondisk::Hfid;

use crate::source_classes::SourceClass;

pub fn lob_dir_heap(c: &SourceClass, classes: &[SourceClass]) -> Hfid {
    let is_partition = c.class.partitions.iter().any(|p| p.pname.is_some());
    c.class
        .super_classes
        .first()
        .filter(|_| is_partition)
        .and_then(|root| classes.iter().find(|k| k.oid == *root))
        .map_or(c.class.heap, |root| root.class.heap)
}
