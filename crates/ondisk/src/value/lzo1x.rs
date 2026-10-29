//! LZO1X block decompression: the payload of a 10.2 compressed string.
//!
//! Input: one LZO1X-1 block (the bytes after a string's `[0xFF][compressed_len][len]` header) and the length the
//! header promises. Output: the decompressed bytes, or where the block is damaged. Every literal run, match distance
//! and match length is checked against the input and the promised output before it is copied, so a damaged block is
//! an error, never a panic or an over-read.
//! Source: engine release/10.2 @416bb667c `object_primitive.c:pr_get_compressed_data_from_buffer` (`lzo1x_decompress`,
//! result must be `LZO_E_OK` with exactly `len` bytes) and the LZO 2.x `lzo1x_d.ch` decoder it links; 11.0 replaced
//! LZO with LZ4 (CBRD-23703 ecaa13201); R8 § 2.8, D5 (workspace#513). Written here, no crate (ADR 0001, workspace#520).

/// `M2_MAX_OFFSET`: the farthest distance an M2 match reaches; a 3-byte match right after a literal run starts past it.
const M2_MAX_OFFSET: usize = 0x0800;
/// The distance an M4 match adds to its encoded value.
const M4_OFFSET: usize = 0x4000;

/// Where and why a block failed to decompress; `at` is the offset into the block.
#[derive(Debug)]
pub struct Lzo1xError {
    pub at: usize,
    pub what: &'static str,
}

/// Decompresses one LZO1X block that must expand to exactly `len` bytes and end with the end-of-stream marker on
/// its last byte.
pub fn decompress(block: &[u8], len: usize) -> Result<Vec<u8>, Lzo1xError> {
    let mut d = Decoder {
        input: block,
        ip: 0,
        out: Vec::with_capacity(len),
        len,
    };
    d.run()?;
    if d.ip != block.len() {
        return Err(d.error("input left after the end-of-stream marker"));
    }
    if d.out.len() != len {
        return Err(d.error("output shorter than the header's length"));
    }
    Ok(d.out)
}

/// Where the decoder continues; the labels of `lzo1x_d.ch`.
enum Next {
    /// Read an instruction byte that starts with a literal run (`t < 16`) or a match.
    Instruction,
    /// After a literal run of 4 or more: a byte below 16 is a 3-byte match beyond `M2_MAX_OFFSET`.
    FirstLiteralRun,
    /// A match whose instruction byte is `t`.
    Match(usize),
    /// `t` (1..3) literals, then the next match.
    TrailingLiterals(usize),
}

struct Decoder<'a> {
    input: &'a [u8],
    ip: usize,
    out: Vec<u8>,
    len: usize,
}

impl Decoder<'_> {
    fn error(&self, what: &'static str) -> Lzo1xError {
        Lzo1xError { at: self.ip, what }
    }

    fn byte(&mut self) -> Result<usize, Lzo1xError> {
        let b = *self
            .input
            .get(self.ip)
            .ok_or_else(|| self.error("block ends inside an instruction"))?;
        self.ip += 1;
        Ok(b as usize)
    }

    /// A run length that overflows its instruction bits: each 0 byte adds 255, then `base` plus the first non-zero byte.
    fn extended_length(&mut self, base: usize) -> Result<usize, Lzo1xError> {
        let mut n = base;
        loop {
            match self.byte()? {
                0 => n += 255,
                b => return Ok(n + b),
            }
        }
    }

    /// The 14-bit little-endian distance field of M3/M4 matches (low two bits are the trailing literal count).
    fn distance14(&mut self) -> Result<usize, Lzo1xError> {
        let lo = self.byte()?;
        let hi = self.byte()?;
        Ok((lo >> 2) + (hi << 6))
    }

    fn literals(&mut self, n: usize) -> Result<(), Lzo1xError> {
        let src = self
            .input
            .get(self.ip..self.ip + n)
            .ok_or_else(|| self.error("literal run past the end of the block"))?;
        if self.out.len() + n > self.len {
            return Err(self.error("literal run past the header's length"));
        }
        self.out.extend_from_slice(src);
        self.ip += n;
        Ok(())
    }

    /// Copies `n` bytes starting `distance` bytes back; an overlapping copy repeats the bytes it has just written.
    fn copy_match(&mut self, distance: usize, n: usize) -> Result<(), Lzo1xError> {
        if distance == 0 || distance > self.out.len() {
            return Err(self.error("match distance before the start of the output"));
        }
        if self.out.len() + n > self.len {
            return Err(self.error("match past the header's length"));
        }
        let start = self.out.len() - distance;
        if distance >= n {
            self.out.extend_from_within(start..start + n);
        } else {
            for i in start..start + n {
                let b = self.out[i];
                self.out.push(b);
            }
        }
        Ok(())
    }

    /// The trailing literal count stored in the low bits of the byte two back (the match's last distance byte, or its
    /// instruction byte for M1/M2).
    fn after_match(&self) -> Next {
        match self.input[self.ip - 2] & 3 {
            0 => Next::Instruction,
            t => Next::TrailingLiterals(t as usize),
        }
    }

    fn run(&mut self) -> Result<(), Lzo1xError> {
        let mut next = match self.input.first() {
            Some(&b) if b > 17 => {
                self.ip = 1;
                let t = b as usize - 17;
                if t < 4 {
                    Next::TrailingLiterals(t)
                } else {
                    self.literals(t)?;
                    Next::FirstLiteralRun
                }
            }
            _ => Next::Instruction,
        };
        loop {
            next = match next {
                Next::Instruction => {
                    let t = self.byte()?;
                    if t >= 16 {
                        Next::Match(t)
                    } else {
                        let t = if t == 0 { self.extended_length(15)? } else { t };
                        self.literals(t + 3)?;
                        Next::FirstLiteralRun
                    }
                }
                Next::FirstLiteralRun => {
                    let t = self.byte()?;
                    if t >= 16 {
                        Next::Match(t)
                    } else {
                        let distance = 1 + M2_MAX_OFFSET + (t >> 2) + (self.byte()? << 2);
                        self.copy_match(distance, 3)?;
                        self.after_match()
                    }
                }
                Next::Match(t) => {
                    if t >= 64 {
                        // M2: length 3..8, distance 1..2048
                        let distance = 1 + ((t >> 2) & 7) + (self.byte()? << 3);
                        self.copy_match(distance, (t >> 5) + 1)?;
                    } else if t >= 32 {
                        // M3: distance 1..16384
                        let t = match t & 31 {
                            0 => self.extended_length(31)?,
                            t => t,
                        };
                        let distance = 1 + self.distance14()?;
                        self.copy_match(distance, t + 2)?;
                    } else if t >= 16 {
                        // M4: distance 16385..49151; distance 0 is the end-of-stream marker
                        let high = (t & 8) << 11;
                        let t = match t & 7 {
                            0 => self.extended_length(7)?,
                            t => t,
                        };
                        let distance = high + self.distance14()?;
                        if distance == 0 {
                            return Ok(());
                        }
                        self.copy_match(distance + M4_OFFSET, t + 2)?;
                    } else {
                        // M1: 2 bytes, distance 1..1024
                        let distance = 1 + (t >> 2) + (self.byte()? << 2);
                        self.copy_match(distance, 2)?;
                    }
                    self.after_match()
                }
                Next::TrailingLiterals(t) => {
                    self.literals(t)?;
                    Next::Match(self.byte()?)
                }
            };
        }
    }
}
