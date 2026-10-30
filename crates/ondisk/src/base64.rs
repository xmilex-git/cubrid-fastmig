//! Base64 as the engine's `base64_encode` / `base64_decode` write and read it in a DBLink server password: the standard
//! alphabet with `=` padding, the encoder's line breaks removed.
//!
//! Input: bytes, or base64 text. Output: the text, or the bytes (`None` on a byte outside the alphabet or a bad length).
//! Source: engine `src/base/base64.c` `base64_map` (develop @682b080b2 = release/11.4 @46212adcd); the caller
//! `crypt_dblink_bin_to_str` drops the `\n` every 76 characters (crypt_opfunc.c, develop @682b080b2).

const MAP: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            out.push(if i <= c.len() {
                MAP[(n >> shift & 0x3f) as usize]
            } else {
                b'='
            });
        }
    }
    out
}

pub fn decode(text: &[u8]) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    for c in text.as_chunks::<4>().0 {
        let pad = c.iter().rev().take_while(|&&b| b == b'=').count();
        if pad > 2 {
            return None;
        }
        let mut n = 0u32;
        for &b in &c[..4 - pad] {
            n = n << 6 | MAP.iter().position(|&m| m == b)? as u32;
        }
        n <<= 6 * pad;
        out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8][..3 - pad]);
    }
    Some(out)
}
