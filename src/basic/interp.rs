//! Step-through TI-BASIC interpreter for the Program Editor's debugger.
//!
//! It runs a subset of TI-BASIC on a simulated home screen: reals, strings and lists;
//! Disp, Output(, ClrHome, Input, Prompt, Pause, Menu(, getKey; If/Then/Else,
//! For(/While/Repeat/End, Goto/Lbl, IS>(/DS<(, Return/Stop; common math, list and string
//! functions. Graph-screen and other commands it doesn't simulate are skipped and reported.
//! Errors use the calculator's names (ERR:DIVIDE BY 0, ERR:LABEL, …).

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use super::parse::{BinOp, Expr, ListName, PostOp, Program, Stmt, Target, Var};
use super::{codes, text, texts, tk};
use crate::tokens;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Num(f64),
    List(Vec<f64>),
    /// String contents as token codes
    Str(Vec<u16>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Stopped between statements (before the first one, after a step, at a breakpoint…).
    Paused,
    Running,
    /// Waiting for Input/Prompt.
    Input { prompt: String, var: Var },
    /// The program executed Pause; waiting for Enter.
    PauseCmd,
    /// Waiting for a Menu( choice.
    Menu { title: String, items: Vec<(String, String)> },
    Done,
    Error { line: usize, msg: String },
}

#[derive(Clone, Debug)]
enum Frame {
    If,
    For { var: u8, end: f64, step: f64, body: usize },
    While { at: usize },
    Repeat { at: usize },
}

#[derive(Clone, Debug)]
pub struct Screen {
    pub cols: usize,
    pub rows: usize,
    pub cells: Vec<Vec<char>>,
    /// Next row Disp writes to.
    row: usize,
}

impl Screen {
    /// Row the next Disp/entry line goes to (== rows when the next line will scroll).
    pub fn next_row(&self) -> usize {
        self.row
    }
    /// Make room for one more line (scrolling if needed) and return its row, without writing.
    pub fn reserve_row(&mut self) -> usize {
        if self.row >= self.rows {
            self.cells.remove(0);
            self.cells.push(vec![' '; self.cols]);
            self.row = self.rows - 1;
        }
        self.row
    }
}

impl Screen {
    pub fn new(cols: usize, rows: usize) -> Screen {
        Screen { cols, rows, cells: vec![vec![' '; cols]; rows], row: 0 }
    }
    pub fn clear(&mut self) {
        for r in &mut self.cells {
            r.fill(' ');
        }
        self.row = 0;
    }
    fn newline_row(&mut self) -> usize {
        if self.row >= self.rows {
            self.cells.remove(0);
            self.cells.push(vec![' '; self.cols]);
            self.row = self.rows - 1;
        }
        let r = self.row;
        self.row += 1;
        r
    }
    /// Disp-style line: strings left-aligned, numbers right-aligned.
    pub fn line(&mut self, s: &str, right: bool) {
        let r = self.newline_row();
        let chars: Vec<char> = s.chars().take(self.cols).collect();
        let start = if right { self.cols - chars.len() } else { 0 };
        self.cells[r] = vec![' '; self.cols];
        for (i, c) in chars.into_iter().enumerate() {
            self.cells[r][start + i] = c;
        }
    }
    /// Output(-style write at (row, col), 0-based, wrapping to following rows.
    fn write_at(&mut self, mut r: usize, mut c: usize, s: &str) {
        for ch in s.chars() {
            if r >= self.rows {
                break;
            }
            self.cells[r][c] = ch;
            c += 1;
            if c >= self.cols {
                c = 0;
                r += 1;
            }
        }
    }
    pub fn text(&self) -> String {
        self.cells.iter().map(|r| r.iter().collect::<String>()).collect::<Vec<_>>().join("\n")
    }
}

pub struct Machine {
    prog: Program,
    pub pc: usize,
    pub state: State,
    /// true: return to Paused after each statement / input (stepping)
    stepping: bool,
    reals: [f64; 27],
    strs: [Option<Vec<u16>>; 10],
    lists: BTreeMap<ListName, Vec<f64>>,
    ans: Value,
    stack: Vec<Frame>,
    pub screen: Screen,
    /// Source lines with a breakpoint.
    pub breakpoints: BTreeSet<usize>,
    skip_bp_at: Option<usize>,
    key: u16,
    rng: u64,
    pending_prompts: Vec<Var>,
    /// Commands that were skipped because the debugger doesn't simulate them.
    pub skipped: BTreeSet<String>,
    /// Variables the program has read or written.
    pub used: BTreeSet<Var>,
    pub steps: u64,
    /// Programs that `prgmNAME` can call (name → source text).
    pub programs: BTreeMap<String, String>,
    calls: Vec<(Program, usize, Vec<Frame>)>,
    /// Whether the last statement executed produced a value (expression or store).
    pub last_was_value: bool,
}

type R<T> = Result<T, String>;

fn err<T>(s: &str) -> R<T> {
    Err(s.to_string())
}

fn truthy(x: f64) -> bool {
    x != 0.0
}

fn b(x: bool) -> f64 {
    if x {
        1.0
    } else {
        0.0
    }
}

/// Approximate equality the way the calculator compares (≈10 significant digits).
fn same(a: f64, b: f64) -> bool {
    a == b || (a - b).abs() <= 1e-10 * a.abs().max(b.abs())
}

/// Format a number like the calculator in Float mode (10 significant digits).
pub fn fmt_num(x: f64) -> String {
    if x == 0.0 || !x.is_finite() {
        return if x.is_finite() { "0".into() } else { "ERR".into() };
    }
    if x.fract() == 0.0 && x.abs() < 1e10 {
        return format!("{}", x as i64);
    }
    let sci = format!("{:.9e}", x);
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let trim = |s: String| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    };
    if (-3..10).contains(&exp) {
        let decimals = (9 - exp).max(0) as usize;
        let s = trim(format!("{:.*}", decimals, x));
        if let Some(rest) = s.strip_prefix("0.") {
            format!(".{rest}")
        } else if let Some(rest) = s.strip_prefix("-0.") {
            format!("-.{rest}")
        } else {
            s
        }
    } else {
        format!("{}ᴇ{}", trim(mant.to_string()), exp)
    }
}

