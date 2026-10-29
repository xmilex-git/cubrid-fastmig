//! CRC-32 as the engine's `crypt_crc32` computes it (reflected polynomial 0x04C11DB7, init and xorout 0xFFFFFFFF;
//! the zlib CRC-32), used by the log page checksum.
//!
//! Input: bytes. Output: the CRC.
//! Source: engine `src/base/CRC.h:1567-1571` (develop @5b1867174); R5 § 2.7. Checked against the T1 `_lgat`
//! header page checksum (workspace#490).

const fn table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static TABLE: [u32; 256] = table();

pub fn crc32(bytes: &[u8]) -> u32 {
    !bytes.iter().fold(!0u32, |c, &b| {
        TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8)
    })
}
