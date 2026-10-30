//! The key material of the empty TDE metadata: one master key and three data keys, the `<db>_keys` file that holds
//! the master key, and the keyinfo record that holds the data keys encrypted with it.
//!
//! Input: nothing; the keys come from `/dev/urandom`.
//! Output: the `_keys` file bytes and the 156 B keyinfo record, as `tde_initialize` writes them for a new database.
//! Source: engine develop @682b080b2 `src/storage/tde.c` `tde_initialize` (:111-226), `tde_create_keys_file`,
//! `tde_add_mk` (:1556-1630), `tde_generate_keyinfo`, `tde_encrypt_dk` / `tde_dk_nonce` (data key = AES-256-CTR
//! under the master key, IV 16 B filled with 0x00/0x01/0x02), `tde_make_mk_hash` (SHA-256); `tde.h:54-96`; R9 § 2.6
//! (workspace#510). Same format from 11.0 to develop. This is not TDE support: a migration writes no encrypted page
//! (user decision 2026-10-30, workspace#515).

use std::fs::File;
use std::io::{self, Read};

use aes::Aes256;
use ctr::cipher::{KeyIvInit, StreamCipher};
use sha2::{Digest, Sha256};
use zerocopy::little_endian::{I32, I64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// `CUBRID_MAGIC_KEYS`, NUL-padded to `CUBRID_MAGIC_MAX_LENGTH`.
pub const KEYS_MAGIC: &[u8] = b"CUBRID/Keys";
/// `CUBRID_MAGIC_MAX_LENGTH` = `TDE_MK_FILE_CONTENTS_START`.
pub const KEYS_MAGIC_SIZE: usize = 25;
/// `TDE_MASTER_KEY_LENGTH` = `TDE_DATA_KEY_LENGTH`.
pub const KEY_LEN: usize = 32;
/// The record `tde_initialize` inserts: an `int` 0 (so vacuum leaves it alone) and `TDE_KEYINFO`.
pub const KEYINFO_RECORD_SIZE: usize = 4 + size_of::<KeyInfoLe>();

/// Engine `TDE_MK_FILE_ITEM`. Size 40 B: `created_time` @0 (−1 = free), `master_key` @8.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct MkFileItemLe {
    created_time: I64,
    master_key: [u8; KEY_LEN],
}
const _: () = assert!(size_of::<MkFileItemLe>() == 40);

/// Engine `TDE_KEYINFO`. Size 152 B: `mk_index` @0, 4 B hole, `created_time` @8, `set_time` @16, `mk_hash` @24,
/// `dk_perm` @56, `dk_temp` @88, `dk_log` @120 (gdb 11.0.16 = 11.4.6, R9 § 2.6).
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct KeyInfoLe {
    mk_index: I32,
    hole: [u8; 4],
    created_time: I64,
    set_time: I64,
    mk_hash: [u8; KEY_LEN],
    dk_perm: [u8; KEY_LEN],
    dk_temp: [u8; KEY_LEN],
    dk_log: [u8; KEY_LEN],
}
const _: () = assert!(size_of::<KeyInfoLe>() == 152);

/// A 256-bit key; overwritten with zeros when dropped so no key outlives its use in this process.
struct Key([u8; KEY_LEN]);

impl Drop for Key {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

/// Zeroes `b`; `black_box` keeps the compiler from dropping the stores as dead.
pub fn wipe(b: &mut [u8]) {
    b.fill(0);
    std::hint::black_box(b);
}

/// The data key slots of `TDE_KEYINFO`, in the engine's `TDE_DATA_KEY_TYPE` order.
#[derive(Clone, Copy)]
enum DataKeyType {
    Perm = 0,
    Temp = 1,
    Log = 2,
}

/// AES-256-CTR (128-bit big-endian counter, OpenSSL `EVP_aes_256_ctr`) of `buf` in place.
fn aes256_ctr(key: &Key, iv: &[u8; 16], buf: &mut [u8]) {
    ctr::Ctr128BE::<Aes256>::new(&key.0.into(), iv.into()).apply_keystream(buf);
}

/// A data key encrypted or decrypted under the master key (`tde_encrypt_dk` / `tde_decrypt_dk`; CTR is symmetric).
fn crypt_data_key(master: &Key, t: DataKeyType, k: &[u8; KEY_LEN]) -> [u8; KEY_LEN] {
    let mut out = *k;
    aes256_ctr(master, &[t as u8; 16], &mut out);
    out
}

/// The log data key of a source database (`tde_Cipher.data_keys.log_key` once its TDE module is loaded). From 11.2 to
/// 11.4 it is also the key of every stored DBLink server password ([`crate::dblink_password`], workspace#556); nothing
/// else in the tool reads it.
pub struct LogKey(Key);

impl LogKey {
    /// The key a source without a loaded TDE module used: its `dblink_Cipher.crypt_key` stays zero-initialized
    /// (release/11.4 crypt_opfunc.c:734, `dblink_get_cipher_master_key` fails with ER_TDE_CIPHER_IS_NOT_LOADED).
    pub fn zero() -> Self {
        LogKey(Key([0; KEY_LEN]))
    }