pub fn show(v: &Value) -> String {
    match v {
        Value::Num(x) => fmt_num(*x),
        Value::Str(t) => texts(t),
        Value::List(l) => format!("{{{}}}", l.iter().map(|x| fmt_num(*x)).collect::<Vec<_>>().join(" ")),
    }
}

impl Machine {
    /// Build a machine for program text, on a `cols`×`rows` home screen
    /// (26×10 for the TI-84 Plus CE, 16×8 for the TI-84 Plus).
    pub fn new(text: &str, cols: usize, rows: usize) -> R<Machine> {
        let bytes = tokens::tokenize(text).map_err(|e| e.to_string())?;
        let prog = Program::parse(&codes(&bytes));
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64) | 1;
        Ok(Machine {
            prog,
            pc: 0,
            state: State::Paused,
            stepping: true,
            reals: [0.0; 27],
            strs: Default::default(),
            lists: (1..=6).map(|i| (ListName::L(i), Vec::new())).collect(),
            ans: Value::Num(0.0),
            stack: Vec::new(),
            screen: Screen::new(cols, rows),
            breakpoints: BTreeSet::new(),
            skip_bp_at: None,
            key: 0,
            rng: seed,
            pending_prompts: Vec::new(),
            skipped: BTreeSet::new(),
            used: BTreeSet::new(),
            steps: 0,
            programs: BTreeMap::new(),
            calls: Vec::new(),
            last_was_value: false,
        })
    }

    /// Source line of the next statement to run.
    pub fn current_line(&self) -> Option<usize> {
        (self.pc < self.prog.stmts.len()).then(|| self.prog.line_of(self.pc))
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.state, State::Done | State::Error { .. })
    }

    /// Continue running (until a breakpoint, input, the end or an error).
    pub fn resume(&mut self) {
        if matches!(self.state, State::Paused) {
            self.stepping = false;
            self.skip_bp_at = Some(self.pc);
            self.state = State::Running;
        }
    }

    pub fn pause(&mut self) {
        self.stepping = true;
        if self.state == State::Running {
            self.state = State::Paused;
        }
    }

    /// Run exactly one statement.
    pub fn step(&mut self) {
        if matches!(self.state, State::Paused) {
            self.stepping = true;
            self.state = State::Running;
            self.exec_one();
            if self.state == State::Running {
                self.state = State::Paused;
            }
        }
    }

    /// Run for up to `budget` while Running.
    pub fn run_for(&mut self, budget: Duration) {
        let t0 = Instant::now();
        while self.state == State::Running && !self.stepping {
            if let Some(line) = self.current_line() {
                if self.breakpoints.contains(&line) && self.skip_bp_at != Some(self.pc) {
                    self.stepping = true;
                    self.state = State::Paused;
                    return;
                }
            }
            self.skip_bp_at = None;
            self.exec_one();
            if self.steps % 256 == 0 && t0.elapsed() >= budget {
                return;
            }
        }
    }

    /// Answer an Input/Prompt. On a bad entry the machine keeps waiting and the error is returned.
    pub fn give_input(&mut self, s: &str) -> R<()> {
        let State::Input { var, .. } = self.state.clone() else { return Ok(()) };
        let toks = codes(&tokens::tokenize(s).map_err(|e| e.to_string())?);
        let val = if let Var::Str(_) = var {
            Value::Str(toks)
        } else {
            let e = match super::parse::parse_stmt(&toks)? {
                Stmt::Expr(e) => e,
                _ => return err("enter a value"),
            };
            self.eval(&e)?
        };
        self.screen.line(&show(&val), true);
        self.store_var(&var, val)?;
        if !self.pending_prompts.is_empty() {
            let v = self.pending_prompts.remove(0);
            self.ask(format!("{v}=?"), v);
        } else {
            self.after_wait();
        }
        Ok(())
    }

    /// Continue after Pause.
    pub fn enter(&mut self) {
        if self.state == State::PauseCmd {
            self.after_wait();
        }
    }

    /// Choose a Menu( item (0-based).
    pub fn choose(&mut self, i: usize) {
        let State::Menu { items, .. } = self.state.clone() else { return };
        let Some((_, label)) = items.get(i) else { return };
        match self.prog.labels.get(label) {
            Some(&idx) => {
                self.pc = idx + 1;
                self.after_wait();
            }
            None => self.fail(format!("ERR:LABEL (no Lbl {label})")),
        }
    }

    /// A key press for getKey (TI key code: 105 Enter, 25 up, 34 down, 45 Clear…).
    pub fn press_key(&mut self, code: u16) {
        self.key = code;
    }

    /// Variables used so far, with their values.
    pub fn variables(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = vec![("Ans".into(), show(&self.ans))];
        for v in &self.used {
            let val = match v {
                Var::Real(i) => Some(Value::Num(self.reals[*i as usize])),
                Var::Str(n) => self.strs[*n as usize].clone().map(Value::Str),
                Var::List(l) => self.lists.get(l).cloned().map(Value::List),
                Var::Ans => continue,
            };
            let shown = match val {
                Some(Value::Str(t)) => format!("\"{}\"", texts(&t)),
                Some(v) => show(&v),
                None => "undefined".into(),
            };
            out.push((v.to_string(), shown));
        }
        out
    }

    /// Run token codes as a new program on the same variables and screen (a home-screen entry,
    /// or a whole program). Runs until done, an error, or input is needed; continue with `run_for`.
    pub fn run_codes(&mut self, codes: &[u16]) {
        self.prog = Program::parse(codes);
        self.pc = 0;
        self.stack.clear();
        self.calls.clear();
        self.pending_prompts.clear();
        self.last_was_value = false;
        self.stepping = false;
        self.skip_bp_at = None;
        self.state = State::Running;
    }

    pub fn ans(&self) -> &Value {
        &self.ans
    }

    /// Stop a running program as the ON key does.
    pub fn break_program(&mut self) {
        if !matches!(self.state, State::Done | State::Error { .. }) {
            let line = self.current_line().unwrap_or(0);
            self.calls.clear();
            self.state = State::Error { line, msg: "ERR:BREAK".into() };
        }
    }

    /// Evaluate a parsed expression with X set to `x` (for graphing); X is restored afterwards.
    pub fn eval_at_x(&mut self, e: &Expr, x: f64) -> Option<f64> {
        const X: usize = 23;
        let saved = self.reals[X];
        self.reals[X] = x;
        let r = match self.eval(e) {
            Ok(Value::Num(y)) if y.is_finite() => Some(y),
            _ => None,
        };
        self.reals[X] = saved;
        r
    }

    /// Value of a real variable (A–Z = 0–25, θ = 26).
    pub fn real(&self, i: usize) -> f64 {
        self.reals[i]
    }

    /// Open loops and If-Thens, innermost last, as "For( line 3" etc.
    pub fn block_stack(&self) -> Vec<String> {
        self.stack
            .iter()
            .map(|f| match f {
                Frame::If => "If…Then".to_string(),
                Frame::For { var, .. } => format!("For( {}", Var::Real(*var)),
                Frame::While { at } => format!("While (line {})", self.prog.line_of(*at)),
                Frame::Repeat { at } => format!("Repeat (line {})", self.prog.line_of(*at)),
            })
            .collect()
    }

    fn after_wait(&mut self) {
        self.state = if self.stepping { State::Paused } else { State::Running };
    }

    fn ask(&mut self, prompt: String, var: Var) {
        self.screen.line(&prompt, false);
        self.state = State::Input { prompt, var };
    }

    fn fail(&mut self, msg: String) {
        let line = self.prog.line_of(self.pc.min(self.prog.stmts.len().saturating_sub(1)));
        self.state = State::Error { line, msg };
    }

    /// End of the current program: return to the caller, or finish.
    fn finish_or_return(&mut self) {
        match self.calls.pop() {
            Some((prog, pc, stack)) => {
                self.prog = prog;
                self.pc = pc;
                self.stack = stack;
            }
            None => {
                self.pc = self.prog.stmts.len();
                self.state = State::Done;
            }
        }
    }

    fn exec_one(&mut self) {
        if self.pc >= self.prog.stmts.len() {
            self.finish_or_return();
            return;
        }
        self.steps += 1;
        let at = self.pc;
        if let Err(msg) = self.exec(at) {
            self.pc = at;
            self.fail(msg);
        } else if self.pc >= self.prog.stmts.len() && self.state == State::Running {
            self.finish_or_return();
        }
    }

    /// After a block opener at `from`, find the statement after its matching End
    /// (or after its Else, when `stop_at_else`). Returns (index, stopped_at_else).
    fn skip_block(&self, from: usize, stop_at_else: bool) -> (usize, bool) {
        let mut depth = 0;
        for j in from + 1..self.prog.stmts.len() {
            match &self.prog.stmts[j] {
                Ok(Stmt::Then | Stmt::For { .. } | Stmt::While(_) | Stmt::Repeat(_)) => depth += 1,
                Ok(Stmt::End) if depth == 0 => return (j + 1, false),
                Ok(Stmt::End) => depth -= 1,
                Ok(Stmt::Else) if depth == 0 && stop_at_else => return (j + 1, true),
                _ => {}
            }
        }
        (self.prog.stmts.len(), false)
    }

    fn exec(&mut self, at: usize) -> R<()> {
        let stmt = match &self.prog.stmts[at] {
            Ok(s) => s.clone(),
            Err(msg) => return Err(format!("ERR:SYNTAX ({msg})")),
        };
        self.last_was_value = matches!(stmt, Stmt::Expr(_) | Stmt::Store(..));
        self.pc = at + 1;
        match stmt {
            Stmt::Empty | Stmt::Lbl(_) => {}
            Stmt::Ignored(code) => {
                self.skipped.insert(text(code).trim().to_string());
            }
            Stmt::Expr(e) => self.ans = self.eval(&e)?,
            Stmt::Store(e, target) => {
                let v = self.eval(&e)?;
                self.store(&target, v.clone())?;
                self.ans = v;
            }
            Stmt::If(cond) => {
                let t = truthy(self.num(&cond)?);
                let next = at + 1;
                if matches!(self.prog.stmts.get(next), Some(Ok(Stmt::Then))) {
                    if t {
                        self.stack.push(Frame::If);
                        self.pc = next + 1;
                    } else {
                        let (to, at_else) = self.skip_block(next, true);
                        if at_else {
                            self.stack.push(Frame::If);
                        }
                        self.pc = to;
                    }
                } else if !t {
                    self.pc = next + 1;
                }
            }
            Stmt::Then => return err("ERR:SYNTAX (Then without If)"),
            Stmt::Else => {
                // reached at the end of the true branch: skip the else branch
                match self.stack.pop() {
                    Some(Frame::If) => self.pc = self.skip_block(at, false).0,
                    _ => return err("ERR:SYNTAX (Else without If…Then)"),
                }
            }
            Stmt::For { var, start, end, step } => {
                let Var::Real(v) = var else { return err("ERR:SYNTAX") };
                let s = self.num(&start)?;
                let e = self.num(&end)?;
                let st = match &step {
                    Some(x) => self.num(x)?,
                    None => 1.0,
                };
                if st == 0.0 {
                    return err("ERR:INCREMENT (step of 0)");
                }
                self.set_real(v, s);
                if (st > 0.0 && s > e) || (st < 0.0 && s < e) {
                    self.pc = self.skip_block(at, false).0;
                } else {
                    self.stack.push(Frame::For { var: v, end: e, step: st, body: at + 1 });
                }
            }
            Stmt::While(cond) => {
                if truthy(self.num(&cond)?) {
                    self.stack.push(Frame::While { at });
                } else {
                    self.pc = self.skip_block(at, false).0;
                }
            }
            Stmt::Repeat(_) => self.stack.push(Frame::Repeat { at }),
            Stmt::End => match self.stack.pop() {
                None => return err("ERR:SYNTAX (End without a block)"),
                Some(Frame::If) => {}
                Some(Frame::For { var, end, step, body }) => {
                    let v = self.reals[var as usize] + step;
                    self.set_real(var, v);
                    if (step > 0.0 && v <= end) || (step < 0.0 && v >= end) {
                        self.stack.push(Frame::For { var, end, step, body });
                        self.pc = body;
                    }
                }
                Some(Frame::While { at: w }) => self.pc = w,
                Some(Frame::Repeat { at: r }) => {
                    let Ok(Stmt::Repeat(cond)) = self.prog.stmts[r].clone() else { return err("ERR:SYNTAX") };
                    if !truthy(self.num(&cond)?) {
                        self.stack.push(Frame::Repeat { at: r });
                        self.pc = r + 1;
                    }
                }
            },
            Stmt::Goto(name) => match self.prog.labels.get(&name) {
                Some(&i) => {
                    self.pc = i + 1;
                    if self.stack.len() > 1000 {
                        return err("ERR:MEMORY (Goto keeps jumping out of loops/If…Then blocks)");
                    }
                }
                None => return Err(format!("ERR:LABEL (no Lbl {name})")),
            },
            Stmt::Return => {
                self.pc = self.prog.stmts.len();
                self.finish_or_return();
            }
            Stmt::Stop => {
                self.calls.clear();
                self.pc = self.prog.stmts.len();
                self.state = State::Done;
            }
            Stmt::Pause(e) => {
                if let Some(e) = e {
                    let v = self.eval(&e)?;
                    self.disp(&v);
                }
                self.state = State::PauseCmd;
            }
            Stmt::Input { prompt, var } => {
                let p = match prompt {
                    Some(e) => show(&self.eval(&e)?),
                    None => "?".into(),
                };
                self.used.insert(var.clone());
                self.ask(p, var);
            }
            Stmt::Prompt(vars) => {
                for v in &vars {
                    self.used.insert(v.clone());
                }
                let mut vars = vars;
                let first = vars.remove(0);
                self.pending_prompts = vars;
                self.ask(format!("{first}=?"), first);
            }
            Stmt::Disp(items) => {
                for e in &items {
                    let v = self.eval(e)?;
                    self.disp(&v);
                }
            }
            Stmt::Output(r, c, e) => {
                let r = self.num(&r)?;
                let c = self.num(&c)?;
                let v = self.eval(&e)?;
                if r < 1.0 || c < 1.0 || r as usize > self.screen.rows || c as usize > self.screen.cols || r.fract() != 0.0 || c.fract() != 0.0 {
                    return Err(format!("ERR:DOMAIN (Output( position {r},{c} is off the {}×{} screen)", self.screen.cols, self.screen.rows));
                }
                self.screen.write_at(r as usize - 1, c as usize - 1, &show(&v));
            }
            Stmt::ClrHome => self.screen.clear(),
            Stmt::DelVar(vars) => {
                for v in vars {
                    match &v {
                        Var::Real(i) => self.reals[*i as usize] = 0.0,
                        Var::Str(n) => self.strs[*n as usize] = None,
                        Var::List(l) => {
                            self.lists.remove(l);
                        }
                        Var::Ans => {}
                    }
                }
            }
            Stmt::Isg(Var::Real(v), lim) => {
                let x = self.reals[v as usize] + 1.0;
                self.set_real(v, x);
                if x > self.num(&lim)? {
                    self.pc += 1;
                }
            }
            Stmt::Dsl(Var::Real(v), lim) => {
                let x = self.reals[v as usize] - 1.0;
                self.set_real(v, x);
                if x < self.num(&lim)? {
                    self.pc += 1;
                }
            }
            Stmt::Isg(..) | Stmt::Dsl(..) => return err("ERR:SYNTAX"),
            Stmt::Menu { title, items } => {
                let title = show(&self.eval(&title)?);
                let mut list = Vec::new();
                for (e, label) in items {
                    list.push((show(&self.eval(&e)?), label));
                }
                self.state = State::Menu { title, items: list };
            }
            Stmt::Prgm(name) => {
                let Some(text) = self.programs.get(&name).cloned() else {
                    return Err(format!("ERR:UNDEFINED (no program {name} loaded)"));
                };
                if self.calls.len() >= 64 {
                    return err("ERR:MEMORY (programs calling each other too deeply)");
                }
                let bytes = tokens::tokenize(&text).map_err(|e| format!("prgm{name}: {e}"))?;
                let callee = Program::parse(&codes(&bytes));
                let caller = std::mem::replace(&mut self.prog, callee);
                self.calls.push((caller, self.pc, std::mem::take(&mut self.stack)));
                self.pc = 0;
            }
        }
        Ok(())
    }

    fn disp(&mut self, v: &Value) {
        let right = !matches!(v, Value::Str(_));
        self.screen.line(&show(v), right);
    }

    fn set_real(&mut self, i: u8, x: f64) {
        self.used.insert(Var::Real(i));
        self.reals[i as usize] = x;
    }

    fn store_var(&mut self, var: &Var, v: Value) -> R<()> {
        self.used.insert(var.clone());
        match (var, v) {
            (Var::Real(i), Value::Num(x)) => self.reals[*i as usize] = x,
            (Var::Str(n), Value::Str(s)) => self.strs[*n as usize] = Some(s),
            (Var::List(l), Value::List(x)) => {
                self.lists.insert(l.clone(), x);
            }
            (Var::List(_), Value::Num(_)) => return err("ERR:DATA TYPE (a list needs { … })"),
            (Var::Ans, _) => return err("ERR:SYNTAX"),
            (v, _) => return Err(format!("ERR:DATA TYPE (wrong kind of value for {v})")),
        }
        Ok(())
    }

    fn store(&mut self, t: &Target, v: Value) -> R<()> {
        match t {
            Target::Var(var) => self.store_var(var, v),
            Target::ListElem(l, idx) => {
                let i = self.num(idx)?;
                let Value::Num(x) = v else { return err("ERR:DATA TYPE") };
                self.used.insert(Var::List(l.clone()));
                let list = self.lists.entry(l.clone()).or_default();
                if i.fract() != 0.0 || i < 1.0 || i as usize > list.len() + 1 {
                    return Err(format!("ERR:INVALID DIM ({l} has {} elements)", list.len()));
                }
                let i = i as usize;
                if i == list.len() + 1 {
                    list.push(x);
                } else {
                    list[i - 1] = x;
                }
                Ok(())
            }
            Target::Dim(l) => {
                let Value::Num(n) = v else { return err("ERR:DATA TYPE") };
                if n < 0.0 || n > 999.0 || n.fract() != 0.0 {
                    return err("ERR:INVALID DIM");
                }
                self.used.insert(Var::List(l.clone()));
                self.lists.entry(l.clone()).or_default().resize(n as usize, 0.0);
                Ok(())
            }
        }
    }

    fn num(&mut self, e: &Expr) -> R<f64> {
        match self.eval(e)? {
            Value::Num(x) => Ok(x),
            _ => err("ERR:DATA TYPE (expected a number)"),
        }
    }

    fn list_arg(&mut self, e: &Expr) -> R<Vec<f64>> {
        match self.eval(e)? {
            Value::List(l) => Ok(l),
            _ => err("ERR:DATA TYPE (expected a list)"),
        }
    }

    fn str_arg(&mut self, e: &Expr) -> R<Vec<u16>> {
        match self.eval(e)? {
            Value::Str(s) => Ok(s),
            _ => err("ERR:DATA TYPE (expected a string)"),
        }
    }

    fn random(&mut self) -> f64 {
        // xorshift64*
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
    }

    fn eval(&mut self, e: &Expr) -> R<Value> {
        Ok(match e {
            Expr::Num(x) | Expr::Const(x) => Value::Num(*x),
            Expr::Str(s) => Value::Str(s.clone()),
            Expr::Var(v) => {
                self.used.insert(v.clone());
                match v {
                    Var::Real(i) => Value::Num(self.reals[*i as usize]),
                    Var::Str(n) => Value::Str(self.strs[*n as usize].clone().ok_or_else(|| format!("ERR:UNDEFINED ({v} has no value)"))?),
                    Var::List(l) => Value::List(self.lists.get(l).cloned().ok_or_else(|| format!("ERR:UNDEFINED ({l} doesn't exist)"))?),
                    Var::Ans => self.ans.clone(),
                }
            }
            Expr::ListLit(items) => {
                let mut v = Vec::new();
                for i in items {
                    v.push(self.num(i)?);
                }
                Value::List(v)
            }
            Expr::Neg(x) => map1(self.eval(x)?, |a| Ok(-a))?,
            Expr::Bin(op, l, r) => {
                let (a, bv) = (self.eval(l)?, self.eval(r)?);
                binop(*op, a, bv)?
            }
            Expr::Post(op, x) => {
                let v = self.eval(x)?;
                map1(v, |a| match op {
                    PostOp::Square => Ok(a * a),
                    PostOp::Cube => Ok(a * a * a),
                    PostOp::Inv => {
                        if a == 0.0 {
                            err("ERR:DIVIDE BY 0")
                        } else {
                            Ok(1.0 / a)
                        }
                    }
                    PostOp::Fact => {
                        if a < 0.0 || a.fract() != 0.0 || a > 69.0 {
                            err("ERR:DOMAIN (! needs a whole number 0–69)")
                        } else {
                            Ok((1..=a as u64).map(|i| i as f64).product())
                        }
                    }
                })?
            }
            Expr::Index(l, idx) => {
                self.used.insert(Var::List(l.clone()));
                let i = self.num(idx)?;
                let list = self.lists.get(l).ok_or_else(|| format!("ERR:UNDEFINED ({l} doesn't exist)"))?;
                if i.fract() != 0.0 || i < 1.0 || i as usize > list.len() {
                    return Err(format!("ERR:INVALID DIM ({l}({}) but {l} has {} elements)", fmt_num(i), list.len()));
                }
                Value::Num(list[i as usize - 1])
            }
            Expr::GetKey => Value::Num(std::mem::take(&mut self.key) as f64),
            Expr::Rand => Value::Num(self.random()),
            Expr::Call(code, args) => self.call(*code, args)?,
            Expr::Unsupported(c) => return Err(format!("{} isn't supported by the debugger", text(*c).trim())),
        })
    }

    fn call(&mut self, code: u16, args: &[Expr]) -> R<Value> {
        let name = text(code);
        let argc = |n: usize| -> R<()> {
            if args.len() == n {
                Ok(())
            } else {
                Err(format!("ERR:ARGUMENT ({name} takes {n} argument{})", if n == 1 { "" } else { "s" }))
            }
        };
        let unary = |m: &mut Machine, f: fn(f64) -> R<f64>| -> R<Value> {
            argc(1)?;
            let v = m.eval(&args[0])?;
            map1(v, f)
        };
        match code {
            tk::ABS => unary(self, |a| Ok(a.abs())),
            tk::INT => unary(self, |a| Ok(a.floor())),
            tk::IPART => unary(self, |a| Ok(a.trunc())),
            tk::FPART => unary(self, |a| Ok(a.fract())),
            tk::NOT => unary(self, |a| Ok(b(a == 0.0))),
            tk::SQRT => unary(self, |a| if a < 0.0 { err("ERR:NONREAL ANS") } else { Ok(a.sqrt()) }),
            tk::SIN => unary(self, |a| Ok(a.sin())),
            tk::COS => unary(self, |a| Ok(a.cos())),
            tk::TAN => unary(self, |a| Ok(a.tan())),
            tk::ASIN => unary(self, |a| if a.abs() > 1.0 { err("ERR:DOMAIN") } else { Ok(a.asin()) }),
            tk::ACOS => unary(self, |a| if a.abs() > 1.0 { err("ERR:DOMAIN") } else { Ok(a.acos()) }),
            tk::ATAN => unary(self, |a| Ok(a.atan())),
            tk::LN => unary(self, |a| if a <= 0.0 { err("ERR:NONREAL ANS") } else { Ok(a.ln()) }),
            tk::LOG => unary(self, |a| if a <= 0.0 { err("ERR:NONREAL ANS") } else { Ok(a.log10()) }),
            tk::EXP => unary(self, |a| Ok(a.exp())),
            tk::TENPOW => unary(self, |a| Ok(10f64.powf(a))),
            tk::ROUND => {
                let digits = match args.len() {
                    1 => 9.0,
                    2 => self.num(&args[1])?,
                    _ => return err("ERR:ARGUMENT (round( takes 1 or 2 arguments)"),
                };
                let m = 10f64.powi(digits as i32);
                let v = self.eval(&args[0])?;
                map1(v, |a| Ok((a * m).round() / m))
            }
            tk::MIN | tk::MAX => {
                let pick = |a: f64, c: f64| if code == tk::MIN { a.min(c) } else { a.max(c) };
                match args.len() {
                    1 => {
                        let l = self.list_arg(&args[0])?;
                        l.into_iter().reduce(pick).map(Value::Num).ok_or_else(|| "ERR:INVALID DIM (empty list)".to_string())
                    }
                    2 => {
                        let (x, y) = (self.eval(&args[0])?, self.eval(&args[1])?);
                        zip(x, y, |a, c| Ok(pick(a, c)))
                    }
                    _ => Err(format!("ERR:ARGUMENT ({name} takes 1 or 2 arguments)")),
                }
            }
            tk::RANDINT => {
                if !(2..=3).contains(&args.len()) {
                    return err("ERR:ARGUMENT (randInt( takes 2 or 3 arguments)");
                }
                let (lo, hi) = (self.num(&args[0])?, self.num(&args[1])?);
                let (lo, hi) = (lo.min(hi).ceil(), lo.max(hi).floor());
                let one = |m: &mut Machine| (lo + (m.random() * (hi - lo + 1.0)).floor()).min(hi);
                if args.len() == 3 {
                    let n = self.num(&args[2])?;
                    if n < 1.0 || n > 999.0 {
                        return err("ERR:DOMAIN");
                    }
                    Ok(Value::List((0..n as usize).map(|_| one(self)).collect()))
                } else {
                    Ok(Value::Num(one(self)))
                }
            }
            tk::DIM => {
                argc(1)?;
                Ok(Value::Num(self.list_arg(&args[0])?.len() as f64))
            }
            tk::SUM => {
                argc(1)?;
                Ok(Value::Num(self.list_arg(&args[0])?.iter().sum()))
            }
            tk::PROD => {
                argc(1)?;
                Ok(Value::Num(self.list_arg(&args[0])?.iter().product()))
            }
            tk::MEAN => {
                argc(1)?;
                let l = self.list_arg(&args[0])?;
                if l.is_empty() {
                    return err("ERR:INVALID DIM (empty list)");
                }
                Ok(Value::Num(l.iter().sum::<f64>() / l.len() as f64))
            }
            tk::REMAINDER | tk::GCD | tk::LCM => {
                argc(2)?;
                let (x, y) = (self.eval(&args[0])?, self.eval(&args[1])?);
                zip(x, y, |a, c| match code {
                    tk::REMAINDER if c == 0.0 => err("ERR:DIVIDE BY 0"),
                    tk::REMAINDER => Ok(a % c),
                    _ => {
                        if a.fract() != 0.0 || c.fract() != 0.0 {
                            return err("ERR:DOMAIN (whole numbers only)");
                        }
                        let (mut p, mut q) = (a.abs() as u64, c.abs() as u64);
                        while q != 0 {
                            (p, q) = (q, p % q);
                        }
                        if code == tk::GCD {
                            Ok(p as f64)
                        } else if p == 0 {
                            Ok(0.0)
                        } else {
                            Ok((a.abs() / p as f64) * c.abs())
                        }
                    }
                })
            }
            tk::LENGTH => {
                argc(1)?;
                Ok(Value::Num(self.str_arg(&args[0])?.len() as f64))
            }
            tk::SUB_STR => {
                argc(3)?;
                let s = self.str_arg(&args[0])?;
                let (start, len) = (self.num(&args[1])?, self.num(&args[2])?);
                if start < 1.0 || len < 0.0 || start.fract() != 0.0 || len.fract() != 0.0 || (start - 1.0 + len) as usize > s.len() {
                    return Err(format!("ERR:DOMAIN (sub( of a {}-character string from {} for {})", s.len(), fmt_num(start), fmt_num(len)));
                }
                let st = start as usize - 1;
                Ok(Value::Str(s[st..st + len as usize].to_vec()))
            }
            tk::INSTRING => {
                if !(2..=3).contains(&args.len()) {
                    return err("ERR:ARGUMENT (inString( takes 2 or 3 arguments)");
                }
                let hay = self.str_arg(&args[0])?;
                let needle = self.str_arg(&args[1])?;
                let start = if args.len() == 3 { self.num(&args[2])? as usize } else { 1 };
                let pos = (start.max(1) - 1..hay.len())
                    .find(|&i| !needle.is_empty() && hay[i..].starts_with(&needle))
                    .map_or(0.0, |i| (i + 1) as f64);
                Ok(Value::Num(pos))
            }
            tk::TOSTRING => {
                argc(1)?;
                let v = self.eval(&args[0])?;
                let shown = show(&v);
                let toks = tokens::tokenize(&shown).map(|b| codes(&b)).unwrap_or_default();
                Ok(Value::Str(toks))
            }
            _ => Err(format!("{} isn't supported by the debugger", name.trim())),
        }
    }
}

