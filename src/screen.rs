//! Screenshot decoding.

use std::io::Write;

use anyhow::{bail, Result};

pub struct Image {
    pub width: u32,
    pub height: u32,
    /// RGB8, row-major
    pub rgb: Vec<u8>,
}

/// Decode the data returned for parameter 0x0022.
/// * 768 bytes: 96×64 monochrome, 1 bit per pixel, MSB first, 1 = dark (TI-84 Plus).
/// * 153600 bytes: 320×240 RGB565 (TI-84 Plus CE; little-endian unless `big_endian`).
pub fn decode(data: &[u8], big_endian: bool) -> Result<Image> {
    match data.len() {
        768 => {
            let (w, h) = (96u32, 64u32);
            let mut rgb = Vec::with_capacity((w * h * 3) as usize);
            for i in 0..(w * h) as usize {
                let on = data[i / 8] & (0x80 >> (i % 8)) != 0;
                rgb.extend_from_slice(if on { &[0x10, 0x18, 0x20] } else { &[0xB4, 0xC0, 0xA8] });
            }
            Ok(Image { width: w, height: h, rgb })
        }
        153600 => {
            let mut rgb = Vec::with_capacity(320 * 240 * 3);
            for px in data.as_chunks::<2>().0 {
                let v = if big_endian { u16::from_be_bytes([px[0], px[1]]) } else { u16::from_le_bytes([px[0], px[1]]) };
                let r = ((v >> 11) & 0x1F) as u8;
                let g = ((v >> 5) & 0x3F) as u8;
                let b = (v & 0x1F) as u8;
                rgb.extend_from_slice(&[(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)]);
            }
            Ok(Image { width: 320, height: 240, rgb })
        }
        _ if data.first() == Some(&0x01) => {
            let raw = decode_cse_rle(data)?;
            decode(&raw, big_endian)
        }
        n => bail!("unsupported screenshot format ({n} bytes)"),
    }
}

/// The TI-84 Plus C Silver Edition sends its screen compressed. Format (as decoded by libticalcs):
///
/// `01 | body | palette (N little-endian RGB565 colours) | N | 4 trailing bytes`
///
/// The body starts in nibble mode and can switch twice:
/// * nibble mode: `hl` (h≠0) = palette[h] then palette[l] (a 0 nibble emits nothing);
///   `0i n` = n × palette[i]; `00 00` → byte mode.
/// * byte mode: `c` (≠0) = palette[c]; `00 c n` = n × palette[c]; `00 00 00` → literal mode.
/// * literal mode: `lo hi` = that RGB565 colour; `01 00 lo hi n` = n × that colour.
///
/// Output: 320×240 RGB565 little-endian (153600 bytes).
pub fn decode_cse_rle(data: &[u8]) -> Result<Vec<u8>> {
    const OUT: usize = 320 * 240 * 2;
    let bad = |why: &str| anyhow::anyhow!("invalid TI-84 Plus C SE screenshot ({why})");
    if data.len() < 6 || data[0] != 0x01 {
        return Err(bad("missing header"));
    }
    let stream = &data[1..data.len() - 4];
    let n = *stream.last().ok_or_else(|| bad("empty"))? as usize;
    if stream.len() <= 2 * n + 1 {
        return Err(bad("palette larger than the data"));
    }
    let palette = &stream[stream.len() - 1 - 2 * n..stream.len() - 1];
    let body = &stream[..stream.len() - 1 - 2 * n];
    let color = |i: usize| -> Result<[u8; 2]> {
        if i == 0 || i > n {
            return Err(bad("colour index out of range"));
        }
        Ok([palette[2 * (i - 1)], palette[2 * (i - 1) + 1]])
    };
    let mut out: Vec<u8> = Vec::with_capacity(OUT);
    let emit = |c: [u8; 2], count: usize, out: &mut Vec<u8>| -> Result<()> {
        if out.len() + 2 * count > OUT {
            return Err(bad("too many pixels"));
        }
        for _ in 0..count {
            out.extend_from_slice(&c);
        }
        Ok(())
    };
    let mut i = 0;
    let mut mode = 0;
    while i < body.len() {
        let b = body[i];
        match mode {
            0 => {
                let (hi, lo) = ((b >> 4) as usize, (b & 0x0F) as usize);
                if hi != 0 {
                    emit(color(hi)?, 1, &mut out)?;
                    if lo != 0 {
                        emit(color(lo)?, 1, &mut out)?;
                    }
                    i += 1;
                } else if lo != 0 && i + 1 < body.len() {
                    emit(color(lo)?, body[i + 1] as usize, &mut out)?;
                    i += 2;
                } else if i + 1 < body.len() && body[i + 1] == 0 {
                    mode = 1;
                    i += 2;
                } else {
                    return Err(bad("bad nibble-mode code"));
                }
            }
            1 => {
                if b != 0 {
                    emit(color(b as usize)?, 1, &mut out)?;
                    i += 1;
                } else if i + 2 < body.len() && body[i + 1] != 0 {
                    emit(color(body[i + 1] as usize)?, body[i + 2] as usize, &mut out)?;
                    i += 3;
                } else if i + 2 < body.len() && body[i + 1] == 0 && body[i + 2] == 0 {
                    mode = 2;
                    i += 3;
                } else {
                    return Err(bad("bad byte-mode code"));
                }
            }
            _ => {
                if i + 1 >= body.len() {
                    return Err(bad("truncated literal"));
                }
                if !(body[i] == 0x01 && body[i + 1] == 0x00) {
                    emit([body[i], body[i + 1]], 1, &mut out)?;
                    i += 2;
                } else if i + 4 < body.len() {
                    emit([body[i + 2], body[i + 3]], body[i + 4] as usize, &mut out)?;
                    i += 5;
                } else {
                    return Err(bad("truncated literal run"));
                }
            }
        }
    }
    if out.len() != OUT {
        return Err(bad(&format!("decoded {} of {OUT} bytes", out.len())));
    }
    Ok(out)
}

