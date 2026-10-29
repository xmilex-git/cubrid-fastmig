//! B-tree node header: slot 0 of every leaf and non-leaf page.
//!
//! Input/output: the 32 B header record.
//! Source: engine `BTREE_NODE_HEADER` (btree_load.h), `btree_init_node_header` (btree_load.c); R6 § 3.1.

use zerocopy::little_endian::{F32, I16, I32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::ids::{Vpid, VpidLe};
use crate::source_version::HeaderLayout;
use crate::value::{DecodeError, DecodeResult};

/// Engine `BTREE_NODE_HEADER` (src/storage/btree_load.h), host order. Size: 32 B in 11.4 and develop
/// (release/11.4 46212adcd, develop 5b1867174); checked on T1 pages (header record length 32 on every leaf). 28 B in
/// 10.2..11.3, which have no `common_prefix` ([`NodeHeader::read_layout`], workspace#529).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodeHeader {
    /// @0 `split_info.pivot` f32; 0 when built.
    pub split_pivot: f32,
    /// @4 `split_info.index` i32; 0 when built.
    pub split_index: i32,
    /// @8 `prev_vpid`: previous leaf; NULL on the first leaf and on non-leaf pages.
    pub prev: Vpid,
    /// @16 `next_vpid`: next leaf; NULL on the last leaf and on non-leaf pages.
    pub next: Vpid,
    /// @24 `node_level` i16: leaf 1, non-leaf ≥ 2.
    pub node_level: i16,
    /// @26 `max_key_len` i16: longest key image on the page (6 for an overflow key).
    pub max_key_len: i16,
    /// @28 `common_prefix` i32: MIDXKEY columns a compressed leaf leaves out; 0 when built.
    pub common_prefix: i32,
}

impl NodeHeader {
    pub const SIZE: usize = 32;

    pub fn new(node_level: i16) -> Self {
        Self {
            split_pivot: 0.0,
            split_index: 0,
            prev: Vpid::NULL,
            next: Vpid::NULL,
            node_level,
            max_key_len: 0,
            common_prefix: 0,
        }
    }

    /// Size in a layout: 28 B before 11.4 (no `common_prefix`, `ptype /o` of release 11.2.9 and 11.3.6), 32 B since.
    pub fn size(layout: HeaderLayout) -> usize {
        match layout {
            HeaderLayout::Before11_4 => size_of::<NodeHeaderBefore11_4Le>(),
            HeaderLayout::Since11_4 => Self::SIZE,
        }
    }

    /// Decodes a header record from a volume written in `layout`; an error when the record is too short.
    pub fn read_layout(b: &[u8], layout: HeaderLayout) -> DecodeResult<Self> {
        let short = || DecodeError {
            at: b.len(),
            what: format!("B-tree node header of {} B", b.len()),
        };
        Ok(match layout {
            HeaderLayout::Since11_4 => {
                Self::from(NodeHeaderLe::ref_from_prefix(b).map_err(|_| short())?.0)
            }
            HeaderLayout::Before11_4 => {
                let h = NodeHeaderBefore11_4Le::ref_from_prefix(b)
                    .map_err(|_| short())?
                    .0;
                Self::from_common(h, 0)
            }
        })
    }

    pub fn read(b: &[u8]) -> Self {
        Self::from(
            NodeHeaderLe::ref_from_prefix(b)
                .expect("record holds the node header")
                .0,
        )
    }

    fn from_common(h: &NodeHeaderBefore11_4Le, common_prefix: i32) -> Self {
        Self {
            split_pivot: h.split_pivot.get(),
            split_index: h.split_index.get(),
            prev: h.prev.get(),
            next: h.next.get(),
            node_level: h.node_level.get(),
            max_key_len: h.max_key_len.get(),
            common_prefix,
        }
    }

    pub fn write(&self, b: &mut [u8]) {
        NodeHeaderLe::from(self)
            .write_to_prefix(b)
            .expect("record holds the node header");
    }

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut b = [0u8; Self::SIZE];
        self.write(&mut b);
        b
    }
}

/// The node header of 10.2..11.3 as stored: every field the 11.4 header has but `common_prefix`.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub(crate) struct NodeHeaderBefore11_4Le {
    split_pivot: F32,
    split_index: I32,
    prev: VpidLe,
    next: VpidLe,
    node_level: I16,
    max_key_len: I16,
}

/// [`NodeHeader`] as stored by 11.4 and develop: the older header plus `common_prefix`.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
pub(crate) struct NodeHeaderLe {
    common: NodeHeaderBefore11_4Le,
    common_prefix: I32,
}
const _: () = assert!(size_of::<NodeHeaderBefore11_4Le>() == 28);
const _: () = assert!(size_of::<NodeHeaderLe>() == NodeHeader::SIZE);

impl From<&NodeHeaderLe> for NodeHeader {
    fn from(h: &NodeHeaderLe) -> Self {
        NodeHeader::from_common(&h.common, h.common_prefix.get())
    }
}

impl From<&NodeHeader> for NodeHeaderLe {
    fn from(n: &NodeHeader) -> Self {
        NodeHeaderLe {
            common: NodeHeaderBefore11_4Le {
                split_pivot: n.split_pivot.into(),
                split_index: n.split_index.into(),
                prev: n.prev.into(),
                next: n.next.into(),
                node_level: n.node_level.into(),
                max_key_len: n.max_key_len.into(),
            },
            common_prefix: n.common_prefix.into(),
        }
    }
}
