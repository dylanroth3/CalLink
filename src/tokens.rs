//! TI-83 Plus / TI-84 Plus (CE) token table, tokenizer and detokenizer.
//!
//! The table is `data/tokens.tsv` (`HEXCODE<TAB>text`, see its header); codes above 0xFF
//! are two-byte tokens. Some texts use glyphs of TI's calculator font (U+E000–F8FF); those
//! with an obvious Unicode equivalent are shown as that equivalent (both spellings are
//! accepted when tokenizing).
//!
//! Plain lowercase letters are the lowercase-letter tokens (0xBBB0…), which is what programs
//! use in strings; statistics/sequence variables named by single letters are spelled `|a`,
//! `|n`, `|u`, … so that detokenize → tokenize round-trips byte-exactly.

use std::collections::HashMap;
use std::sync::OnceLock;

const TABLE: &str = include_str!("../data/tokens.tsv");

/// First bytes that introduce a two-byte token.
pub const PREFIXES: [u8; 11] = [0x5C, 0x5D, 0x5E, 0x60, 0x61, 0x62, 0x63, 0x7E, 0xAA, 0xBB, 0xEF];

pub fn is_prefix(b: u8) -> bool {
    PREFIXES.contains(&b)
}

/// Private-use glyphs in TI's font → readable Unicode.
const GLYPHS: &[(char, &str)] = &[
    ('\u{F000}', "\u{1D07}"),    // ᴇ  (exponent)
    ('\u{F001}', "\u{02B3}"),    // ʳ  (radian)
    ('\u{F002}', "\u{1D40}"),    // ᵀ  (transpose)
    ('\u{F005}', "\u{207B}\u{00B9}"), // ⁻¹
    ('\u{F00D}', "\u{00B3}"),    // ³
    ('\u{F014}', "\u{25BA}"),    // ►
    ('\u{F024}', "\u{0394}"),    // Δ
    ('\u{F02F}', "\u{1D456}"),   // 𝑖  (imaginary unit)
    ('\u{F038}', "\u{1D1B}"),    // ᴛ  (parametric T)
    ('\u{F03F}', "\u{212F}"),    // ℯ
    ('\u{F01D}', "\u{2081}\u{2080}"), // ₁₀ (as in ₁₀^( )
    ('\u{00AD}', "\u{207B}"),    // ⁻  negative sign (TI's table uses an invisible soft hyphen)
    ('\u{230A}', "\u{029F}"),    // ʟ  custom-list prefix (TI's table uses ⌊)
];

pub struct TokenTable {
    /// code → display text (readable glyphs)
    by_code: HashMap<u16, String>,
    /// code → display text using TI's private-use glyphs
    by_code_raw: HashMap<u16, String>,
    /// text → code (both readable and raw spellings)
    by_text: HashMap<String, u16>,
    max_chars: usize,
    /// Command words whose token text ends in a space ("Disp ", "Pause "…), without it.
    /// Accepted at the end of a line, before ':' or at the end of the program.
    bare: HashMap<String, u16>,
}

/// Undo the escapes of data/tokens.tsv: `\\`, `\n`, `\t`, `\u{XXXX}`.
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some('u') if it.peek() == Some(&'{') => {
                it.next();
                let hex: String = it.by_ref().take_while(|&c| c != '}').collect();
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn readable(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match GLYPHS.iter().find(|(g, _)| *g == c) {
            Some((_, r)) => out.push_str(r),
            None => out.push(c),
        }
    }
    out
}

impl TokenTable {
    /// Every lookup the tokenizer uses, in a fixed order (for checking two tables are equal).
    #[doc(hidden)]
    pub fn canonical_dump(&self) -> String {
        let mut out = vec![format!("max_chars {}", self.max_chars)];
        let mut v: Vec<String> = self.by_code.iter().map(|(c, t)| format!("code {c:04X} {t:?}")).collect();
        v.extend(self.by_code_raw.iter().map(|(c, t)| format!("raw {c:04X} {t:?}")));
        v.extend(self.by_text.iter().map(|(t, c)| format!("text {t:?} {c:04X}")));
        v.extend(self.bare.iter().map(|(t, c)| format!("bare {t:?} {c:04X}")));
        v.sort();
        out.extend(v);
        out.join("\n")
    }

