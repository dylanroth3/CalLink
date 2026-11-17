//! Flash files (`**TIFL**`): calculator OS images (.8xu for the TI-84 Plus, .8eu for the CE).
//!
//! A file is one or more sections, each a 78-byte header followed by `data_length` bytes:
//! ```text
//!  0  "**TIFL**"          8  version major (BCD)   9  version minor (BCD)
//! 10  flags              11  object type          12  day (BCD) 13 month (BCD) 14-15 year (BCD)
//! 16  name length        17-24 name               48  device type (0x73 = 83+/84+ family)
//! 49  data type (0x23 = OS, 0x24 = app, 0x3E = license)   73 hardware id   74-77 data length (LE)
//! ```
//! TI-84 Plus OS data is Intel HEX text grouped into 16 KB flash pages in three sections
//! (signed header, OS pages, signature); CE OS data is raw binary.

use anyhow::{bail, ensure, Context, Result};

pub const TIFL: &[u8; 8] = b"**TIFL**";
pub const DEVICE_83P_FAMILY: u8 = 0x73;
pub const TYPE_OS: u8 = 0x23;
pub const PAGE_SIZE: usize = 16384;

#[derive(Clone, Debug)]
pub struct FlashSection {
    pub version: (u8, u8),
    pub name: String,
    pub device_type: u8,
    pub data_type: u8,
    pub hw_id: u8,
    pub data: Vec<u8>,
}

/// One block of flash data, as the reference implementation (libtifiles) groups it.
#[derive(Clone, Debug, PartialEq)]
pub struct FlashPage {
    pub addr: u16,
    pub page: u16,
    pub flag: u8,
    /// Real bytes (not padded).
    pub data: Vec<u8>,
}

impl FlashPage {
    /// `len` bytes starting at `off`, padded with 0xFF past the end of the page's data
    /// (the reference keeps every page in a 0xFF-filled 16 KB buffer).
    pub fn padded(&self, off: usize, len: usize) -> Vec<u8> {
        (off..off + len).map(|i| self.data.get(i).copied().unwrap_or(0xFF)).collect()
    }
}

#[derive(Clone, Debug)]
pub struct OsFile {
    pub section: FlashSection,
    /// Only for TI-84 Plus (Intel HEX) images.
    pub pages: Vec<FlashPage>,
}

fn bcd(b: u8) -> u8 {
    (b >> 4) * 10 + (b & 0xF)
}

pub fn parse_sections(bytes: &[u8]) -> Result<Vec<FlashSection>> {
    ensure!(bytes.starts_with(TIFL), "not a TI flash file (missing **TIFL** signature)");
    let mut out = Vec::new();
    let mut p = 0;
    while p + 78 <= bytes.len() && bytes[p..].starts_with(TIFL) {
        let h = &bytes[p..p + 78];
        let len = u32::from_le_bytes([h[74], h[75], h[76], h[77]]) as usize;
        let data = bytes.get(p + 78..p + 78 + len).context("flash file truncated")?.to_vec();
        let nlen = (h[16] as usize).min(8);
        out.push(FlashSection {
            version: (bcd(h[8]), bcd(h[9])),
            name: String::from_utf8_lossy(&h[17..17 + nlen]).trim_end_matches('\0').to_string(),
            device_type: h[48],
            data_type: h[49],
            hw_id: h[73],
            data,
        });
        p += 78 + len;
    }
    ensure!(!out.is_empty(), "flash file has no sections");
    Ok(out)
}

fn hex_byte(s: &[u8]) -> Result<u8> {
    let t = std::str::from_utf8(s).ok().and_then(|t| u8::from_str_radix(t, 16).ok());
    t.context("bad hex digit in Intel HEX record")
}