fn map1(v: Value, f: impl Fn(f64) -> R<f64>) -> R<Value> {
    match v {
        Value::Num(a) => f(a).map(Value::Num),
        Value::List(l) => l.into_iter().map(f).collect::<R<Vec<f64>>>().map(Value::List),
        Value::Str(_) => err("ERR:DATA TYPE (a string can't be used here)"),
    }
}

fn zip(a: Value, c: Value, f: impl Fn(f64, f64) -> R<f64>) -> R<Value> {
    match (a, c) {
        (Value::Num(x), Value::Num(y)) => f(x, y).map(Value::Num),
        (Value::List(l), Value::Num(y)) => l.into_iter().map(|x| f(x, y)).collect::<R<_>>().map(Value::List),
        (Value::Num(x), Value::List(l)) => l.into_iter().map(|y| f(x, y)).collect::<R<_>>().map(Value::List),
        (Value::List(l), Value::List(m)) => {
            if l.len() != m.len() {
                return Err(format!("ERR:DIM MISMATCH ({} vs {} elements)", l.len(), m.len()));
            }
            l.into_iter().zip(m).map(|(x, y)| f(x, y)).collect::<R<_>>().map(Value::List)
        }
        _ => err("ERR:DATA TYPE (a string can't be used here)"),
    }
}