    /// Entries (raw text, code) from data/tokens.tsv, in file order.
    fn entries() -> Vec<(String, u16)> {
        let mut out = Vec::new();
        for line in TABLE.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((code, text)) = line.split_once('\t') else { continue };
            let Ok(code) = u16::from_str_radix(code, 16) else { continue };
            out.push((unescape(text), code));
        }
        out
    }

    fn build() -> TokenTable {
        Self::from_entries(Self::entries())
    }

    fn from_entries(entries: Vec<(String, u16)>) -> TokenTable {
        let mut by_code = HashMap::new();
        let mut by_code_raw = HashMap::new();
        let mut by_text = HashMap::new();
        let mut max_chars = 1;
        for (text, code) in &entries {
            let r = readable(text);
            by_code.insert(*code, r.clone());
            by_code_raw.insert(*code, text.clone());
            for t in [text.clone(), r] {
                max_chars = max_chars.max(t.chars().count());
                by_text.entry(t).or_insert(*code);
            }
        }
        // Prefer the later (CE) spelling's code when both texts are identical is moot; but make
        // sure every display string maps back to the code it was produced from.
        for (code, text) in by_code.iter().chain(by_code_raw.iter()) {
            by_text.insert(text.clone(), *code);
        }
        let mut bare = HashMap::new();
        for (text, &code) in &by_text {
            let t = text.trim_end_matches(' ');
            if t.len() < text.len() && !t.trim().is_empty() && !by_text.contains_key(t) {
                bare.entry(t.to_string()).or_insert(code);
            }
        }
        TokenTable { by_code, by_code_raw, by_text, max_chars, bare }
    }

    pub fn get() -> &'static TokenTable {
        static T: OnceLock<TokenTable> = OnceLock::new();
        T.get_or_init(TokenTable::build)
    }

    pub fn text(&self, code: u16, raw_glyphs: bool) -> Option<&str> {
        let m = if raw_glyphs { &self.by_code_raw } else { &self.by_code };
        m.get(&code).map(|s| s.as_str())
    }

    pub fn code(&self, text: &str) -> Option<u16> {
        self.by_text.get(text).copied()
    }

    /// All (code, text) pairs, sorted by code.
    pub fn all(&self) -> Vec<(u16, &str)> {
        let mut v: Vec<_> = self.by_code.iter().map(|(c, t)| (*c, t.as_str())).collect();
        v.sort();
        v
    }
}

/// Turn token bytes into text. Unknown tokens are written as `\x{HH}` / `\x{HHHH}`.
pub fn detokenize(bytes: &[u8], raw_glyphs: bool) -> String {
    let t = TokenTable::get();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let (code, len) = if is_prefix(b) {
            let second = bytes.get(i + 1).copied().unwrap_or(0);
            (((b as u16) << 8) | second as u16, 2)
        } else {
            (b as u16, 1)
        };
        match t.text(code, raw_glyphs) {
            Some(s) => out.push_str(s),
            None if len == 2 => out.push_str(&format!("\\x{{{code:04X}}}")),
            None => out.push_str(&format!("\\x{{{code:02X}}}")),
        }
        i += len;
    }
    out
}

#[derive(Debug)]
pub struct TokenizeError {
    pub line: usize,
    pub column: usize,
    pub text: String,
}

impl std::fmt::Display for TokenizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}, column {}: no token matches {:?}", self.line, self.column, self.text)
    }
}
impl std::error::Error for TokenizeError {}