    /// The log key from a source's `_keys` file and its keyinfo record (the one record of the
    /// `BOOT_DB_PARM.tde_keyinfo_hfid` heap), as `tde_load_mk` + `tde_decrypt_dk` load it; `None` when the record names
    /// no item of the file or the item's master key does not hash to the record's `mk_hash`.
    pub fn read(keys_file: &[u8], keyinfo_record: &[u8]) -> Option<Self> {
        let (ki, _) = KeyInfoLe::ref_from_prefix(keyinfo_record.get(4..)?).ok()?;
        let at =
            KEYS_MAGIC_SIZE + usize::try_from(ki.mk_index.get()).ok()? * size_of::<MkFileItemLe>();
        let (item, _) = MkFileItemLe::ref_from_prefix(keys_file.get(at..)?).ok()?;
        let master = Key(item.master_key);
        if item.created_time.get() == -1
            || <[u8; KEY_LEN]>::from(Sha256::digest(master.0)) != ki.mk_hash
        {
            return None;
        }
        Some(LogKey(Key(crypt_data_key(
            &master,
            DataKeyType::Log,
            &ki.dk_log,
        ))))
    }

    pub(crate) fn bytes(&self) -> &[u8; KEY_LEN] {
        &self.0.0
    }
}

/// AES-256-CTR under a raw 256-bit key, for the DBLink password re-wrap ([`crate::dblink_password`]).
pub(crate) fn aes256_ctr_raw(key: &[u8; KEY_LEN], iv: &[u8; 16], buf: &mut [u8]) {
    ctr::Ctr128BE::<Aes256>::new(key.into(), iv.into()).apply_keystream(buf);
}

/// The keys of one database: what `tde_initialize` creates for a new one.
pub struct TdeKeySet {
    master: Key,
    /// `time(NULL)` when the master key was made; the keyinfo record and the `_keys` item must agree on it.
    created_time: i64,
    perm: Key,
    temp: Key,
    log: Key,
}

impl TdeKeySet {
    /// Four new random keys (`RAND_bytes` in the engine; here the kernel CSPRNG), made at `created_time`.
    pub fn generate(created_time: i64) -> io::Result<Self> {
        let mut f = File::open("/dev/urandom")?;
        let mut key = || -> io::Result<Key> {
            let mut k = Key([0; KEY_LEN]);
            f.read_exact(&mut k.0)?;
            Ok(k)
        };
        Ok(TdeKeySet {
            master: key()?,
            created_time,
            perm: key()?,
            temp: key()?,
            log: key()?,
        })
    }

    /// The `_keys` file as `tde_create_keys_file` + `tde_add_mk` leave it: the magic, then the master key in item 0
    /// (65 B). The caller wipes the returned bytes after writing them.
    pub fn keys_file(&self) -> Vec<u8> {
        let mut out = vec![0u8; KEYS_MAGIC_SIZE];
        out[..KEYS_MAGIC.len()].copy_from_slice(KEYS_MAGIC);
        let item = MkFileItemLe {
            created_time: self.created_time.into(),
            master_key: self.master.0,
        };
        out.extend_from_slice(item.as_bytes());
        let mut item = item;
        wipe(&mut item.master_key);
        out
    }

    /// The keyinfo record for master key item 0, written at `set_time`: the `int` 0, then `TDE_KEYINFO` with the
    /// master key's SHA-256 and the three data keys encrypted under it. The hole after `mk_index` is zero (the engine
    /// leaves stack bytes there; nothing reads them).
    pub fn keyinfo_record(&self, set_time: i64) -> [u8; KEYINFO_RECORD_SIZE] {
        let ki = KeyInfoLe {
            mk_index: 0.into(),
            hole: [0; 4],
            created_time: self.created_time.into(),
            set_time: set_time.into(),
            mk_hash: Sha256::digest(self.master.0).into(),
            dk_perm: crypt_data_key(&self.master, DataKeyType::Perm, &self.perm.0),
            dk_temp: crypt_data_key(&self.master, DataKeyType::Temp, &self.temp.0),
            dk_log: crypt_data_key(&self.master, DataKeyType::Log, &self.log.0),
        };
        let mut out = [0u8; KEYINFO_RECORD_SIZE];
        out[4..].copy_from_slice(ki.as_bytes());
        out
    }
}