fn binop(op: BinOp, a: Value, c: Value) -> R<Value> {
    if let (Value::Str(x), Value::Str(y)) = (&a, &c) {
        return match op {
            BinOp::Add => Ok(Value::Str([x.as_slice(), y.as_slice()].concat())),
            BinOp::Eq => Ok(Value::Num(b(x == y))),
            BinOp::Ne => Ok(Value::Num(b(x != y))),
            _ => err("ERR:DATA TYPE (strings only support + = ≠)"),
        };
    }
    zip(a, c, |x, y| {
        Ok(match op {
            BinOp::Add => x + y,
            BinOp::Sub => x - y,
            BinOp::Mul => x * y,
            BinOp::Div => {
                if y == 0.0 {
                    return err("ERR:DIVIDE BY 0");
                }
                x / y
            }
            BinOp::Pow => {
                if x < 0.0 && y.fract() != 0.0 {
                    return err("ERR:NONREAL ANS");
                }
                if x == 0.0 && y < 0.0 {
                    return err("ERR:DIVIDE BY 0");
                }
                x.powf(y)
            }
            BinOp::Eq => b(same(x, y)),
            BinOp::Ne => b(!same(x, y)),
            BinOp::Lt => b(x < y && !same(x, y)),
            BinOp::Gt => b(x > y && !same(x, y)),
            BinOp::Le => b(x < y || same(x, y)),
            BinOp::Ge => b(x > y || same(x, y)),
            BinOp::And => b(truthy(x) && truthy(y)),
            BinOp::Or => b(truthy(x) || truthy(y)),
            BinOp::Xor => b(truthy(x) != truthy(y)),
        })
    })
}

