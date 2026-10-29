//! B-tree node header: slot 0 of every leaf and non-leaf page.
//!
//! Input/output: the 32 B header record.
//! Source: engine `BTREE_NODE_HEADER` (btree_load.h), `btree_init_node_header` (btree_load.c); R6 § 3.1.

use crate::ids::Vpid;
use crate::le_field::{get_f32, get_i16, get_i32, put_f32, put_i16, put_i32};
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
            HeaderLayout::Before11_4 => 28,
            HeaderLayout::Since11_4 => Self::SIZE,
        }
    }

    /// Decodes a header record from a volume written in `layout`; an error when the record is too short.
    pub fn read_layout(b: &[u8], layout: HeaderLayout) -> DecodeResult<Self> {
        if b.len() < Self::size(layout) {
            return Err(DecodeError {
                at: b.len(),
                what: format!("B-tree node header of {} B", b.len()),
            });
        }
        Ok(match layout {
            HeaderLayout::Since11_4 => Self::read(b),
            HeaderLayout::Before11_4 => {
                let mut wide = [0u8; Self::SIZE];
                wide[..28].copy_from_slice(&b[..28]);
                Self::read(&wide)
            }
        })
    }

    pub fn read(b: &[u8]) -> Self {
        Self {
            split_pivot: get_f32(b, 0),
            split_index: get_i32(b, 4),
            prev: Vpid::read(b, 8),
            next: Vpid::read(b, 16),
            node_level: get_i16(b, 24),
            max_key_len: get_i16(b, 26),
            common_prefix: get_i32(b, 28),
        }
    }

    pub fn write(&self, b: &mut [u8]) {
        put_f32(b, 0, self.split_pivot);
        put_i32(b, 4, self.split_index);
        self.prev.write(b, 8);
        self.next.write(b, 16);
        put_i16(b, 24, self.node_level);
        put_i16(b, 26, self.max_key_len);
        put_i32(b, 28, self.common_prefix);
    }

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut b = [0u8; Self::SIZE];
        self.write(&mut b);
        b
    }
}
