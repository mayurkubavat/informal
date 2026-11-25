//! Minimal pure-Rust raw LZ4 block compressor and decompressor for GTKWave FST
//! hierarchy (`FST_BL_HIER_LZ4`) and value-change streams (`pack_type = '4'`).

use anyhow::{bail, Result};

const MIN_MATCH: usize = 4;
const HASH_LOG: usize = 12;
const HASH_SIZE: usize = 1 << HASH_LOG;
const LAST_LITERALS: usize = 5;
const MF_LIMIT: usize = 12;

#[inline]
fn hash4(data: &[u8], pos: usize) -> usize {
    let v = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
    ((v.wrapping_mul(2654435761u32)) >> (32 - HASH_LOG)) as usize
}

#[inline]
fn push_extra_len(out: &mut Vec<u8>, mut len: usize) {
    while len >= 255 {
        out.push(255);
        len -= 255;
    }
    out.push(len as u8);
}

/// Compress `src` into a standard raw LZ4 block (compatible with `LZ4_decompress_safe`).
pub fn compress(src: &[u8]) -> Vec<u8> {
    if src.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(src.len() + (src.len() / 255) + 16);
    if src.len() < MF_LIMIT {
        emit_literals(&mut out, src);
        return out;
    }

    let mut table = [0u32; HASH_SIZE];
    let mut ip = 0usize;
    let mut anchor = 0usize;
    let limit = src.len() - MF_LIMIT;
    let match_limit = src.len() - LAST_LITERALS;

    table[hash4(src, 0)] = 0;
    ip += 1;

    while ip <= limit {
        let h = hash4(src, ip);
        let cand = table[h] as usize;
        table[h] = ip as u32;

        if ip > cand
            && ip - cand <= 65535
            && src[cand] == src[ip]
            && src[cand + 1] == src[ip + 1]
            && src[cand + 2] == src[ip + 2]
            && src[cand + 3] == src[ip + 3]
        {
            let lit_len = ip - anchor;
            let offset = (ip - cand) as u16;
            let mut mlen = MIN_MATCH;
            while ip + mlen < match_limit && src[cand + mlen] == src[ip + mlen] {
                mlen += 1;
            }
            let ml_code = mlen - MIN_MATCH;
            let token = ((lit_len.min(15) as u8) << 4) | (ml_code.min(15) as u8);
            out.push(token);
            if lit_len >= 15 {
                push_extra_len(&mut out, lit_len - 15);
            }
            out.extend_from_slice(&src[anchor..ip]);
            out.extend_from_slice(&offset.to_le_bytes());
            if ml_code >= 15 {
                push_extra_len(&mut out, ml_code - 15);
            }
            ip += mlen;
            anchor = ip;
            if ip <= limit {
                table[hash4(src, ip - 2)] = (ip - 2) as u32;
            }
        } else {
            ip += 1;
        }
    }

    emit_literals(&mut out, &src[anchor..]);
    out
}

fn emit_literals(out: &mut Vec<u8>, lits: &[u8]) {
    let lit_len = lits.len();
    let token = (lit_len.min(15) as u8) << 4;
    out.push(token);
    if lit_len >= 15 {
        push_extra_len(out, lit_len - 15);
    }
    out.extend_from_slice(lits);
}

/// Decompress a standard raw LZ4 block of expected uncompressed size `expected_len`.
pub fn decompress(src: &[u8], expected_len: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected_len);
    let mut ip = 0usize;
    while ip < src.len() {
        let token = src[ip];
        ip += 1;

        let mut lit_len = (token >> 4) as usize;
        if lit_len == 15 {
            loop {
                if ip >= src.len() {
                    bail!("truncated LZ4 literal length");
                }
                let b = src[ip] as usize;
                ip += 1;
                lit_len += b;
                if b != 255 {
                    break;
                }
            }
        }
        if ip + lit_len > src.len() {
            bail!("truncated LZ4 literals");
        }
        out.extend_from_slice(&src[ip..ip + lit_len]);
        ip += lit_len;

        if ip >= src.len() {
            break;
        }
        if ip + 2 > src.len() {
            bail!("truncated LZ4 offset");
        }
        let offset = u16::from_le_bytes([src[ip], src[ip + 1]]) as usize;
        ip += 2;
        if offset == 0 || offset > out.len() {
            bail!("invalid LZ4 match offset {offset} at out len {}", out.len());
        }

        let mut mlen = ((token & 0x0f) as usize) + MIN_MATCH;
        if (token & 0x0f) == 15 {
            loop {
                if ip >= src.len() {
                    bail!("truncated LZ4 match length");
                }
                let b = src[ip] as usize;
                ip += 1;
                mlen += b;
                if b != 255 {
                    break;
                }
            }
        }
        if out.len() + mlen > expected_len.saturating_add(16) {
            bail!("LZ4 output exceeds expected length {expected_len}");
        }
        let start = out.len() - offset;
        for i in 0..mlen {
            let b = out[start + i];
            out.push(b);
        }
    }
    if out.len() != expected_len {
        bail!(
            "LZ4 decompressed length mismatch: got {}, expected {expected_len}",
            out.len()
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_short_repetitive_and_random_buffers() {
        for payload in [
            b"".as_slice(),
            b"abc".as_slice(),
            b"informal trace of a: 3 cycles".as_slice(),
            &b"010101010101010101010101010101010101010101010101".repeat(20),
        ] {
            let enc = compress(payload);
            let dec = decompress(&enc, payload.len()).unwrap();
            assert_eq!(dec, payload);
        }
        // Pseudo-random incompressible buffer
        let mut x = 0x1234_5678_9abc_def0u64;
        let rnd: Vec<u8> = (0..1024)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let enc = compress(&rnd);
        let dec = decompress(&enc, rnd.len()).unwrap();
        assert_eq!(dec, rnd);
    }
}