/// Turn text into token bytes using greedy longest match (what TI Connect's editor does).
/// Accepts `\r\n` or `\n` line endings; a single trailing newline is ignored.
pub fn tokenize(text: &str) -> Result<Vec<u8>, TokenizeError> {
    let t = TokenTable::get();
    let text = text.replace("\r\n", "\n");
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let (mut line, mut line_start) = (1, 0);
    'outer: while i < chars.len() {
        // \x{HH} or \x{HHHH} escape for tokens that have no text form
        if chars[i] == '\\' && chars.get(i + 1) == Some(&'x') && chars.get(i + 2) == Some(&'{') {
            if let Some(end) = chars[i + 3..].iter().position(|&c| c == '}') {
                let hex: String = chars[i + 3..i + 3 + end].iter().collect();
                if let (Ok(v), 2 | 4) = (u16::from_str_radix(&hex, 16), hex.len()) {
                    if hex.len() == 4 {
                        out.push((v >> 8) as u8);
                    }
                    out.push(v as u8);
                    i += 4 + end;
                    continue;
                }
            }
        }
        let maxn = t.max_chars.min(chars.len() - i);
        for n in (1..=maxn).rev() {
            let s: String = chars[i..i + n].iter().collect();
            let at_boundary = matches!(chars.get(i + n), None | Some('\n') | Some(':'));
            let found = t.code(&s).or_else(|| if at_boundary { t.bare.get(&s).copied() } else { None });
            if let Some(code) = found {
                if code > 0xFF {
                    out.push((code >> 8) as u8);
                }
                out.push(code as u8);
                if chars[i] == '\n' {
                    line += 1;
                    line_start = i + 1;
                }
                i += n;
                continue 'outer;
            }
        }
        return Err(TokenizeError {
            line,
            column: i - line_start + 1,
            text: chars[i..(i + 10).min(chars.len())].iter().collect(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_program() {
        let src = "ClrHome\nDisp \"Hello, world\"\nFor(I,1,10)\nOutput(I,1,I²)\nEnd\n3→A:A^-1\n";
        let toks = tokenize(src).unwrap();
        assert_eq!(&toks[..2], &[0xE1, 0x3F]); // ClrHome, newline
        let back = detokenize(&toks, false);
        assert_eq!(tokenize(&back).unwrap(), toks);
    }

    #[test]
    fn lowercase_and_stat_vars() {
        assert_eq!(tokenize("a").unwrap(), vec![0xBB, 0xB0]);
        assert_eq!(tokenize("z").unwrap(), vec![0xBB, 0xCA]);
        assert_eq!(tokenize("l").unwrap(), vec![0xBB, 0xBC]);
        assert_eq!(tokenize("|a").unwrap(), vec![0x62, 0x16]);
        assert_eq!(tokenize("|u").unwrap(), vec![0x5E, 0x80]);
        assert_eq!(detokenize(&[0x62, 0x16], false), "|a");
    }

    #[test]
    fn two_byte_and_glyphs() {
        assert_eq!(tokenize("L₁").unwrap(), vec![0x5D, 0x00]);
        assert_eq!(tokenize("Str1").unwrap(), vec![0xAA, 0x00]);
        assert_eq!(detokenize(&[0x3B], false), "ᴇ");
        assert_eq!(detokenize(&[0x3B], true), "\u{F000}");
        assert_eq!(tokenize("\u{F000}").unwrap(), vec![0x3B]);
        assert_eq!(tokenize("sin⁻¹(").unwrap(), vec![0xC3]);
        // name with a dangling prefix byte, as names come over the wire
        assert_eq!(detokenize(&[0x5D], false), "L₁");
    }

    #[test]
    fn every_token_roundtrips() {
        let t = TokenTable::get();
        for (code, text) in t.all() {
            if code == 0x3F {
                continue; // newline: a lone trailing newline is deliberately dropped
            }
            let mut bytes = vec![];
            if code > 0xFF {
                bytes.push((code >> 8) as u8);
            }
            bytes.push(code as u8);
            assert_eq!(detokenize(&bytes, false), text);
            // a lone token must re-tokenize to itself
            assert_eq!(tokenize(text).unwrap(), bytes, "token {code:#06x} {text:?}");
        }
    }

    #[test]
    fn command_words_without_trailing_space() {
        // "Pause"/"Disp" at the end of a line or before ':' are the commands
        assert_eq!(tokenize("Pause\nDisp:Return").unwrap(), vec![0xD8, 0x3F, 0xDE, 0x3E, 0xD5]);
        // elsewhere the letters stay letters
        assert_eq!(tokenize("Dispo").unwrap()[0], b'D');
    }

    #[test]
    fn unknown_escape() {
        let b = [0xBB, 0xFE, 0x41];
        let s = detokenize(&b, false);
        assert_eq!(s, "\\x{BBFE}A");
        assert_eq!(tokenize(&s).unwrap(), b);
    }
}
