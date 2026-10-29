//! An index's key domain: splits key images into columns, decodes and encodes keys, orders them, and rebuilds
//! prefix-compressed leaf keys.
//!
//! Input: the key domain (a packed domain from the B-tree root header, or one built from the class) and key
//! images. Output: [`KeyDomain`] with column slices, values, key images, an [`Ordering`], and the
//! [`crate::btree::key_image::KeyImage`] answers.
//! MIDXKEY image: a null bitmap of `ceil(n / 8)` bytes (bit `i % 8` of byte `i / 8` set = column `i` present), an
//! offset table of `n + 1` bytes (offset of column `i` from the image start, 255 = not recorded; entry `n` = image
//! size), then the present columns' images back to back. A prefix-compressed leaf key has its first `k` columns
//! marked NULL with their bytes removed. Order: columns in turn, NULL before non-NULL; the first differing column
//! decides, reversed when that column is DESC.
//! Source: engine `src/base/object_representation.h` `or_multi_*` (:2653-2830, same in release/11.4 @46212adcd
//! and develop @5b1867174), `src/object/object_primitive.c` `pr_midxkey_add_elements`, `pr_midxkey_add_prefix`,
//! `pr_midxkey_remove_prefix`, `pr_midxkey_compare_resolved` (develop :7764-7990, :9845);
//! `src/storage/btree.c` `btree_node_get_common_prefix` (:16380-16449).

use std::cmp::Ordering;

use crate::VersionLine;
use crate::btree::key_image::KeyImage;
use crate::value::db_type::DbType;
use crate::value::db_value::Value;
use crate::value::domain::Domain;
use crate::value::index_image::{Collation, compare_images, decode_image, encode_image, image_len};
use crate::value::or_buf::{DecodeError, DecodeResult, OrReader};
use crate::value::packed_domain::decode_packed_domain;

/// Engine `OR_MULTI_MAX_OFFSET`: an offset-table entry that is not recorded.
const MAX_OFFSET: usize = 255;

/// One column of a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyColumn {
    pub domain: Domain,
    /// The order of a string column; `None` for other types.
    pub collation: Option<Collation>,
}

/// The key domain of one index in one version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyDomain {
    pub version: VersionLine,
    /// True for a MIDXKEY (multi-column) key, false for a single-column key image.
    pub midxkey: bool,
    pub columns: Vec<KeyColumn>,
}

fn err(what: String) -> DecodeError {
    DecodeError { at: 0, what }
}

impl KeyDomain {
    /// Builds the key domain from its domain (MIDXKEY or the single column's). Fails for a column type that cannot
    /// be a key or a string collation [`Collation`] does not implement.
    pub fn new(version: VersionLine, domain: &Domain) -> DecodeResult<KeyDomain> {
        let midxkey = domain.ty == DbType::Midxkey;
        let cols: Vec<Domain> = if midxkey {
            domain.set_domain.clone()
        } else {
            vec![domain.clone()]
        };
        if cols.is_empty() {
            return Err(err("key domain without columns".into()));
        }
        let columns = cols
            .into_iter()
            .map(|d| {
                let collation = if d.ty.is_char_string() {
                    Some(Collation::from_id(d.collation_id).ok_or_else(|| {
                        err(format!(
                            "collation {} of a {:?} key column is not supported",
                            d.collation_id, d.ty
                        ))
                    })?)
                } else {
                    None
                };
                Ok(KeyColumn {
                    domain: d,
                    collation,
                })
            })
            .collect::<DecodeResult<_>>()?;
        Ok(KeyDomain {
            version,
            midxkey,
            columns,
        })
    }

    /// Parses a packed key domain (B-tree root header) and builds the key domain.
    pub fn from_packed(version: VersionLine, packed: &[u8]) -> DecodeResult<KeyDomain> {
        let chain = decode_packed_domain(version, &mut OrReader::new(packed))?;
        let top = chain
            .first()
            .ok_or_else(|| err("empty key domain".into()))?;
        KeyDomain::new(version, top)
    }

    pub fn n_columns(&self) -> usize {
        self.columns.len()
    }

    fn header_size(&self) -> usize {
        let n = self.columns.len();
        n.div_ceil(8) + n + 1
    }

