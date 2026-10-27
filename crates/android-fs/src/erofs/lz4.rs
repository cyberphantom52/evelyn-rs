//! LZ4 block decoding into a fixed-size output.
//!
//! EROFS stores each compressed pcluster as one raw LZ4 block, and a read may need
//! only a prefix of it, so decoding stops as soon as `output` is full.

use std::io;

fn corrupt() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "malformed LZ4 block")
}

/// Fill `output` from the LZ4 block `input`.
pub fn decompress(input: &[u8], output: &mut [u8]) -> io::Result<()> {
    let (mut i, mut o) = (0, 0);
    let length = |i: &mut usize, mut n: usize| -> io::Result<usize> {
        if n == 15 {
            loop {
                let byte = *input.get(*i).ok_or_else(corrupt)?;
                *i += 1;
                n += byte as usize;
                if byte != 255 {
                    break;
                }
            }
        }
        Ok(n)
    };
    while o < output.len() {
        let token = *input.get(i).ok_or_else(corrupt)?;
        i += 1;
        let literals = length(&mut i, (token >> 4) as usize)?;
        let take = literals.min(output.len() - o);
        output[o..o + take].copy_from_slice(input.get(i..i + take).ok_or_else(corrupt)?);
        i += literals;
        o += take;
        if o == output.len() {
            break;
        }
        let offset = u16::from_le_bytes([
            *input.get(i).ok_or_else(corrupt)?,
            *input.get(i + 1).ok_or_else(corrupt)?,
        ]) as usize;
        i += 2;
        let matched = length(&mut i, (token & 15) as usize)? + 4;
        if offset == 0 || offset > o {
            return Err(corrupt());
        }
        // Byte-by-byte: a match may overlap the bytes it is producing.
        for _ in 0..matched.min(output.len() - o) {
            output[o] = output[o - offset];
            o += 1;
        }
    }
    Ok(())
}
