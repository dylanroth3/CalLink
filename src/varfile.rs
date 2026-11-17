//! TI-83 Plus family variable files (`**TI83F*` – .8xp, .8xv, .8xl, .8ca, …) and
//! decoding of variable payloads.
//!
//! Layout:
//! ```text
//!   "**TI83F*" 1A 0A 00 | comment[42] | u16 data_len | entries… | u16 checksum
//!   entry: u16 hdr_len(0x0B|0x0D) u16 data_len u8 type name[8] [u8 version u8 flags] u16 data_len data
//! ```
//! All integers little-endian; checksum = sum of the entry bytes mod 2^16.

use anyhow::{bail, ensure, Context, Result};

use crate::tokens;

pub const SIGNATURE: &[u8; 11] = b"**TI83F*\x1A\x0A\x00";
pub const FLASH_SIGNATURE: &[u8; 8] = b"**TIFL**";
pub const ARCHIVED_FLAG: u8 = 0x80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VarType(pub u8);

impl VarType {
    pub const REAL: VarType = VarType(0x00);
    pub const LIST: VarType = VarType(0x01);
    pub const MATRIX: VarType = VarType(0x02);
    pub const EQUATION: VarType = VarType(0x03);
    pub const STRING: VarType = VarType(0x04);
    pub const PROGRAM: VarType = VarType(0x05);
    pub const PROT_PROGRAM: VarType = VarType(0x06);
    pub const PICTURE: VarType = VarType(0x07);
    pub const GDB: VarType = VarType(0x08);
    pub const COMPLEX: VarType = VarType(0x0C);
    pub const COMPLEX_LIST: VarType = VarType(0x0D);
    pub const APPVAR: VarType = VarType(0x15);
    pub const GROUP: VarType = VarType(0x17);
    pub const IMAGE: VarType = VarType(0x1A);
    pub const OS: VarType = VarType(0x23);
    pub const APP: VarType = VarType(0x24);

