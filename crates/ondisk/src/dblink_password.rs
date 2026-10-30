//! Re-wraps a stored DBLink server password (`_db_server.password`) of an 11.2..11.4 source into the form develop reads.
//!
//! 11.2..11.4 store the password under the database's TDE log data key: the stored string carries an empty private key.
//! Develop reads only a string that carries its own private key, and asserts on an empty one, so a server moved as is
//! cannot log in. Inside both forms lies the same obfuscated password (`shake_dblink_password`); the re-wrap keeps it
//! byte for byte and swaps only the AES-256-CTR layer around it, so the tool never holds the password itself.
//!
//! Input: the stored string and the candidate keys (the source's log key, then the zero key of a source whose TDE module
//! was not loaded). Output: [`Rewrap`].
//! Source: engine `src/query/crypt_opfunc.c` `crypt_dblink_bin_to_str`, `crypt_dblink_str_to_bin`,
//! `crypt_dblink_encrypt`, `init_dblink_cipher` (AES-256-CTR when `pk[13]` is even or the key empty, nonce 16 × 0x07),
//! `shake_dblink_password` (release/11.4 @46212adcd, same in 11.2 and 11.3); develop @682b080b2
//! `crypt_dblink_password_encrypt` / `_decrypt` (88be3e3a1, CBRD-26862); `execute_statement.c`
//! `get_dblink_password_encrypt` with `is_external` false for CREATE/ALTER SERVER (release/11.4 :21722-21780).
//! Not TDE support: a source with encrypted pages is still refused (workspace#510); this key opens the password only
//! (workspace#556).

use std::fs::File;
use std::io::{self, Read};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::base64;
use crate::tde_keys::{KEY_LEN, LogKey, aes256_ctr_raw, wipe};

/// `DBLINK_PASSWORD_MAX_BUFSIZE` rounded down to a multiple of 4: the length of every stored string (134 / 3 × 4 + 40).
pub const STORED_LEN: usize = 216;
/// `DBLINK_PASSWORD_MAX_LENGTH`.
const MAX_PASSWORD_LEN: usize = 128;
/// `DBLINK_PASSWORD_CIPHER_LENGTH` (password + 6 header bytes), rounded up to the multiple of 3 the shake pads to.
const MAX_SHAKEN_LEN: usize = 135;
/// The nonce of `init_dblink_cipher`.
const NONCE: [u8; 16] = [0x07; 16];
/// The filler alphabet of `crypt_dblink_bin_to_str`.
const FILLER: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz1234567890";

/// What becomes of one stored password on a develop target.
pub enum Rewrap {
    /// Empty, or already carrying its own private key: develop reads it as is.
    Kept,
    /// The develop form of the same password.
    Rewrapped(Vec<u8>),
    /// No candidate key opens it (another key, or a damaged string).
    Unreadable,
}

/// The develop form of `stored`, opened with the first of `keys` whose result passes the engine's checksum.
pub fn to_develop(stored: &[u8], keys: &[&LogKey]) -> io::Result<Rewrap> {
    if stored.is_empty() {
        return Ok(Rewrap::Kept);
    }
    let Some((pk, cipher)) = unwrap(stored) else {
        return Ok(Rewrap::Unreadable);
    };
    if !pk.is_empty() {
        return Ok(Rewrap::Kept);
    }
    for key in keys {
        let mut shaken = cipher.clone();
        aes256_ctr_raw(key.bytes(), &NONCE, &mut shaken);
        if is_shaken_password(&shaken) {
            let out = wrap(&mut shaken);
            wipe(&mut shaken);
            return out.map(Rewrap::Rewrapped);
        }
        wipe(&mut shaken);
    }
    Ok(Rewrap::Unreadable)
}

/// `HEX_2_BYTE`: two upper-case hex digits.
fn hex_byte(h: &[u8]) -> Option<u8> {
    let d = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    };
    Some(d(h[0])? << 4 | d(h[1])?)
}

/// `BYTE_2_HEX`.
fn push_hex(out: &mut Vec<u8>, b: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    out.extend_from_slice(&[HEX[(b >> 4) as usize], HEX[(b & 0x0f) as usize]]);
}