pub fn write_png<W: Write>(img: &Image, w: W, scale: u32) -> Result<()> {
    let scale = scale.max(1);
    let (sw, sh) = (img.width * scale, img.height * scale);
    let mut enc = png::Encoder::new(w, sw, sh);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header()?;
    let mut out = Vec::with_capacity((sw * sh * 3) as usize);
    for y in 0..sh {
        let row = &img.rgb[((y / scale) * img.width * 3) as usize..][..(img.width * 3) as usize];
        for px in row.as_chunks::<3>().0 {
            for _ in 0..scale {
                out.extend_from_slice(px);
            }
        }
    }
    wr.write_image_data(&out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compress a screen the way the C SE does, exercising all three modes, and decode it.
    #[test]
    fn cse_rle_roundtrip() {
        let red = 0xF800u16.to_le_bytes();
        let white = 0xFFFFu16.to_le_bytes();
        let teal = 0x0410u16.to_le_bytes();
        let mut expected: Vec<u8> = Vec::new();
        let mut body = Vec::new();
        // nibble mode: 4 single pixels (two bytes "12" "21"), then runs of white
        body.extend([0x12, 0x21]);
        for c in [red, white, white, red] {
            expected.extend(c);
        }
        let mut left = 320 * 120 - 4;
        while left > 0 {
            let n = left.min(255);
            body.extend([0x02, n as u8]);
            for _ in 0..n {
                expected.extend(white);
            }
            left -= n;
        }
        // switch to byte mode: one teal pixel, then runs of red
        body.extend([0x00, 0x00, 0x03]);
        expected.extend(teal);
        let mut left = 320 * 60 - 1;
        while left > 0 {
            let n = left.min(255);
            body.extend([0x00, 0x01, n as u8]);
            for _ in 0..n {
                expected.extend(red);
            }
            left -= n;
        }
        // switch to literal mode: a literal colour, then literal runs of 0x1234
        body.extend([0x00, 0x00, 0x00]);
        body.extend(0xABCDu16.to_le_bytes());
        expected.extend(0xABCDu16.to_le_bytes());
        let mut left = 320 * 60 - 1;
        while left > 0 {
            let n = left.min(255);
            body.extend([0x01, 0x00, 0x34, 0x12, n as u8]);
            for _ in 0..n {
                expected.extend(0x1234u16.to_le_bytes());
            }
            left -= n;
        }
        let mut data = vec![0x01];
        data.extend(&body);
        data.extend(red);
        data.extend(white);
        data.extend(teal);
        data.push(3);
        data.extend([0, 0, 0, 0]);
        let out = decode_cse_rle(&data).unwrap();
        assert_eq!(out.len(), 153600);
        assert_eq!(out, expected);
        // and through the general decoder
        let img = decode(&data, false).unwrap();
        assert_eq!((img.width, img.height), (320, 240));
        assert_eq!(&img.rgb[..3], &[0xFF, 0, 0]);
        // corrupt: palette index out of range
        let mut bad = data.clone();
        bad[1] = 0x19;
        assert!(decode_cse_rle(&bad).is_err());
    }

    #[test]
    fn rgb565() {
        let mut d = vec![0u8; 153600];
        d[0] = 0x00;
        d[1] = 0xF8; // LE 0xF800 = pure red
        let img = decode(&d, false).unwrap();
        assert_eq!(&img.rgb[..3], &[0xFF, 0, 0]);
        let mut png = Vec::new();
        write_png(&img, &mut png, 2).unwrap();
        assert_eq!(&png[1..4], b"PNG");
    }
}