    /// The column images of `img` (`None` = NULL column) and the image's total length.
    pub fn split<'a>(&self, img: &'a [u8]) -> DecodeResult<(Vec<Option<&'a [u8]>>, usize)> {
        if !self.midxkey {
            let c = &self.columns[0].domain;
            let n = image_len(self.version, c, img)?;
            let s = img
                .get(..n)
                .ok_or_else(|| err(format!("key image needs {n} bytes")))?;
            return Ok((vec![Some(s)], n));
        }
        let mut pos = self.header_size();
        if img.len() < pos {
            return Err(err("MIDXKEY image shorter than its header".into()));
        }
        let mut out = Vec::with_capacity(self.columns.len());
        for (i, c) in self.columns.iter().enumerate() {
            if img[i / 8] & (1 << (i % 8)) == 0 {
                out.push(None);
                continue;
            }
            let rest = &img[pos..];
            let n = image_len(self.version, &c.domain, rest)?;
            out.push(Some(
                rest.get(..n)
                    .ok_or_else(|| err(format!("column {i} needs {n} bytes")))?,
            ));
            pos += n;
        }
        Ok((out, pos))
    }

    /// Length of the image at the start of `img`, every column checked ([`KeyImage::image_len`]).
    pub fn try_image_len(&self, img: &[u8]) -> DecodeResult<usize> {
        Ok(self.split(img)?.1)
    }

    /// The key's column values (NULL as [`Value::Null`]).
    pub fn decode_key(&self, img: &[u8]) -> DecodeResult<Vec<Value>> {
        let (cols, _) = self.split(img)?;
        cols.iter()
            .zip(&self.columns)
            .map(|(s, c)| match s {
                None => Ok(Value::Null),
                Some(s) => decode_image(self.version, &c.domain, s),
            })
            .collect()
    }

    /// Builds the MIDXKEY image from column images (engine `pr_midxkey_add_elements`).
    fn assemble(&self, cols: &[Option<&[u8]>]) -> Vec<u8> {
        let n = self.columns.len();
        let header = self.header_size();
        let mut out = vec![0u8; header];
        let table = n.div_ceil(8);
        for (i, c) in cols.iter().enumerate() {
            let pos = out.len();
            out[table + i] = pos.min(MAX_OFFSET) as u8;
            if let Some(b) = c {
                out[i / 8] |= 1 << (i % 8);
                out.extend_from_slice(b);
            }
        }
        out[table + n] = out.len().min(MAX_OFFSET) as u8;
        out
    }

    /// The key image of `values` (one per column). A single-column key cannot be NULL.
    pub fn encode_key(&self, values: &[Value]) -> Vec<u8> {
        assert_eq!(values.len(), self.columns.len(), "one value per key column");
        if !self.midxkey {
            assert!(
                !values[0].is_null(),
                "a single-column key image cannot be NULL"
            );
            let mut out = Vec::new();
            encode_image(self.version, &self.columns[0].domain, &values[0], &mut out);
            return out;
        }
        let imgs: Vec<Option<Vec<u8>>> = values
            .iter()
            .zip(&self.columns)
            .map(|(v, c)| {
                (!v.is_null()).then(|| {
                    let mut b = Vec::new();
                    encode_image(self.version, &c.domain, v, &mut b);
                    b
                })
            })
            .collect();
        let refs: Vec<Option<&[u8]>> = imgs.iter().map(|o| o.as_deref()).collect();
        self.assemble(&refs)
    }

    fn compare_column(&self, i: usize, a: Option<&[u8]>, b: Option<&[u8]>) -> Ordering {
        let c = &self.columns[i];
        let o = match (a, b) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(x), Some(y)) => compare_images(self.version, &c.domain, c.collation, x, y),
        };
        if c.domain.is_desc { o.reverse() } else { o }
    }

    /// Key order (engine `pr_midxkey_compare` / `index_cmpdisk`).
    /// Invariant: both images split. Keys read from a volume reach here only after `KeyImage::image_len` or
    /// `try_image_len` accepted them (leaf record reader, verify `key_of`); the index builder compares images it
    /// encoded itself. A sort comparator cannot return an error, so a broken invariant panics.
    pub fn compare(&self, a: &[u8], b: &[u8]) -> Ordering {
        let (ca, _) = self.split(a).expect("key image checked before comparing");
        let (cb, _) = self.split(b).expect("key image checked before comparing");
        for i in 0..self.columns.len() {
            let o = self.compare_column(i, ca[i], cb[i]);
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    }

    /// Leading columns on which `a` and `b` are equal.
    pub fn common_columns(&self, a: &[u8], b: &[u8]) -> usize {
        let (ca, _) = self.split(a).expect("key image checked before comparing");
        let (cb, _) = self.split(b).expect("key image checked before comparing");
        (0..self.columns.len())
            .take_while(|&i| self.compare_column(i, ca[i], cb[i]) == Ordering::Equal)
            .count()
    }

    /// Full key of a leaf record on a page whose header has `common_prefix` = `ncols` > 0: the first `ncols`
    /// MIDXKEY columns come from `fence` (the page's lower fence key, a full image) and the rest from `stored`
    /// (prefix compression `btree_node_get_common_prefix`, `btree.c:16380-16449`, develop 5b1867174).
    pub fn try_restore_prefix(
        &self,
        fence: &[u8],
        stored: &[u8],
        ncols: usize,
    ) -> DecodeResult<Vec<u8>> {
        // the engine applies `common_prefix` only to MIDXKEY keys and only when 0 < it < columns
        if !self.midxkey || ncols == 0 || ncols >= self.columns.len() {
            return Ok(stored[..self.try_image_len(stored)?].to_vec());
        }
        let (f, _) = self.split(fence)?;
        let (s, _) = self.split(stored)?;
        let cols: Vec<Option<&[u8]>> = (0..self.columns.len())
            .map(|i| if i < ncols { f[i] } else { s[i] })
            .collect();
        Ok(self.assemble(&cols))
    }
}

impl KeyImage for KeyDomain {
    fn image_len(&self, bytes: &[u8]) -> DecodeResult<usize> {
        self.try_image_len(bytes)
    }
}