/// The two check characters over the filler `s[from..]` (`crypt_dblink_bin_to_str`: the sum of byte + position).
fn check_chars(s: &[u8], from: usize) -> [u8; 2] {
    let sum: usize = (from..s.len()).map(|i| s[i] as usize + i).sum();
    [((sum >> 8) % 26) as u8 + b'A', (sum % 26) as u8 + b'a']
}

/// `crypt_dblink_str_to_bin`: the private key (shift undone) and the encrypted bytes; `None` for a string the engine
/// would reject.
fn unwrap(s: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    if s.len() != STORED_LEN {
        return None;
    }
    let enc_len = hex_byte(&s[0..2])? as usize;
    let shifts = hex_byte(&s[2..4])?;
    let pk_len = hex_byte(&s[4..6])? as usize;
    let filler = 6 + pk_len + enc_len;
    if pk_len >= KEY_LEN || filler > STORED_LEN - 2 {
        return None;
    }
    if check_chars(&s[..STORED_LEN - 2], filler) != s[STORED_LEN - 2..] {
        return None;
    }
    let (odd, even) = (shifts >> 4, shifts & 0x0f);
    let pk = s[6..6 + pk_len]
        .iter()
        .enumerate()
        .map(|(i, &b)| b.wrapping_sub(b'a' - b'0' + if i % 2 == 0 { odd } else { even }))
        .collect();
    Some((pk, base64::decode(&s[6 + pk_len..filler])?))
}

/// The layout `shake_dblink_password` leaves: shift (int), length, filler count, filler, password, then the checksum
/// byte repeated up to a multiple of 3 — what `reverse_shake_dblink_password` checks.
fn is_shaken_password(c: &[u8]) -> bool {
    if c.len() < 7 || c.len() > MAX_SHAKEN_LEN || !c.len().is_multiple_of(3) {
        return false;
    }
    let end = 6 + c[5] as usize + c[4] as usize;
    if c[4] as usize > MAX_PASSWORD_LEN || end >= c.len() || c.len() - end > 3 {
        return false;
    }
    let sum = c[..end].iter().fold(0x7Cu8, |a, &b| a.wrapping_add(b));
    c[end..].iter().all(|&b| b == sum)
}

/// `crypt_dblink_password_encrypt` after the shake: a private key from the clock in the engine's `%08ld%06ld` form,
/// with `pk[13]` even so develop picks AES, the shaken bytes encrypted under it, then `crypt_dblink_bin_to_str`.
/// `shaken` is encrypted in place.
fn wrap(shaken: &mut [u8]) -> io::Result<Vec<u8>> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?;
    let mut usec = now.subsec_micros() as u64;
    // pk[13] is the hundreds digit of usec in this form
    if usec / 100 % 2 == 1 {
        usec -= 100;
    }
    let pk = format!("{:08}{:06}", now.as_secs(), usec).into_bytes();
    let mut key = [0u8; KEY_LEN];
    key[..pk.len()].copy_from_slice(&pk);
    aes256_ctr_raw(&key, &NONCE, shaken);
    wipe(&mut key);
    let b64 = base64::encode(shaken);
    let (odd, even) = ((usec >> 3) % 15, usec % 15);
    let mut out = Vec::with_capacity(STORED_LEN);
    push_hex(&mut out, b64.len() as u8);
    push_hex(&mut out, (odd << 4 | even) as u8);
    push_hex(&mut out, pk.len() as u8);
    out.extend(
        pk.iter()
            .enumerate()
            .map(|(i, &b)| b + b'a' - b'0' + if i % 2 == 0 { odd } else { even } as u8),
    );
    out.extend_from_slice(&b64);
    let filler = out.len();
    let mut random = vec![0u8; STORED_LEN - 2 - filler];
    File::open("/dev/urandom")?.read_exact(&mut random)?;
    out.extend(random.iter().map(|&r| FILLER[r as usize % FILLER.len()]));
    let check = check_chars(&out, filler);
    out.extend_from_slice(&check);
    Ok(out)
}