/// TI key codes for getKey from desktop keys (by name, as egui reports them).
pub fn key_code(name: &str) -> Option<u16> {
    Some(match name {
        "ArrowLeft" => 24,
        "ArrowUp" => 25,
        "ArrowRight" => 26,
        "ArrowDown" => 34,
        "Enter" => 105,
        "Escape" | "Backspace" => 45, // CLEAR
        "Delete" => 23,
        "+" => 95,
        "-" => 85,
        "*" => 65,
        "/" => 55,
        "^" => 14,
        "0" => 102,
        "1" => 92,
        "2" => 93,
        "3" => 94,
        "4" => 82,
        "5" => 83,
        "6" => 84,
        "7" => 72,
        "8" => 73,
        "9" => 74,
        "." => 103,
        " " => 102, // alpha-space shares the 0 key
        _ => {
            // letters: the ALPHA labels on the keypad
            let c = name.chars().next().filter(|c| name.len() == 1 && c.is_ascii_alphabetic())?.to_ascii_uppercase();
            const KEYS: [u16; 26] = [41, 42, 43, 51, 52, 53, 54, 55, 61, 62, 63, 64, 65, 71, 72, 73, 74, 75, 81, 82, 83, 84, 85, 91, 92, 93];
            KEYS[(c as u8 - b'A') as usize]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> Machine {
        let mut m = Machine::new(src, 16, 8).unwrap();
        m.resume();
        m.run_for(Duration::from_secs(2));
        m
    }

    fn screen(m: &Machine) -> Vec<String> {
        m.screen.cells.iter().map(|r| r.iter().collect::<String>().trim_end().to_string()).filter(|s| !s.is_empty()).collect()
    }

    #[test]
    fn numbers_format_like_the_calculator() {
        assert_eq!(fmt_num(3.0), "3");
        assert_eq!(fmt_num(-12.5), "-12.5");
        assert_eq!(fmt_num(1.0 / 3.0), ".3333333333");
        assert_eq!(fmt_num(2f64.sqrt()), "1.414213562");
        assert_eq!(fmt_num(1e12), "1ᴇ12");
        assert_eq!(fmt_num(0.0001234), "1.234ᴇ-4");
    }

    #[test]
    fn loops_and_branches() {
        let m = run("ClrHome\n0→S\nFor(I,1,10)\nIf remainder(I,2)=0\nThen\nS+I→S\nElse\nS-1→S\nEnd\nEnd\nDisp S\n1→N\nWhile N<100\n2N→N\nEnd\nDisp N\nRepeat K>3\nK+1→K\nEnd\nDisp K");
        assert_eq!(m.state, State::Done);
        // even sum 30, minus 5 odd → 25
        assert_eq!(screen(&m), vec!["              25", "             128", "               4"]);
    }

    #[test]
    fn strings_lists_and_goto() {
        let m = run("\"HELLO\"→Str1\nDisp sub(Str1,2,3)+\"!\",length(Str1)\n{3,1,2}→L₁\n2L₁→L₂\nDisp L₂,sum(L₂)\n5→L₁(4)\nDisp dim(L₁)\nGoto Z\nDisp \"SKIPPED\"\nLbl Z\nDisp \"END\"");
        assert_eq!(m.state, State::Done, "{:?}", m.state);
        assert_eq!(screen(&m), vec!["ELL!", "               5", "         {6 2 4}", "              12", "               4", "END"]);
    }

    #[test]
    fn errors_point_at_their_line() {
        let m = run("Disp 1\nDisp 1/0");
        assert_eq!(m.state, State::Error { line: 2, msg: "ERR:DIVIDE BY 0".into() });
        let m = run("Goto Q");
        assert!(matches!(m.state, State::Error { line: 1, ref msg } if msg.starts_with("ERR:LABEL")));
    }

    #[test]
    fn breakpoints_and_stepping() {
        let mut m = Machine::new("1→A\n2→A\n3→A\n4→A", 16, 8).unwrap();
        m.breakpoints.insert(3);
        m.resume();
        m.run_for(Duration::from_secs(1));
        assert_eq!((m.state.clone(), m.current_line()), (State::Paused, Some(3)));
        assert_eq!(m.variables()[1], ("A".to_string(), "2".to_string()));
        m.step();
        assert_eq!(m.current_line(), Some(4));
        m.resume();
        m.run_for(Duration::from_secs(1));
        assert_eq!(m.state, State::Done);
        assert_eq!(m.variables()[1].1, "4");
    }

    #[test]
    fn input_prompt_pause_menu_getkey() {
        let mut m = run("Prompt A,B\nInput \"NAME?\",Str1\nDisp A+B,Str1\nPause\nMenu(\"GO\",\"ONE\",1,\"TWO\",2)\nLbl 1\nDisp \"ONE\"\nStop\nLbl 2\ngetKey→K\nDisp K");
        assert!(matches!(m.state, State::Input { ref prompt, .. } if prompt == "A=?"));
        m.give_input("2").unwrap();
        assert!(matches!(m.state, State::Input { ref prompt, .. } if prompt == "B=?"));
        assert!(m.give_input("3+").is_err()); // bad entry: still waiting
        m.give_input("⁻5").unwrap();
        m.run_for(Duration::from_secs(1));
        assert!(matches!(m.state, State::Input { ref prompt, .. } if prompt == "NAME?"));
        m.give_input("BOB").unwrap();
        m.run_for(Duration::from_secs(1));
        assert_eq!(m.state, State::PauseCmd);
        m.enter();
        m.run_for(Duration::from_secs(1));
        assert!(matches!(m.state, State::Menu { ref items, .. } if items.len() == 2));
        m.press_key(105);
        m.choose(1);
        m.run_for(Duration::from_secs(1));
        assert_eq!(m.state, State::Done);
        let s = screen(&m);
        assert!(s.contains(&"              -3".to_string()) && s.contains(&"BOB".to_string()), "{s:?}");
        assert_eq!(s.last().unwrap().trim(), "105");
    }

    #[test]
    fn programs_call_programs() {
        let mut m = Machine::new("Disp \"MAIN\"\nprgmSUB\nDisp \"BACK\",A", 16, 8).unwrap();
        m.programs.insert("SUB".into(), "5→A\nDisp \"SUB\"\nReturn\nDisp \"NO\"".into());
        m.resume();
        m.run_for(Duration::from_secs(1));
        assert_eq!(m.state, State::Done, "{:?}", m.state);
        assert_eq!(screen(&m), vec!["MAIN", "SUB", "BACK", "               5"]);
    }

    #[test]
    fn home_entries_share_variables() {
        let mut m = Machine::new("", 26, 10).unwrap();
        let run = |m: &mut Machine, s: &str| {
            m.run_codes(&codes(&tokens::tokenize(s).unwrap()));
            m.run_for(Duration::from_secs(1));
        };
        run(&mut m, "3→A");
        assert!(m.last_was_value);
        run(&mut m, "A²+Ans");
        assert_eq!(m.ans(), &Value::Num(12.0));
        run(&mut m, "ClrHome");
        assert!(!m.last_was_value);
        let e = match super::super::parse::parse_stmt(&codes(&tokens::tokenize("X²-1").unwrap())).unwrap() {
            Stmt::Expr(e) => e,
            _ => unreachable!(),
        };
        assert_eq!(m.eval_at_x(&e, 3.0), Some(8.0));
    }

    #[test]
    fn skips_graphics() {
        let m = run("ClrDraw\nLine(0,0,1,1)\nDisp 7");
        assert_eq!(m.state, State::Done);
        assert!(m.skipped.contains("Line(") && m.skipped.contains("ClrDraw"));
    }

    #[test]
    fn output_wraps_and_checks_bounds() {
        let m = run("Output(1,15,\"ABCD\")");
        assert_eq!(screen(&m), vec!["              AB", "CD"]);
        let m = run("Output(9,1,1)");
        assert!(matches!(m.state, State::Error { .. }));
    }
}