    pub fn name(self) -> &'static str {
        match self.0 {
            0x00 => "Real",
            0x01 => "List",
            0x02 => "Matrix",
            0x03 => "Equation",
            0x04 => "String",
            0x05 => "Program",
            0x06 => "Protected program",
            0x07 => "Picture",
            0x08 => "GDB",
            0x0B => "Smart equation",
            0x0C => "Complex",
            0x0D => "Complex list",
            0x0E => "Undefined",
            0x0F => "Window settings",
            0x10 => "Zoom (RclWindow)",
            0x11 => "Table setup",
            0x13 => "Backup",
            0x15 => "AppVar",
            0x17 => "Group",
            0x18 => "Real fraction",
            0x1A => "Image",
            0x1B => "Complex fraction",
            0x1C => "Real radical",
            0x1D => "Complex radical",
            0x1E => "Complex pi",
            0x1F => "Complex pi fraction",
            0x20 => "Real pi",
            0x21 => "Real pi fraction",
            0x23 => "OS",
            0x24 => "Flash app",
            0x25 => "Certificate",
            0x27 => "Certificate memory",
            _ => "Unknown",
        }
    }

    /// File extension TI Connect CE uses for this type.
    pub fn extension(self) -> &'static str {
        match self.0 {
            0x00 | 0x18 | 0x1C | 0x20 | 0x21 => "8xn",
            0x01 | 0x0D => "8xl",
            0x02 => "8xm",
            0x03 | 0x0B => "8xy",
            0x04 => "8xs",
            0x05 | 0x06 => "8xp",
            0x07 => "8ci",
            0x08 => "8xd",
            0x0C | 0x1B | 0x1D | 0x1E | 0x1F => "8xc",
            0x0F => "8xw",
            0x10 => "8xz",
            0x11 => "8xt",
            0x15 => "8xv",
            0x17 => "8xg",
            0x1A => "8ca",
            0x23 => "8eu",
            0x24 => "8ek",
            _ => "8xx",
        }
    }

    /// File extension for a variable with this much data: pictures are .8xi on monochrome
    /// models (756 bytes) and .8ci on colour ones.
    pub fn extension_for(self, data_len: usize) -> &'static str {
        if self == VarType::PICTURE {
            crate::model::picture_extension(data_len)
        } else {
            self.extension()
        }
    }

    /// Variable names for these types are plain ASCII; for the rest they are tokens.
    pub fn name_is_ascii(self) -> bool {
        matches!(self.0, 0x05 | 0x06 | 0x15 | 0x17 | 0x23 | 0x24 | 0x25)
    }

    pub fn parse(s: &str) -> Option<VarType> {
        let s = s.trim().to_ascii_lowercase();
        if let Some(h) = s.strip_prefix("0x") {
            return u8::from_str_radix(h, 16).ok().map(VarType);
        }
        Some(match s.as_str() {
            "real" | "8xn" => Self::REAL,
            "list" | "8xl" => Self::LIST,
            "matrix" | "8xm" => Self::MATRIX,
            "equation" | "8xy" => Self::EQUATION,
            "string" | "8xs" => Self::STRING,
            "program" | "prgm" | "8xp" => Self::PROGRAM,
            "protected" => Self::PROT_PROGRAM,
            "picture" | "pic" | "8ci" | "8xi" => Self::PICTURE,
            "gdb" | "8xd" => Self::GDB,
            "complex" | "8xc" => Self::COMPLEX,
            "appvar" | "8xv" => Self::APPVAR,
            "group" | "8xg" => Self::GROUP,
            "image" | "8ca" => Self::IMAGE,
            "app" | "8ek" => Self::APP,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct VarEntry {
    pub var_type: VarType,
    /// Raw on-calculator name (tokens or ASCII), without trailing NULs.
    pub name: Vec<u8>,
    pub version: u8,
    pub archived: bool,
    /// Variable data exactly as stored on the calculator (e.g. programs start with u16 length).
    pub data: Vec<u8>,
}

impl VarEntry {
    /// Human-readable name (e.g. `L₁`, `Str1`, `HELLO`).
    pub fn display_name(&self) -> String {
        display_name(self.var_type, &self.name)
    }
}

pub fn display_name(t: VarType, raw: &[u8]) -> String {
    if t == VarType::IMAGE && raw.first() == Some(&0x3C) {
        return String::from_utf8_lossy(&link_name_from_file_name(t, raw)).into_owned();
    }
    if t.name_is_ascii() {
        String::from_utf8_lossy(raw).into_owned()
    } else {
        tokens::detokenize(raw, false)
    }
}

/// Over USB (DUSB) the calculator names variables with UTF-8 text such as `L₁` or `Str1`,
/// while .8x files store the tokenized name (`5D 00`, `AA 00`). These convert between them.
pub fn link_name_from_file_name(t: VarType, raw: &[u8]) -> Vec<u8> {
    if t.name_is_ascii() {
        raw.to_vec()
    } else if let (VarType::IMAGE, [0x3C, rest @ ..]) = (t, raw) {
        // Image variables are named 3C 00..09 (Image1..Image9, Image0); a trailing 00 may
        // have been trimmed off with the name padding
        let n = rest.first().copied().unwrap_or(0);
        format!("Image{}", (n + 1) % 10).into_bytes()
    } else {
        tokens::detokenize(raw, false).into_bytes()
    }
}

pub fn file_name_from_link_name(t: VarType, link: &[u8]) -> Vec<u8> {
    if t.name_is_ascii() {
        return link.to_vec();
    }
    if t == VarType::IMAGE {
        if let Some(d) = link.strip_prefix(b"Image").and_then(|r| std::str::from_utf8(r).ok()).and_then(|r| r.parse::<u8>().ok()) {
            if d <= 9 {
                return vec![0x3C, (d + 9) % 10];
            }
        }
    }
    let tokenized = std::str::from_utf8(link).ok().and_then(|s| tokens::tokenize(s).ok());
    let mut v = tokenized.unwrap_or_else(|| link.to_vec());
    v.truncate(8);
    v
}

/// Display form of a name as received over USB.
pub fn link_display_name(link: &[u8]) -> String {
    String::from_utf8_lossy(link).into_owned()
}

/// A filesystem-safe version of a display name.
pub fn safe_file_stem(display: &str) -> String {
    let mut s = String::new();
    for c in display.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' => s.push(c),
            '₀'..='₉' => s.push(char::from(b'0' + (c as u32 - '₀' as u32) as u8)),
            'θ' => s.push_str("theta"),
            _ => s.push('_'),
        }
    }
    if s.is_empty() {
        s.push_str("var");
    }
    s
}

#[derive(Clone, Debug, Default)]
pub struct VarFile {
    pub comment: String,
    pub entries: Vec<VarEntry>,
}

fn u16le(b: &[u8], at: usize) -> Result<u16> {
    let s = b.get(at..at + 2).context("file truncated")?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

impl VarFile {
    pub fn parse(bytes: &[u8]) -> Result<VarFile> {
        if bytes.starts_with(FLASH_SIGNATURE) {
            bail!("this is a flash file (**TIFL**: app or OS), not a variable file");
        }
        ensure!(bytes.len() >= 57 && bytes[..8] == SIGNATURE[..8], "not a TI-83 Plus family variable file (missing **TI83F* signature)");
        let comment = String::from_utf8_lossy(&bytes[11..53]).trim_end_matches('\0').to_string();
        let data_len = u16le(bytes, 53)? as usize;
        let data = bytes.get(55..55 + data_len).context("file truncated (data section)")?;
        if let Ok(stored) = u16le(bytes, 55 + data_len) {
            let sum = data.iter().fold(0u16, |a, &b| a.wrapping_add(b as u16));
            if sum != stored {
                eprintln!("warning: checksum mismatch (stored {stored:#06x}, computed {sum:#06x})");
            }
        }
        let mut entries = Vec::new();
        let mut p = 0;
        while p < data.len() {
            let hdr_len = u16le(data, p)? as usize;
            let len = u16le(data, p + 2)? as usize;
            let var_type = VarType(*data.get(p + 4).context("truncated entry")?);
            let name_raw = data.get(p + 5..p + 13).context("truncated entry")?;
            let name: Vec<u8> = {
                let end = name_raw.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
                name_raw[..end].to_vec()
            };
            let (version, archived) = if hdr_len >= 0x0D {
                (data[p + 13], data[p + 14] & ARCHIVED_FLAG != 0)
            } else {
                (0, false)
            };
            let start = p + 2 + hdr_len + 2;
            let body = data.get(start..start + len).context("truncated entry data")?;
            entries.push(VarEntry { var_type, name, version, archived, data: body.to_vec() });
            p = start + len;
        }
        Ok(VarFile { comment, entries })
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut data = Vec::new();
        for e in &self.entries {
            ensure!(e.name.len() <= 8, "variable name longer than 8 bytes");
            ensure!(e.data.len() <= 0xFFFF, "variable {} too large for a .8x file", e.display_name());
            data.extend_from_slice(&0x0Du16.to_le_bytes());
            data.extend_from_slice(&(e.data.len() as u16).to_le_bytes());
            data.push(e.var_type.0);
            let mut name = [0u8; 8];
            name[..e.name.len()].copy_from_slice(&e.name);
            data.extend_from_slice(&name);
            data.push(e.version);
            data.push(if e.archived { ARCHIVED_FLAG } else { 0 });
            data.extend_from_slice(&(e.data.len() as u16).to_le_bytes());
            data.extend_from_slice(&e.data);
        }
        ensure!(data.len() <= 0xFFFF, "file data section too large");
        let mut out = Vec::with_capacity(data.len() + 57);
        out.extend_from_slice(SIGNATURE);
        let mut comment = [0u8; 42];
        let c = self.comment.as_bytes();
        let n = c.len().min(42);
        comment[..n].copy_from_slice(&c[..n]);
        out.extend_from_slice(&comment);
        out.extend_from_slice(&(data.len() as u16).to_le_bytes());
        let sum = data.iter().fold(0u16, |a, &b| a.wrapping_add(b as u16));
        out.extend_from_slice(&data);
        out.extend_from_slice(&sum.to_le_bytes());
        Ok(out)
    }
}

/// Convert a program name like `HELLO` or `θABC` to calculator bytes.
pub fn program_name_bytes(name: &str) -> Result<Vec<u8>> {
    let mut v = Vec::new();
    for (i, c) in name.chars().enumerate() {
        let b = match c {
            'A'..='Z' => c as u8,
            '0'..='9' if i > 0 => c as u8,
            'θ' => 0x5B,
            _ => bail!("invalid character {c:?} in program name (use A-Z, 0-9, θ; must not start with a digit)"),
        };
        v.push(b);
    }
    ensure!(!v.is_empty() && v.len() <= 8, "program names are 1-8 characters");
    Ok(v)
}

/// Build a program variable from source text.
pub fn program_from_text(name: &str, text: &str, protected: bool) -> Result<VarEntry> {
    let toks = tokens::tokenize(text)?;
    ensure!(toks.len() <= 0xFFFF - 2, "program too large");
    let mut data = (toks.len() as u16).to_le_bytes().to_vec();
    data.extend_from_slice(&toks);
    Ok(VarEntry {
        var_type: if protected { VarType::PROT_PROGRAM } else { VarType::PROGRAM },
        name: program_name_bytes(name)?,
        version: 0,
        archived: false,
        data,
    })
}

/// Token bytes of a program / string / equation variable (skips the u16 length prefix).
pub fn token_body(data: &[u8]) -> Result<&[u8]> {
    let n = u16le(data, 0)? as usize;
    data.get(2..2 + n).context("variable data shorter than its length prefix")
}

/// Format a 9-byte TI floating point number.
pub fn format_real(b: &[u8]) -> String {
    if b.len() < 9 {
        return "?".into();
    }
    let neg = b[0] & 0x80 != 0;
    let exp = b[1] as i32 - 0x80;
    let mut digits: Vec<u8> = b[2..9].iter().flat_map(|&x| [x >> 4, x & 0xF]).collect();
    if digits.iter().any(|&d| d > 9) {
        return format!("<bad BCD {:02X?}>", &b[..9]);
    }
    while digits.len() > 1 && *digits.last().unwrap() == 0 {
        digits.pop();
    }
    if digits == [0] {
        return "0".into();
    }
    let ds: String = digits.iter().map(|d| char::from(b'0' + d)).collect();
    let mut s = String::new();
    if neg {
        s.push('-');
    }
    if (-10..=13).contains(&exp) {
        if exp < 0 {
            s.push_str("0.");
            s.push_str(&"0".repeat((-exp - 1) as usize));
            s.push_str(&ds);
        } else {
            let int_len = exp as usize + 1;
            if ds.len() <= int_len {
                s.push_str(&ds);
                s.push_str(&"0".repeat(int_len - ds.len()));
            } else {
                s.push_str(&ds[..int_len]);
                s.push('.');
                s.push_str(&ds[int_len..]);
            }
        }
    } else {
        s.push_str(&ds[..1]);
        if ds.len() > 1 {
            s.push('.');
            s.push_str(&ds[1..]);
        }
        s.push_str(&format!("ᴇ{exp}"));
    }
    s
}

fn format_complex(b: &[u8]) -> String {
    if b.len() < 18 {
        return "?".into();
    }
    let re = format_real(&b[..9]);
    let im = format_real(&b[9..18]);
    if im == "0" {
        re
    } else if re == "0" {
        format!("{im}𝑖")
    } else if let Some(m) = im.strip_prefix('-') {
        format!("{re}-{m}𝑖")
    } else {
        format!("{re}+{im}𝑖")
    }
}

/// Render the contents of a variable as text where the type is understood.
pub fn describe_contents(e: &VarEntry) -> String {
    let d = &e.data;
    match e.var_type.0 {
        0x00 | 0x18 | 0x20 | 0x21 => format_real(d),
        0x0C | 0x1B | 0x1E | 0x1F => format_complex(d),
        0x01 | 0x0D => {
            let complex = e.var_type.0 == 0x0D;
            let n = u16le(d, 0).unwrap_or(0) as usize;
            let w = if complex { 18 } else { 9 };
            let items: Vec<String> = (0..n)
                .map(|i| {
                    let s = d.get(2 + i * w..2 + (i + 1) * w).unwrap_or(&[]);
                    if complex { format_complex(s) } else { format_real(s) }
                })
                .collect();
            format!("{{{}}}", items.join(","))
        }
        0x02 => {
            let (cols, rows) = (*d.first().unwrap_or(&0) as usize, *d.get(1).unwrap_or(&0) as usize);
            let mut s = String::from("[");
            for r in 0..rows {
                let row: Vec<String> = (0..cols)
                    .map(|c| format_real(d.get(2 + (r * cols + c) * 9..2 + (r * cols + c + 1) * 9).unwrap_or(&[])))
                    .collect();
                s.push_str(&format!("[{}]", row.join(",")));
            }
            s.push(']');
            s
        }
        0x03 | 0x04 | 0x05 | 0x0B => match token_body(d) {
            Ok(t) => tokens::detokenize(t, false),
            Err(err) => format!("<{err}>"),
        },
        0x06 => "<protected program; use `callink-cli prog decode` to view>".into(),
        _ => {
            let preview: Vec<String> = d.iter().take(64).map(|b| format!("{b:02X}")).collect();
            format!("{} bytes: {}{}", d.len(), preview.join(" "), if d.len() > 64 { " …" } else { "" })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_program_file() {
        let e = program_from_text("HELLO", "Disp \"HI\"", false).unwrap();
        let f = VarFile { comment: "test".into(), entries: vec![e] };
        let bytes = f.to_bytes().unwrap();
        assert_eq!(&bytes[..11], SIGNATURE);
        let g = VarFile::parse(&bytes).unwrap();
        assert_eq!(g.entries.len(), 1);
        assert_eq!(g.entries[0].display_name(), "HELLO");
        assert_eq!(describe_contents(&g.entries[0]), "Disp \"HI\"");
        assert_eq!(g.to_bytes().unwrap(), bytes);
    }

    #[test]
    fn reals() {
        // 3.14159
        assert_eq!(format_real(&[0x00, 0x80, 0x31, 0x41, 0x59, 0x00, 0x00, 0x00, 0x00]), "3.14159");
        // -42
        assert_eq!(format_real(&[0x80, 0x81, 0x42, 0, 0, 0, 0, 0, 0]), "-42");
        // 0.005
        assert_eq!(format_real(&[0x00, 0x7D, 0x50, 0, 0, 0, 0, 0, 0]), "0.005");
        // 1E50
        assert_eq!(format_real(&[0x00, 0x80 + 50, 0x10, 0, 0, 0, 0, 0, 0]), "1ᴇ50");
        assert_eq!(format_real(&[0x00, 0x80, 0, 0, 0, 0, 0, 0, 0]), "0");
    }

    #[test]
    fn names() {
        assert_eq!(display_name(VarType::LIST, &[0x5D, 0x00]), "L₁");
        assert_eq!(display_name(VarType::LIST, &[0x5D]), "L₁");
        assert_eq!(display_name(VarType::STRING, &[0xAA, 0x01]), "Str2");
        assert_eq!(safe_file_stem("L₁"), "L1");
        assert_eq!(safe_file_stem("[A]"), "_A_");
        assert!(program_name_bytes("1ABC").is_err());
        // names as seen on a real TI-84 Plus over USB
        assert_eq!(file_name_from_link_name(VarType::LIST, "L₁".as_bytes()), vec![0x5D, 0x00]);
        assert_eq!(link_name_from_file_name(VarType::LIST, &[0x5D, 0x00]), "L₁".as_bytes());
        assert_eq!(link_name_from_file_name(VarType::LIST, &[0x5D]), "L₁".as_bytes());
        assert_eq!(file_name_from_link_name(VarType::STRING, b"Str1"), vec![0xAA, 0x00]);
        assert_eq!(file_name_from_link_name(VarType::APPVAR, b"XYINPUT"), b"XYINPUT");
        // CE images: link name "Image1" ↔ file name 3C 00; "Image0" ↔ 3C 09
        assert_eq!(file_name_from_link_name(VarType::IMAGE, b"Image1"), vec![0x3C, 0x00]);
        assert_eq!(file_name_from_link_name(VarType::IMAGE, b"Image0"), vec![0x3C, 0x09]);
        assert_eq!(link_name_from_file_name(VarType::IMAGE, &[0x3C, 0x04]), b"Image5");
        assert_eq!(display_name(VarType::IMAGE, &[0x3C, 0x09]), "Image0");
        assert_eq!(display_name(VarType::IMAGE, &[0x3C]), "Image1"); // 3C 00 after NUL trimming
        assert_eq!(program_name_bytes("θA1").unwrap(), vec![0x5B, b'A', b'1']);
    }
}