/// Decode TI's Intel HEX OS data into flash pages, grouped like libtifiles does:
/// a new page starts at each page-select record (type 02), at each end-of-section
/// record (type 01, which also toggles the flag between 0x80 and 0x00), or when a block
/// reaches 16 KB.
pub fn parse_intel_hex(text: &[u8]) -> Result<Vec<FlashPage>> {
    let mut pages = Vec::new();
    let mut flag = 0x80u8;
    let mut page = 0u16;
    let mut cur: Option<FlashPage> = None;
    fn flush(pages: &mut Vec<FlashPage>, cur: &mut Option<FlashPage>) {
        if let Some(p) = cur.take() {
            if !p.data.is_empty() {
                pages.push(p);
            }
        }
    }
    // TI's files end with a DOS end-of-file byte (0x1A) after the last record.
    let text = text.split(|&b| b == 0x1A).next().unwrap_or(text);
    for (lineno, line) in text.split(|&b| b == b'\n').enumerate() {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let line = line.trim_ascii();
        if line.is_empty() {
            continue;
        }
        ensure!(line[0] == b':' && line.len() >= 11, "line {}: not an Intel HEX record", lineno + 1);
        let n = hex_byte(&line[1..3])? as usize;
        ensure!(line.len() >= 11 + 2 * n, "line {}: record too short", lineno + 1);
        let rec: Vec<u8> = (0..n + 5).map(|i| hex_byte(&line[1 + 2 * i..3 + 2 * i])).collect::<Result<_>>()?;
        let sum = rec.iter().fold(0u8, |a, &b| a.wrapping_add(b));
        ensure!(sum == 0, "line {}: Intel HEX checksum error", lineno + 1);
        let addr = u16::from_be_bytes([rec[1], rec[2]]);
        let data = &rec[4..4 + n];
        match rec[3] {
            0x00 => {
                let c = cur.get_or_insert_with(|| FlashPage { addr, page, flag, data: Vec::new() });
                c.data.extend_from_slice(data);
                if c.data.len() >= PAGE_SIZE {
                    flush(&mut pages, &mut cur);
                }
            }
            0x01 => {
                flush(&mut pages, &mut cur);
                flag ^= 0x80;
                page = 0;
            }
            0x02 => {
                flush(&mut pages, &mut cur);
                ensure!(n == 2, "line {}: bad page record", lineno + 1);
                page = u16::from_be_bytes([data[0], data[1]]);
            }
            0x03 => break,
            t => bail!("line {}: unsupported Intel HEX record type {t:#04x}", lineno + 1),
        }
    }
    flush(&mut pages, &mut cur);
    Ok(pages)
}

impl OsFile {
    pub fn parse(bytes: &[u8]) -> Result<OsFile> {
        let sections = parse_sections(bytes)?;
        let section = sections
            .into_iter()
            .find(|s| s.data_type == TYPE_OS)
            .context("this flash file contains no operating system (it may be an app)")?;
        ensure!(section.device_type == DEVICE_83P_FAMILY, "this OS is not for the TI-83 Plus / TI-84 Plus family (device {:#04x})", section.device_type);
        let pages = if section.is_intel_hex() {
            let p = parse_intel_hex(&section.data)?;
            ensure!(p.len() >= 3, "OS image has too few flash pages ({})", p.len());
            p
        } else {
            ensure!(section.data.first() == Some(&0x80), "CE OS image does not start with an OS header");
            Vec::new()
        };
        Ok(OsFile { section, pages })
    }

    /// TI-84 Plus OS images are Intel HEX text; CE ones are raw binary.
    pub fn is_ce(&self) -> bool {
        !self.section.is_intel_hex()
    }

    /// The model (product) ID the OS is signed for, from its header.
    pub fn header_product_id(&self) -> Option<u8> {
        let h: &[u8] = if self.is_ce() { &self.section.data[..self.section.data.len().min(512)] } else { &self.pages.first()?.data };
        header_product_id(h)
    }

    pub fn version_string(&self) -> String {
        format!("{}.{:02}", self.section.version.0, self.section.version.1)
    }
}

/// TI OS headers are tagged fields: `80 0F <len:4>` opens the header, then fields `80 tn …`
/// where t is the field type and n the length (0–12, or D/E/F = 1/2/4 length bytes follow).
/// Field type 1 (`80 1n`) holds the model ID, e.g. `80 11 0A` = TI-84 Plus.
pub fn header_product_id(h: &[u8]) -> Option<u8> {
    if h.get(0..2)? != [0x80, 0x0F] {
        return None;
    }
    let mut i = 6;
    for _ in 0..64 {
        if *h.get(i)? != 0x80 {
            return None;
        }
        let b1 = *h.get(i + 1)?;
        let (len, hdr) = match b1 & 0x0F {
            0x0D => (*h.get(i + 2)? as usize, 3),
            0x0E => (u16::from_be_bytes([*h.get(i + 2)?, *h.get(i + 3)?]) as usize, 4),
            0x0F => (u32::from_be_bytes([*h.get(i + 2)?, *h.get(i + 3)?, *h.get(i + 4)?, *h.get(i + 5)?]) as usize, 6),
            n => (n as usize, 2),
        };
        if b1 & 0xF0 == 0x10 && len >= 1 {
            return h.get(i + hdr).copied();
        }
        i += hdr + len;
    }
    None
}

