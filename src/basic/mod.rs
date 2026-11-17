//! TI-BASIC tooling for the Program Editor: a parser, a problem checker and a
//! step-through interpreter for a subset of the language.
//!
//! Everything works on token codes (the bytes the calculator stores), not on text, so the
//! debugger sees exactly what the calculator would. A code above 0xFF is a two-byte token.

pub mod check;
pub mod interp;
pub mod parse;
pub mod sim;
pub mod suggest;

use crate::tokens;

/// Token codes used by the parser and interpreter.
pub mod tk {
    pub const STORE: u16 = 0x04;
    pub const LBRACKET: u16 = 0x06;
    pub const LBRACE: u16 = 0x08;
    pub const RBRACE: u16 = 0x09;
    pub const INV: u16 = 0x0C;
    pub const SQUARE: u16 = 0x0D;
    pub const CUBE: u16 = 0x0F;
    pub const LPAREN: u16 = 0x10;
    pub const RPAREN: u16 = 0x11;
    pub const ROUND: u16 = 0x12;
    pub const MAX: u16 = 0x19;
    pub const MIN: u16 = 0x1A;
    pub const MEAN: u16 = 0x21;
    pub const QUOTE: u16 = 0x2A;
    pub const COMMA: u16 = 0x2B;
    pub const IMAG: u16 = 0x2C;
    pub const FACT: u16 = 0x2D;
    pub const DOT: u16 = 0x3A;
    pub const EE: u16 = 0x3B;
    pub const OR: u16 = 0x3C;
    pub const XOR: u16 = 0x3D;
    pub const COLON: u16 = 0x3E;
    pub const NEWLINE: u16 = 0x3F;
    pub const AND: u16 = 0x40;
    pub const THETA: u16 = 0x5B;
    pub const LIST: u16 = 0x5D; // L₁..L₆ = 5D00..5D05
    pub const PRGM: u16 = 0x5F;
    pub const EQ: u16 = 0x6A;
    pub const LT: u16 = 0x6B;
    pub const GT: u16 = 0x6C;
    pub const LE: u16 = 0x6D;
    pub const GE: u16 = 0x6E;
    pub const NE: u16 = 0x6F;
    pub const ADD: u16 = 0x70;
    pub const SUB: u16 = 0x71;
    pub const ANS: u16 = 0x72;
    pub const MUL: u16 = 0x82;
    pub const DIV: u16 = 0x83;
    pub const STR: u16 = 0xAA; // Str1..Str9,Str0 = AA00..AA09
    pub const RAND: u16 = 0xAB;
    pub const PI: u16 = 0xAC;
    pub const GETKEY: u16 = 0xAD;
    pub const NEG: u16 = 0xB0;
    pub const INT: u16 = 0xB1;
    pub const ABS: u16 = 0xB2;
    pub const DIM: u16 = 0xB5;
    pub const SUM: u16 = 0xB6;
    pub const PROD: u16 = 0xB7;
    pub const NOT: u16 = 0xB8;
    pub const IPART: u16 = 0xB9;
    pub const FPART: u16 = 0xBA;
    pub const SQRT: u16 = 0xBC;
    pub const LN: u16 = 0xBE;
    pub const EXP: u16 = 0xBF;
    pub const LOG: u16 = 0xC0;
    pub const TENPOW: u16 = 0xC1;
    pub const SIN: u16 = 0xC2;
    pub const ASIN: u16 = 0xC3;
    pub const COS: u16 = 0xC4;
    pub const ACOS: u16 = 0xC5;
    pub const TAN: u16 = 0xC6;
    pub const ATAN: u16 = 0xC7;
    pub const IF: u16 = 0xCE;
    pub const THEN: u16 = 0xCF;
    pub const ELSE: u16 = 0xD0;
    pub const WHILE: u16 = 0xD1;
    pub const REPEAT: u16 = 0xD2;
    pub const FOR: u16 = 0xD3;
    pub const END: u16 = 0xD4;
    pub const RETURN: u16 = 0xD5;
    pub const LBL: u16 = 0xD6;
    pub const GOTO: u16 = 0xD7;
    pub const PAUSE: u16 = 0xD8;
    pub const STOP: u16 = 0xD9;
    pub const ISG: u16 = 0xDA;
    pub const DSL: u16 = 0xDB;
    pub const INPUT: u16 = 0xDC;
    pub const PROMPT: u16 = 0xDD;
    pub const DISP: u16 = 0xDE;
    pub const OUTPUT: u16 = 0xE0;
    pub const CLRHOME: u16 = 0xE1;
    pub const MENU: u16 = 0xE6;
    pub const CUSTOM_LIST: u16 = 0xEB; // ʟ
    pub const POW: u16 = 0xF0;
    pub const LCM: u16 = 0xBB08;
    pub const GCD: u16 = 0xBB09;
    pub const RANDINT: u16 = 0xBB0A;
    pub const SUB_STR: u16 = 0xBB0C;
    pub const INSTRING: u16 = 0xBB0F;
    pub const LENGTH: u16 = 0xBB2B;
    pub const E: u16 = 0xBB31;
    pub const DELVAR: u16 = 0xBB54;
    pub const REMAINDER: u16 = 0xEF32;
    pub const TOSTRING: u16 = 0xEF97;
}

