//! Statement and expression parser for TI-BASIC, over token codes.
//!
//! Precedence, lowest first (as on the TI-84 Plus):
//! `or`/`xor` < `and` < relational (`= ≠ < > ≤ ≥`) < `+ -` < `* /` and implied
//! multiplication < negation `⁻` < `^` < postfix (`² ³ ⁻¹ !`) < primaries.
//! Closing `)`, `}` and `"` may be omitted at the end of a statement or before `→`.

use std::collections::HashMap;
use std::fmt;

use super::{split, text, texts, tk, RawStmt};

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ListName {
    /// L₁..L₆
    L(u8),
    /// ʟNAME
    Custom(String),
}

impl fmt::Display for ListName {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ListName::L(n) => write!(f, "L{}", char::from_u32('₀' as u32 + *n as u32).unwrap_or('?')),
            ListName::Custom(s) => write!(f, "ʟ{s}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Var {
    /// A..Z = 0..25, θ = 26
    Real(u8),
    /// low byte of the token: Str1..Str9 = 0..8, Str0 = 9
    Str(u8),
    List(ListName),
    Ans,
}

impl fmt::Display for Var {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Var::Real(26) => write!(f, "θ"),
            Var::Real(i) => write!(f, "{}", (b'A' + i) as char),
            Var::Str(n) => write!(f, "Str{}", (n + 1) % 10),
            Var::List(l) => write!(f, "{l}"),
            Var::Ans => write!(f, "Ans"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
    Xor,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PostOp {
    Square,
    Cube,
    Inv,
    Fact,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Num(f64),
    Str(Vec<u16>),
    Var(Var),
    ListLit(Vec<Expr>),
    Neg(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Post(PostOp, Box<Expr>),
    /// A function token (`abs(`, `randInt(`, …) and its arguments.
    Call(u16, Vec<Expr>),
    Index(ListName, Box<Expr>),
    GetKey,
    Rand,
    Const(f64),
    /// Valid TI-BASIC the debugger can't evaluate (matrices, complex numbers, system variables…).
    Unsupported(u16),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Var(Var),
    ListElem(ListName, Expr),
    Dim(ListName),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    Expr(Expr),
    Store(Expr, Target),
    If(Expr),
    Then,
    Else,
    While(Expr),
    Repeat(Expr),
    For { var: Var, start: Expr, end: Expr, step: Option<Expr> },
    End,
    Return,
    Stop,
    Lbl(String),
    Goto(String),
    Pause(Option<Expr>),
    Input { prompt: Option<Expr>, var: Var },
    Prompt(Vec<Var>),
    Disp(Vec<Expr>),
    Output(Expr, Expr, Expr),
    ClrHome,
    DelVar(Vec<Var>),
    Isg(Var, Expr),
    Dsl(Var, Expr),
    Menu { title: Expr, items: Vec<(Expr, String)> },
    Prgm(String),
    /// A command the debugger doesn't simulate (graph screen, settings…); skipped when run.
    Ignored(u16),
    Empty,
}

pub struct Program {
    pub raw: Vec<RawStmt>,
    pub stmts: Vec<Result<Stmt, String>>,
    /// Label name → index of its Lbl statement (the first one, as on the calculator).
    pub labels: HashMap<String, usize>,
}

impl Program {
    pub fn parse(codes: &[u16]) -> Program {
        let raw = split(codes);
        let stmts: Vec<Result<Stmt, String>> = raw.iter().map(|r| parse_stmt(&r.toks)).collect();
        let mut labels = HashMap::new();
        for (i, s) in stmts.iter().enumerate() {
            if let Ok(Stmt::Lbl(name)) = s {
                labels.entry(name.clone()).or_insert(i);
            }
        }
        Program { raw, stmts, labels }
    }

    pub fn line_of(&self, idx: usize) -> usize {
        self.raw.get(idx).map_or(0, |r| r.line)
    }
}

fn is_digit(c: u16) -> bool {
    (0x30..=0x39).contains(&c)
}

fn is_letter(c: u16) -> bool {
    (0x41..=0x5B).contains(&c)
}

fn is_list(c: u16) -> bool {
    c >> 8 == tk::LIST && (c & 0xFF) <= 5
}

fn is_str_var(c: u16) -> bool {
    c >> 8 == tk::STR && (c & 0xFF) <= 9
}

/// Token that is a function taking parenthesized arguments (text ends in "(").
fn is_function(c: u16) -> bool {
    text(c).ends_with('(') && !matches!(c, tk::LPAREN | tk::FOR | tk::OUTPUT | tk::MENU | tk::ISG | tk::DSL)
}

/// Can this token begin an expression?
fn starts_operand(c: u16) -> bool {
    is_digit(c)
        || is_letter(c)
        || is_list(c)
        || is_str_var(c)
        || is_function(c)
        || matches!(
            c,
            tk::DOT | tk::EE | tk::QUOTE | tk::LPAREN | tk::LBRACE | tk::LBRACKET | tk::NEG | tk::ANS | tk::PI | tk::E | tk::RAND
                | tk::GETKEY | tk::IMAG | tk::CUSTOM_LIST
        )
        // other system variables (stat results, window settings, Y-vars…)
        || matches!(c >> 8, 0x5C | 0x5E | 0x60 | 0x61 | 0x62 | 0x63)
}

struct P<'a> {
    t: &'a [u16],
    i: usize,
}

type R<T> = Result<T, String>;

impl<'a> P<'a> {
    fn peek(&self) -> Option<u16> {
        self.t.get(self.i).copied()
    }
    fn next(&mut self) -> Option<u16> {
        let c = self.peek();
        self.i += 1;
        c
    }
    fn eat(&mut self, c: u16) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn at_end(&self) -> bool {
        self.i >= self.t.len()
    }
    fn rest(&self) -> &[u16] {
        &self.t[self.i.min(self.t.len())..]
    }

    fn expect_end(&self) -> R<()> {
        match self.peek() {
            None => Ok(()),
            Some(tk::RPAREN) => Err("unmatched )".into()),
            Some(tk::SUB) => Err(neg_hint()),
            Some(c) => Err(format!("unexpected {} here", show(c))),
        }
    }

    fn comma(&mut self, what: &str) -> R<()> {
        if self.eat(tk::COMMA) {
            Ok(())
        } else {
            Err(format!("expected a comma before {what}"))
        }
    }

    // ----- expressions -----

    fn expr(&mut self) -> R<Expr> {
        let mut l = self.and()?;
        loop {
            let op = match self.peek() {
                Some(tk::OR) => BinOp::Or,
                Some(tk::XOR) => BinOp::Xor,
                _ => return Ok(l),
            };
            self.i += 1;
            let r = self.and()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }

    fn and(&mut self) -> R<Expr> {
        let mut l = self.rel()?;
        while self.eat(tk::AND) {
            let r = self.rel()?;
            l = Expr::Bin(BinOp::And, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn rel(&mut self) -> R<Expr> {
        let mut l = self.add()?;
        loop {
            let op = match self.peek() {
                Some(tk::EQ) => BinOp::Eq,
                Some(tk::NE) => BinOp::Ne,
                Some(tk::LT) => BinOp::Lt,
                Some(tk::GT) => BinOp::Gt,
                Some(tk::LE) => BinOp::Le,
                Some(tk::GE) => BinOp::Ge,
                _ => return Ok(l),
            };
            self.i += 1;
            let r = self.add()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }

    fn add(&mut self) -> R<Expr> {
        let mut l = self.mul()?;
        loop {
            let op = match self.peek() {
                Some(tk::ADD) => BinOp::Add,
                Some(tk::SUB) => BinOp::Sub,
                _ => return Ok(l),
            };
            self.i += 1;
            let r = self.mul()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }

    fn mul(&mut self) -> R<Expr> {
        let mut l = self.neg()?;
        loop {
            let op = match self.peek() {
                Some(tk::MUL) => {
                    self.i += 1;
                    BinOp::Mul
                }
                Some(tk::DIV) => {
                    self.i += 1;
                    BinOp::Div
                }
                // implied multiplication: 2A, A(B+1), 3π, 2⁻1 …
                Some(c) if starts_operand(c) && c != tk::QUOTE => BinOp::Mul,
                _ => return Ok(l),
            };
            let r = self.neg()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }

    fn neg(&mut self) -> R<Expr> {
        match self.peek() {
            Some(tk::NEG) => {
                self.i += 1;
                Ok(Expr::Neg(Box::new(self.neg()?)))
            }
            Some(tk::SUB) => Err(neg_hint()),
            _ => self.pow(),
        }
    }

    fn pow(&mut self) -> R<Expr> {
        let mut l = self.post()?;
        while self.eat(tk::POW) {
            let r = if self.eat(tk::NEG) { Expr::Neg(Box::new(self.post()?)) } else { self.post()? };
            l = Expr::Bin(BinOp::Pow, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn post(&mut self) -> R<Expr> {
        let mut e = self.primary()?;
        loop {
            let op = match self.peek() {
                Some(tk::SQUARE) => PostOp::Square,
                Some(tk::CUBE) => PostOp::Cube,
                Some(tk::INV) => PostOp::Inv,
                Some(tk::FACT) => PostOp::Fact,
                _ => return Ok(e),
            };
            self.i += 1;
            e = Expr::Post(op, Box::new(e));
        }
    }

    fn number(&mut self) -> R<Expr> {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if is_digit(c) {
                s.push((c as u8) as char);
            } else if c == tk::DOT {
                if s.contains('.') {
                    return Err("a number can't have two decimal points".into());
                }
                s.push('.');
            } else {
                break;
            }
            self.i += 1;
        }
        if self.eat(tk::EE) {
            if s.is_empty() {
                s.push('1');
            }
            s.push('e');
            if self.eat(tk::NEG) {
                s.push('-');
            }
            let before = s.len();
            while let Some(c) = self.peek().filter(|&c| is_digit(c)) {
                s.push((c as u8) as char);
                self.i += 1;
            }
            if s.len() == before {
                return Err("ᴇ needs an exponent".into());
            }
        }
        if s == "." {
            return Err("a lone decimal point isn't a number".into());
        }
        s.parse::<f64>().map(Expr::Num).map_err(|_| format!("bad number {s}"))
    }

    fn args(&mut self) -> R<Vec<Expr>> {
        let mut v = Vec::new();
        if self.eat(tk::RPAREN) || self.at_end() {
            return Ok(v);
        }
        loop {
            v.push(self.expr()?);
            if !self.eat(tk::COMMA) {
                break;
            }
        }
        self.eat(tk::RPAREN);
        Ok(v)
    }

    fn list_name(&mut self, c: u16) -> R<ListName> {
        if is_list(c) {
            return Ok(ListName::L((c & 0xFF) as u8 + 1));
        }
        // custom list: ʟ followed by 1-5 letters/θ/digits (first must be a letter)
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if name.len() < 5 && (is_letter(c) || (is_digit(c) && !name.is_empty())) {
                name.push_str(&text(c));
                self.i += 1;
            } else {
                break;
            }
        }
        if name.is_empty() {
            return Err("ʟ must be followed by a list name".into());
        }
        Ok(ListName::Custom(name))
    }

    fn primary(&mut self) -> R<Expr> {
        let Some(c) = self.peek() else {
            return Err("the expression ends too early (missing a value)".into());
        };
        if is_digit(c) || c == tk::DOT || c == tk::EE {
            return self.number();
        }
        self.i += 1;
        Ok(match c {
            _ if is_letter(c) => Expr::Var(Var::Real((c - 0x41) as u8)),
            tk::QUOTE => {
                let mut s = Vec::new();
                while let Some(c) = self.peek() {
                    if c == tk::STORE {
                        break;
                    }
                    self.i += 1;
                    if c == tk::QUOTE {
                        break;
                    }
                    s.push(c);
                }
                Expr::Str(s)
            }
            tk::ANS => Expr::Var(Var::Ans),
            tk::PI => Expr::Const(std::f64::consts::PI),
            tk::E => Expr::Const(std::f64::consts::E),
            tk::RAND => Expr::Rand,
            tk::GETKEY => Expr::GetKey,
            tk::LPAREN => {
                let e = self.expr()?;
                self.eat(tk::RPAREN);
                e
            }
            tk::LBRACE => {
                let mut items = Vec::new();
                if !self.eat(tk::RBRACE) {
                    loop {
                        items.push(self.expr()?);
                        if !self.eat(tk::COMMA) {
                            break;
                        }
                    }
                    self.eat(tk::RBRACE);
                }
                Expr::ListLit(items)
            }
            tk::LBRACKET => {
                // matrix literal: skip it, the debugger doesn't do matrices
                let mut depth = 1;
                while let Some(c) = self.next() {
                    match c {
                        tk::LBRACKET => depth += 1,
                        0x07 => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                Expr::Unsupported(tk::LBRACKET)
            }
            _ if is_list(c) || c == tk::CUSTOM_LIST => {
                let name = self.list_name(c)?;
                if self.eat(tk::LPAREN) {
                    let idx = self.expr()?;
                    self.eat(tk::RPAREN);
                    Expr::Index(name, Box::new(idx))
                } else {
                    Expr::Var(Var::List(name))
                }
            }
            _ if is_str_var(c) => Expr::Var(Var::Str((c & 0xFF) as u8)),
            _ if is_function(c) => Expr::Call(c, self.args()?),
            tk::RPAREN => return Err("unmatched )".into()),
            tk::COMMA => return Err("missing a value before ,".into()),
            tk::STORE => return Err("missing a value before →".into()),
            _ if starts_operand(c) => Expr::Unsupported(c),
            _ => return Err(format!("{} can't be used in an expression", show(c))),
        })
    }

    // ----- targets and variables -----

    fn var(&mut self) -> R<Var> {
        let Some(c) = self.next() else { return Err("expected a variable".into()) };
        if is_letter(c) {
            Ok(Var::Real((c - 0x41) as u8))
        } else if is_str_var(c) {
            Ok(Var::Str((c & 0xFF) as u8))
        } else if is_list(c) || c == tk::CUSTOM_LIST {
            Ok(Var::List(self.list_name(c)?))
        } else {
            Err(format!("{} isn't a variable", show(c)))
        }
    }

    fn real_var(&mut self) -> R<Var> {
        match self.var()? {
            v @ Var::Real(_) => Ok(v),
            v => Err(format!("{v} must be a real variable (A–Z or θ) here")),
        }
    }

    fn target(&mut self) -> R<Target> {
        match self.peek() {
            Some(tk::DIM) => {
                self.i += 1;
                let Some(c) = self.next() else { return Err("dim( needs a list".into()) };
                if !(is_list(c) || c == tk::CUSTOM_LIST) {
                    return Err("dim( needs a list".into());
                }
                let l = self.list_name(c)?;
                self.eat(tk::RPAREN);
                Ok(Target::Dim(l))
            }
            Some(tk::ANS) => Err("you can't store to Ans".into()),
            Some(c) if is_list(c) || c == tk::CUSTOM_LIST => {
                self.i += 1;
                let l = self.list_name(c)?;
                if self.eat(tk::LPAREN) {
                    let idx = self.expr()?;
                    self.eat(tk::RPAREN);
                    Ok(Target::ListElem(l, idx))
                } else {
                    Ok(Target::Var(Var::List(l)))
                }
            }
            Some(_) => Ok(Target::Var(self.var().map_err(|e| format!("can't store here: {e}"))?)),
            None => Err("→ needs a variable to store into".into()),
        }
    }

    fn label(&mut self) -> R<String> {
        let name = texts(self.rest()).trim().to_string();
        self.i = self.t.len();
        let ok = (1..=2).contains(&name.chars().count())
            && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == 'θ');
        if ok {
            Ok(name)
        } else if name.is_empty() {
            Err("missing label name".into())
        } else {
            Err(format!("bad label name \"{name}\" (1–2 characters: A–Z, 0–9, θ)"))
        }
    }

    fn stmt(&mut self) -> R<Stmt> {
        let Some(c) = self.peek() else { return Ok(Stmt::Empty) };
        let simple = |s: Stmt, p: &mut P| -> R<Stmt> {
            p.i += 1;
            p.expect_end()?;
            Ok(s)
        };
        let s = match c {
            tk::THEN => return simple(Stmt::Then, self),
            tk::ELSE => return simple(Stmt::Else, self),
            tk::END => return simple(Stmt::End, self),
            tk::RETURN => return simple(Stmt::Return, self),
            tk::STOP => return simple(Stmt::Stop, self),
            tk::CLRHOME => return simple(Stmt::ClrHome, self),
            tk::IF | tk::WHILE | tk::REPEAT => {
                self.i += 1;
                if self.at_end() {
                    return Err(format!("{}needs a condition", text(c)));
                }
                let e = self.expr()?;
                match c {
                    tk::IF => Stmt::If(e),
                    tk::WHILE => Stmt::While(e),
                    _ => Stmt::Repeat(e),
                }
            }
            tk::FOR => {
                self.i += 1;
                let var = self.real_var()?;
                self.comma("the start value")?;
                let start = self.expr()?;
                self.comma("the end value")?;
                let end = self.expr()?;
                let step = if self.eat(tk::COMMA) { Some(self.expr()?) } else { None };
                self.eat(tk::RPAREN);
                Stmt::For { var, start, end, step }
            }
            tk::LBL => {
                self.i += 1;
                return self.label().map(Stmt::Lbl);
            }
            tk::GOTO => {
                self.i += 1;
                return self.label().map(Stmt::Goto);
            }
            tk::PAUSE => {
                self.i += 1;
                Stmt::Pause(if self.at_end() { None } else { Some(self.expr()?) })
            }
            tk::INPUT => {
                self.i += 1;
                if self.at_end() {
                    return Ok(Stmt::Ignored(tk::INPUT)); // Input alone: graph cursor
                }
                let prompt = if self.peek() == Some(tk::QUOTE) || self.peek().is_some_and(is_str_var) {
                    let p = self.expr()?;
                    if self.at_end() {
                        return Err("Input with a prompt needs a variable after a comma".into());
                    }
                    self.comma("the variable")?;
                    Some(p)
                } else {
                    None
                };
                Stmt::Input { prompt, var: self.var()? }
            }
            tk::PROMPT => {
                self.i += 1;
                let mut v = vec![self.var()?];
                while self.eat(tk::COMMA) {
                    v.push(self.var()?);
                }
                Stmt::Prompt(v)
            }
            tk::DISP => {
                self.i += 1;
                let mut v = Vec::new();
                if !self.at_end() {
                    loop {
                        v.push(self.expr()?);
                        if !self.eat(tk::COMMA) {
                            break;
                        }
                    }
                }
                Stmt::Disp(v)
            }
            tk::OUTPUT => {
                self.i += 1;
                let r = self.expr()?;
                self.comma("the column")?;
                let col = self.expr()?;
                self.comma("the value")?;
                let v = self.expr()?;
                self.eat(tk::RPAREN);
                Stmt::Output(r, col, v)
            }
            tk::DELVAR => {
                let mut v = Vec::new();
                while self.eat(tk::DELVAR) {
                    v.push(self.var()?);
                }
                Stmt::DelVar(v)
            }
            tk::ISG | tk::DSL => {
                self.i += 1;
                let var = self.real_var()?;
                self.comma("the limit")?;
                let lim = self.expr()?;
                self.eat(tk::RPAREN);
                if c == tk::ISG { Stmt::Isg(var, lim) } else { Stmt::Dsl(var, lim) }
            }
            tk::MENU => {
                self.i += 1;
                let title = self.expr()?;
                let mut items = Vec::new();
                while self.eat(tk::COMMA) {
                    let opt = self.expr()?;
                    self.comma("the label")?;
                    let start = self.i;
                    while self.peek().is_some_and(|c| c != tk::COMMA && c != tk::RPAREN) {
                        self.i += 1;
                    }
                    let lbl = texts(&self.t[start..self.i]).trim().to_string();
                    if lbl.is_empty() {
                        return Err("Menu( option is missing its label".into());
                    }
                    items.push((opt, lbl));
                }
                self.eat(tk::RPAREN);
                if items.is_empty() {
                    return Err("Menu( needs at least one option".into());
                }
                Stmt::Menu { title, items }
            }
            tk::PRGM => {
                self.i += 1;
                let name = texts(self.rest()).trim().to_string();
                self.i = self.t.len();
                if name.is_empty() {
                    return Err("prgm needs a program name".into());
                }
                Stmt::Prgm(name)
            }
            tk::SUB => return Err(neg_hint()),
            _ if starts_operand(c) => {
                let e = self.expr()?;
                if self.eat(tk::STORE) {
                    let t = self.target()?;
                    self.expect_end()?;
                    return Ok(Stmt::Store(e, t));
                }
                match e {
                    // A bare call to something that isn't a known function is a command
                    // such as Line( or Text( — skipped by the debugger.
                    Expr::Call(code, _) if !FUNCS.contains(&code) => Stmt::Ignored(code),
                    e => Stmt::Expr(e),
                }
            }
            _ => return Ok(Stmt::Ignored(c)),
        };
        self.expect_end()?;
        Ok(s)
    }
}

/// Functions the interpreter evaluates.
pub const FUNCS: &[u16] = &[
    tk::ROUND, tk::MAX, tk::MIN, tk::MEAN, tk::INT, tk::ABS, tk::DIM, tk::SUM, tk::PROD, tk::NOT, tk::IPART, tk::FPART,
    tk::SQRT, tk::LN, tk::EXP, tk::LOG, tk::TENPOW, tk::SIN, tk::ASIN, tk::COS, tk::ACOS, tk::TAN, tk::ATAN, tk::LCM,
    tk::GCD, tk::RANDINT, tk::SUB_STR, tk::INSTRING, tk::LENGTH, tk::REMAINDER, tk::TOSTRING,
];

fn neg_hint() -> String {
    "use the negative sign ⁻ here, not subtract (-); the calculator would show ERR:SYNTAX".into()
}

fn show(c: u16) -> String {
    format!("\"{}\"", text(c).trim())
}

pub fn parse_stmt(toks: &[u16]) -> Result<Stmt, String> {
    P { t: toks, i: 0 }.stmt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::basic::codes;
    use crate::tokens::tokenize;

    fn p(s: &str) -> Result<Stmt, String> {
        parse_stmt(&codes(&tokenize(s).unwrap()))
    }

    #[test]
    fn statements() {
        assert!(matches!(p("Disp \"HI\",A+1").unwrap(), Stmt::Disp(v) if v.len() == 2));
        assert!(matches!(p("For(I,1,10,2").unwrap(), Stmt::For { step: Some(_), .. }));
        assert_eq!(p("Lbl A1").unwrap(), Stmt::Lbl("A1".into()));
        assert!(p("Lbl ABC").is_err());
        assert!(matches!(p("3→L₁(2)").unwrap(), Stmt::Store(_, Target::ListElem(ListName::L(1), _))));
        assert!(matches!(p("5→dim(ʟABC").unwrap(), Stmt::Store(_, Target::Dim(ListName::Custom(n))) if n == "ABC"));
        assert!(matches!(p("Input \"N=\",N").unwrap(), Stmt::Input { prompt: Some(_), .. }));
        assert!(matches!(p("Line(0,0,1,1)").unwrap(), Stmt::Ignored(_)));
        assert!(matches!(p("Menu(\"GO\",\"A\",1,\"B\",B2)").unwrap(), Stmt::Menu { items, .. } if items[1].1 == "B2"));
    }

    #[test]
    fn precedence() {
        // ⁻2² = ⁻(2²)
        assert!(matches!(p("⁻2²").unwrap(), Stmt::Expr(Expr::Neg(_))));
        // implied multiplication
        assert!(matches!(p("2A").unwrap(), Stmt::Expr(Expr::Bin(BinOp::Mul, _, _))));
        // closing paren optional
        assert!(p("Disp abs(⁻3").is_ok());
    }

    #[test]
    fn errors() {
        assert!(p("Disp -3").unwrap_err().contains("negative sign"));
        assert!(p("A+").is_err());
        assert!(p("3)").unwrap_err().contains("unmatched )"));
        assert!(p("3→Ans").is_err());
        assert!(p("If ").is_err());
    }
}