impl FlashSection {
    pub fn is_intel_hex(&self) -> bool {
        self.hw_id == 0 && self.data.first() == Some(&b':')
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    fn rec(ty: u8, addr: u16, data: &[u8]) -> String {
        let mut r = vec![data.len() as u8, (addr >> 8) as u8, addr as u8, ty];
        r.extend_from_slice(data);
        let sum = r.iter().fold(0u8, |a, &b| a.wrapping_add(b));
        r.push(sum.wrapping_neg());
        format!(":{}\r\n", r.iter().map(|b| format!("{b:02X}")).collect::<String>())
    }

    /// A small fake TI-84 Plus OS: header section, two data pages, signature section.
    pub fn fake_84p_os() -> Vec<u8> {
        let mut hex = String::new();
        // header section (flag 0x80): 80 0F <len>, 80 11 0A (TI-84 Plus), 80 21 02, 80 31 37 (2.55)…
        let mut hdr = vec![0x80, 0x0F, 0, 0, 0, 0, 0x80, 0x11, 0x0A, 0x80, 0x21, 0x02, 0x80, 0x31, 0x37];
        hdr.resize(128, 0x83);
        for (i, chunk) in hdr.chunks(32).enumerate() {
            hex += &rec(0, 0x4000 + i as u16 * 32, chunk);
        }
        hex += &rec(1, 0, &[]);
        // OS section (flag 0x00): pages 0x00 and 0x01, 600 bytes each
        for pg in 0..2u16 {
            hex += &rec(2, 0, &pg.to_be_bytes());
            for i in 0..(600 / 20) as u16 {
                hex += &rec(0, 0x4000 + i * 20, &[(pg as u8) * 16 + (i % 16) as u8; 20]);
            }
        }
        hex += &rec(1, 0, &[]);
        // signature section (flag 0x80 again)
        hex += &rec(0, 0x4100, &[0x5A; 64]);
        hex += &rec(1, 0, &[]);
        // the real TI84Plus_OS255.8Xu ends like this: a tag after the last record, then ^Z
        hex = hex.trim_end().to_string() + "   -- CONVERT 2.6 --\r\n\x1a";
        tifl(0x02, 0x55, 0, hex.into_bytes())
    }

    pub fn fake_ce_os() -> Vec<u8> {
        // 80 0F <len>, 80 11 13 (CE model ID), filler up to 46 header bytes
        let mut data = vec![0x80, 0x0F, 0x00, 0x00, 0x01, 0x00, 0x80, 0x11, 0x13];
        data.extend(std::iter::repeat_n(0x11, 37));
        data.extend([0xFF; 4]);
        data.extend((0..3000u32).map(|i| i as u8));
        tifl(0x05, 0x08, 0x13, data)
    }

    pub fn tifl(maj: u8, min: u8, hw: u8, data: Vec<u8>) -> Vec<u8> {
        let mut h = vec![0u8; 78];
        h[..8].copy_from_slice(TIFL);
        h[8] = maj;
        h[9] = min;
        h[16] = 8;
        h[17..25].copy_from_slice(b"basecode");
        h[48] = DEVICE_83P_FAMILY;
        h[49] = TYPE_OS;
        h[73] = hw;
        h[74..78].copy_from_slice(&(data.len() as u32).to_le_bytes());
        h.extend(data);
        h
    }

    #[test]
    fn intel_hex_pages() {
        let os = OsFile::parse(&fake_84p_os()).unwrap();
        assert!(!os.is_ce());
        assert_eq!(os.version_string(), "2.55");
        let p = &os.pages;
        assert_eq!(p.len(), 4);
        assert_eq!((p[0].flag, p[0].page, p[0].addr, p[0].data.len()), (0x80, 0, 0x4000, 128));
        assert_eq!(os.header_product_id(), Some(0x0A));
        assert_eq!((p[1].flag, p[1].page, p[1].data.len()), (0x00, 0, 600));
        assert_eq!((p[2].flag, p[2].page, p[2].data.len()), (0x00, 1, 600));
        assert_eq!((p[3].flag, p[3].addr, p[3].data.len()), (0x80, 0x4100, 64));
        assert_eq!(p[0].padded(120, 16)[..8], [0x83; 8]);
        // the real TI84Plus_OS255.8Xu header starts like this
        assert_eq!(header_product_id(&[0x80, 0x0F, 0, 0, 0, 0, 0x80, 0x11, 0x0A, 0x80, 0x21, 0x02]), Some(0x0A));
        assert_eq!(header_product_id(&[0x80, 0x0F, 0, 0, 0, 0, 0x80, 0x21, 0x02, 0x80, 0x11, 0x0F]), Some(0x0F));
        assert_eq!(header_product_id(&[0x12, 0x34]), None);
        assert_eq!(p[0].padded(120, 16)[8..], [0xFF; 8]);
    }

    #[test]
    fn ce_raw() {
        let os = OsFile::parse(&fake_ce_os()).unwrap();
        assert!(os.is_ce());
        assert_eq!(os.version_string(), "5.08");
        assert_eq!(os.header_product_id(), Some(0x13));
    }

    #[test]
    fn checksum_checked() {
        let mut f = fake_84p_os();
        let i = f.iter().position(|&b| b == b':').unwrap() + 10;
        f[i] = if f[i] == b'0' { b'1' } else { b'0' };
        assert!(OsFile::parse(&f).is_err());
    }
}
