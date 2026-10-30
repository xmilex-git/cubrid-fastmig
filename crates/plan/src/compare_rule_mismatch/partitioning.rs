//! A partitioned class as the compare rule mismatch checks read it: the partition type, the key (a column, or an
//! expression over one), the HASH partition count, and each partition with its class and bound values.
//!
//! Input: the source class objects and line. Output: one [`Partitioning`] per partitioned class.
//! Source: engine `execute_schema.c:pt_node_to_partition_info` (release/10.2 :14772-14845; develop the same
//! shape): the partitioned class's `pvalues` is {key column name, HASH partition count or the RANGE minimum,
//! expression stream}; a RANGE partition's is {minimum or NULL, maximum or NULL for MAXVALUE}; a LIST partition's
//! is its values; `pexpr` is `SELECT <key expression> FROM [<class>]` (10.2 fixture, workspace#548).
//! `DB_PARTITION_TYPE` 0 HASH, 1 RANGE, 2 LIST. HASH partitions are named `p<n>` in hash order.

use fastmig_catalog::source_classes::SourceClass;
use fastmig_ondisk::VersionLine;
use fastmig_ondisk::class_object::Partition;
use fastmig_ondisk::value::{DecodeResult, OrReader, Value, decode_tagged};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartitionType {
    Hash,
    Range,
    List,
}

pub struct Part<'c> {
    pub class: &'c SourceClass,
    /// The partition's own name (`p0`, not `t__p__p0`).
    pub name: Vec<u8>,
    /// RANGE: [minimum, maximum] with NULL for no bound; LIST: the values.
    pub bounds: Vec<Value>,
}

pub struct Partitioning<'c> {
    pub root: &'c SourceClass,
    pub ty: PartitionType,
    /// The column the key is, or the one column its expression reads.
    pub key_column: Vec<u8>,
    /// The key is the column itself, not an expression over it.
    pub key_is_column: bool,
    pub expression: String,
    /// HASH: the partition count.
    pub hash_count: i32,
    pub parts: Vec<Part<'c>>,
}

fn elements(line: VersionLine, packed: &[u8]) -> DecodeResult<Vec<Value>> {
    if packed.is_empty() {
        return Ok(Vec::new());
    }
    Ok(
        match decode_tagged(line, &mut OrReader::new(packed), Some(packed.len()))?.1 {
            Value::Set(s) => s.elements.into_iter().map(|e| e.value).collect(),
            _ => Vec::new(),
        },
    )
}

/// The key expression out of the stored `pexpr`, which the engine keeps as `SELECT <expression> FROM [<class>]`.
fn expression_of(pexpr: &str) -> &str {
    let e = pexpr.trim();
    let e = match e.get(..7) {
        Some(head) if head.eq_ignore_ascii_case("SELECT ") => &e[7..],
        _ => e,
    };
    e.rfind(" FROM ").map_or(e, |at| &e[..at]).trim()
}

/// The expression text names exactly the key column (`c`, `[c]`, `"c"` or `` `c` ``).
fn names_column(expression: &str, column: &[u8]) -> bool {
    let e = expression.trim();
    let inner = ["[]", "\"\"", "``"]
        .iter()
        .find_map(|q| {
            let (open, close) = (q.as_bytes()[0] as char, q.as_bytes()[1] as char);
            e.strip_prefix(open).and_then(|x| x.strip_suffix(close))
        })
        .unwrap_or(e);
    inner.as_bytes().eq_ignore_ascii_case(column)
}

fn master(c: &SourceClass) -> Option<&Partition> {
    c.class.partitions.iter().find(|p| p.pname.is_none())
}

/// Every partitioned class among `classes`, with its partitions.
pub fn partitionings<'c>(
    classes: &'c [SourceClass],
    line: VersionLine,
) -> DecodeResult<Vec<Partitioning<'c>>> {
    let mut out = Vec::new();
    for root in classes {
        let Some(m) = master(root) else { continue };
        let ty = match m.ptype {
            0 => PartitionType::Hash,
            1 => PartitionType::Range,
            2 => PartitionType::List,
            _ => continue,
        };
        let pv = elements(line, &m.pvalues)?;
        let key_column = match pv.first() {
            Some(Value::Varchar(s) | Value::Char(s)) => s.clone(),
            _ => continue,
        };
        let hash_count = match pv.get(1) {
            Some(Value::Integer(n)) => *n,
            _ => 0,
        };
        let pexpr = String::from_utf8_lossy(m.pexpr.as_deref().unwrap_or_default()).into_owned();
        let expression = expression_of(&pexpr).to_string();
        let mut parts = Vec::new();
        for sub in root.class.sub_classes.iter() {
            let Some(class) = classes.iter().find(|c| c.oid == *sub) else {
                continue;
            };
            let Some(p) = class.class.partitions.iter().find(|p| p.pname.is_some()) else {
                continue;
            };
            parts.push(Part {
                class,
                name: p.pname.clone().unwrap_or_default(),
                bounds: elements(line, &p.pvalues)?,
            });
        }
        out.push(Partitioning {
            root,
            ty,
            key_is_column: names_column(&expression, &key_column),
            key_column,
            expression,
            hash_count,
            parts,
        });
    }
    Ok(out)
}