/// Split token bytes into codes (two-byte tokens become one u16).
pub fn codes(bytes: &[u8]) -> Vec<u16> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if tokens::is_prefix(bytes[i]) {
            let second = bytes.get(i + 1).copied().unwrap_or(0);
            out.push(((bytes[i] as u16) << 8) | second as u16);
            i += 2;
        } else {
            out.push(bytes[i] as u16);
            i += 1;
        }
    }
    out
}

/// Text of one token code (readable glyphs).
pub fn text(code: u16) -> String {
    let t = tokens::TokenTable::get();
    t.text(code, false).map(str::to_string).unwrap_or_else(|| format!("\\x{{{code:X}}}"))
}

pub fn texts(codes: &[u16]) -> String {
    codes.iter().map(|&c| text(c)).collect()
}

/// One statement: its tokens and the 1-based source line it starts on.
#[derive(Clone, Debug)]
pub struct RawStmt {
    pub line: usize,
    pub toks: Vec<u16>,
}

/// Split a program into statements at newlines and at colons outside strings.
/// A string ends at a closing quote, a newline or a store arrow, as on the calculator.
pub fn split(codes: &[u16]) -> Vec<RawStmt> {
    let mut out = Vec::new();
    let mut line = 1;
    let mut cur = RawStmt { line, toks: Vec::new() };
    let mut in_str = false;
    for &c in codes {
        if c == tk::NEWLINE || (c == tk::COLON && !in_str) {
            out.push(std::mem::replace(&mut cur, RawStmt { line, toks: Vec::new() }));
            if c == tk::NEWLINE {
                line += 1;
            }
            cur.line = line;
            in_str = false;
            continue;
        }
        if c == tk::QUOTE {
            in_str = !in_str;
        } else if c == tk::STORE {
            in_str = false;
        }
        cur.toks.push(c);
    }
    out.push(cur);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_match_token_table() {
        let t = tokens::TokenTable::get();
        let check = |c: u16, s: &str| assert_eq!(t.text(c, false), Some(s), "code {c:#06x}");
        check(tk::IF, "If ");
        check(tk::PAUSE, "Pause ");
        check(tk::INPUT, "Input ");
        check(tk::DISP, "Disp ");
        check(tk::NEG, "⁻");
        check(tk::TENPOW, "₁₀^(");
        check(tk::DELVAR, "DelVar ");
        check(tk::REMAINDER, "remainder(");
        check(tk::TOSTRING, "toString(");
        check(tk::CUSTOM_LIST, "ʟ");
        check(tk::LIST << 8, "L₁");
        check(tk::STR << 8, "Str1");
    }

    #[test]
    fn splitting() {
        let toks = codes(&tokens::tokenize("Disp \"A:B\":ClrHome\nIf 1:Then").unwrap());
        let s = split(&toks);
        assert_eq!(s.len(), 4);
        assert_eq!((s[0].line, texts(&s[0].toks).as_str()), (1, "Disp \"A:B\""));
        assert_eq!((s[2].line, texts(&s[2].toks).as_str()), (2, "If 1"));
    }
}
