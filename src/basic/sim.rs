//! A TI-84 Plus CE-style calculator built on CalLink's TI-BASIC interpreter.
//!
//! It is not an emulator of TI's hardware or OS: it reproduces the keypad (with each key's
//! real getKey code and its 2nd/alpha functions), the home screen with entry history and Ans,
//! running programs (`prgmNAME`, Input/Prompt/Pause/Menu(/getKey, ON to break), the PRGM,
//! MATH, TEST and CATALOG menus, the Y= editor and a function graph. Drawing is left to the UI.

use std::time::Duration;

use super::interp::{show, Machine, State};
use super::parse::{parse_stmt, Expr, Stmt};
use super::{codes, texts};
use crate::tokens::{self, TokenTable};

/// What a key does in a given shift state.
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    /// Insert this token text
    Tok(&'static str),
    Enter,
    Clear,
    Del,
    Ins,
    Left,
    Right,
    Up,
    Down,
    Second,
    Alpha,
    AlphaLock,
    Quit,
    Entry,
    Menu(MenuKind),
    Catalog,
    YEq,
    Graph,
    Zoom,
    On,
    Nothing,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MenuKind {
    Math,
    Test,
    Prgm,
}

#[derive(Clone, Debug)]
pub struct KeyDef {
    /// getKey code (row × 10 + column on the keypad)
    pub code: u16,
    pub label: &'static str,
    pub second_label: &'static str,
    pub alpha_label: &'static str,
    pub normal: Act,
    pub second: Act,
    pub alpha: Act,
}

fn k(code: u16, label: &'static str, second_label: &'static str, alpha_label: &'static str, normal: Act, second: Act, alpha: Act) -> KeyDef {
    KeyDef { code, label, second_label, alpha_label, normal, second, alpha }
}

/// The keypad below the arrow keys, row by row, as on a TI-84 Plus CE.
/// (The arrow keys are [`ARROWS`]; the top row is the graphing keys.)
pub fn keypad() -> Vec<Vec<KeyDef>> {
    use Act::*;
    vec![
        vec![
            k(11, "y=", "stat plot", "f1", YEq, Nothing, Nothing),
            k(12, "window", "tblset", "f2", Nothing, Nothing, Nothing),
            k(13, "zoom", "format", "f3", Zoom, Nothing, Nothing),
            k(14, "trace", "calc", "f4", Nothing, Nothing, Nothing),
            k(15, "graph", "table", "f5", Graph, Nothing, Nothing),
        ],
        vec![
            k(21, "2nd", "", "", Second, Second, Second),
            k(22, "mode", "quit", "", Nothing, Quit, Quit),
            k(23, "del", "ins", "", Del, Ins, Del),
        ],
        vec![
            k(31, "alpha", "A-lock", "", Alpha, AlphaLock, Alpha),
            k(32, "X,T,θ,n", "link", "", Tok("X"), Nothing, Tok("X")),
            k(33, "stat", "list", "", Nothing, Nothing, Nothing),
        ],
        vec![
            k(41, "math", "test", "A", Menu(MenuKind::Math), Menu(MenuKind::Test), Tok("A")),
            k(42, "apps", "angle", "B", Nothing, Nothing, Tok("B")),
            k(43, "prgm", "draw", "C", Menu(MenuKind::Prgm), Nothing, Tok("C")),
            k(44, "vars", "distr", "", Nothing, Nothing, Nothing),
            k(45, "clear", "", "", Clear, Clear, Clear),
        ],
        vec![
            k(51, "x⁻¹", "matrix", "D", Tok("⁻¹"), Nothing, Tok("D")),
            k(52, "sin", "sin⁻¹", "E", Tok("sin("), Tok("sin⁻¹("), Tok("E")),
            k(53, "cos", "cos⁻¹", "F", Tok("cos("), Tok("cos⁻¹("), Tok("F")),
            k(54, "tan", "tan⁻¹", "G", Tok("tan("), Tok("tan⁻¹("), Tok("G")),
            k(55, "^", "π", "H", Tok("^"), Tok("π"), Tok("H")),
        ],
        vec![
            k(61, "x²", "√", "I", Tok("²"), Tok("√("), Tok("I")),
            k(62, ",", "EE", "J", Tok(","), Tok("ᴇ"), Tok("J")),
            k(63, "(", "{", "K", Tok("("), Tok("{"), Tok("K")),
            k(64, ")", "}", "L", Tok(")"), Tok("}"), Tok("L")),
            k(65, "÷", "e", "M", Tok("/"), Tok("ℯ"), Tok("M")),
        ],
        vec![
            k(71, "log", "10ˣ", "N", Tok("log("), Tok("₁₀^("), Tok("N")),
            k(72, "7", "u", "O", Tok("7"), Nothing, Tok("O")),
            k(73, "8", "v", "P", Tok("8"), Nothing, Tok("P")),
            k(74, "9", "w", "Q", Tok("9"), Nothing, Tok("Q")),
            k(75, "×", "[", "R", Tok("*"), Tok("["), Tok("R")),
        ],
        vec![
            k(81, "ln", "eˣ", "S", Tok("ln("), Tok("ℯ^("), Tok("S")),
            k(82, "4", "L4", "T", Tok("4"), Tok("L₄"), Tok("T")),
            k(83, "5", "L5", "U", Tok("5"), Tok("L₅"), Tok("U")),
            k(84, "6", "L6", "V", Tok("6"), Tok("L₆"), Tok("V")),
            k(85, "−", "]", "W", Tok("-"), Tok("]"), Tok("W")),
        ],
        vec![
            k(91, "sto→", "rcl", "X", Tok("→"), Nothing, Tok("X")),
            k(92, "1", "L1", "Y", Tok("1"), Tok("L₁"), Tok("Y")),
            k(93, "2", "L2", "Z", Tok("2"), Tok("L₂"), Tok("Z")),
            k(94, "3", "L3", "θ", Tok("3"), Tok("L₃"), Tok("θ")),
            k(95, "+", "mem", "\"", Tok("+"), Nothing, Tok("\"")),
        ],
        vec![
            k(101, "on", "off", "", On, On, On),
            k(102, "0", "catalog", "␣", Tok("0"), Catalog, Tok(" ")),
            k(103, ".", "i", ":", Tok("."), Tok("𝑖"), Tok(":")),
            k(104, "(−)", "ans", "?", Tok("⁻"), Tok("Ans"), Tok("?")),
            k(105, "enter", "entry", "solve", Enter, Entry, Enter),
        ],
    ]
}

/// Arrow keys: (code, label, action).
pub const ARROWS: [(u16, &str, Act); 4] = [(25, "▲", Act::Up), (24, "◀", Act::Left), (26, "▶", Act::Right), (34, "▼", Act::Down)];

fn find_key(code: u16) -> Option<KeyDef> {
    if let Some((c, l, a)) = ARROWS.iter().find(|(c, _, _)| *c == code) {
        return Some(KeyDef { code: *c, label: l, second_label: "", alpha_label: "", normal: a.clone(), second: a.clone(), alpha: a.clone() });
    }
    keypad().into_iter().flatten().find(|k| k.code == code)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Insert(String),
    Program(String),
}

#[derive(Clone, Debug)]
pub struct MenuState {
    pub tabs: Vec<(String, Vec<(String, Item)>)>,
    pub tab: usize,
    pub sel: usize,
    /// For the catalog: typing letters jumps to them
    pub catalog: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum View {
    Home,
    Menu,
    YEdit { row: usize },
    Graph,
    Error { msg: String, line: usize, program: Option<String> },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    pub xmin: f64,
    pub xmax: f64,
    pub ymin: f64,
    pub ymax: f64,
}

impl Default for Window {
    fn default() -> Self {
        Window { xmin: -10.0, xmax: 10.0, ymin: -10.0, ymax: 10.0 }
    }
}

pub struct Sim {
    pub m: Machine,
    pub view: View,
    pub menu: Option<MenuState>,
    /// View to return to when a menu closes.
    menu_return: View,
    pub input: Vec<u16>,
    pub cursor: usize,
    pub second: bool,
    pub alpha: bool,
    pub alpha_lock: bool,
    last_entry: Vec<u16>,
    pub y: [Vec<u16>; 6],
    pub window: Window,
    /// A program (or home entry) is running.
    pub running: Option<String>,
    /// Selected item in a program's Menu( screen.
    pub prog_menu_sel: usize,
    /// "Goto" was chosen on an error screen: (program, line) for the UI to open.
    pub goto_request: Option<(String, usize)>,
}

const HOME_COLS: usize = 26;
const HOME_ROWS: usize = 10;

fn toks(text: &str) -> Vec<u16> {
    tokens::tokenize(text).map(|b| codes(&b)).unwrap_or_default()
}

impl Default for Sim {
    fn default() -> Self {
        Self::new()
    }
}

impl Sim {
    pub fn new() -> Sim {
        Sim {
            m: Machine::new("", HOME_COLS, HOME_ROWS).expect("empty program"),
            view: View::Home,
            menu: None,
            menu_return: View::Home,
            input: Vec::new(),
            cursor: 0,
            second: false,
            alpha: false,
            alpha_lock: false,
            last_entry: Vec::new(),
            y: Default::default(),
            window: Window::default(),
            running: None,
            prog_menu_sel: 0,
            goto_request: None,
        }
    }

    /// Make a program available to `prgm` (and the PRGM menu).
    pub fn add_program(&mut self, name: &str, text: &str) {
        self.m.programs.insert(name.to_string(), text.to_string());
    }

    pub fn status(&self) -> String {
        let shift = if self.second {
            "2nd"
        } else if self.alpha_lock {
            "A-LOCK"
        } else if self.alpha {
            "ALPHA"
        } else {
            ""
        };
        let busy = if self.running.is_some() && self.m.state == State::Running { "BUSY " } else { "" };
        format!("NORMAL FLOAT AUTO REAL RADIAN MP  {busy}{shift}")
    }

    fn waiting_for_input(&self) -> bool {
        matches!(self.m.state, State::Input { .. })
    }

    /// Press a key by its getKey code.
    pub fn press(&mut self, code: u16) {
        let Some(key) = find_key(code) else { return };
        let act = if self.second {
            self.second = false;
            key.second.clone()
        } else if self.alpha || self.alpha_lock {
            if !self.alpha_lock {
                self.alpha = false;
            }
            key.alpha.clone()
        } else {
            key.normal.clone()
        };
        if act == Act::On {
            self.m.break_program();
            self.after_program();
            return;
        }
        // While a program runs, keys go to getKey (and Enter/typing to Input, Pause, Menu().
        if self.running.is_some() {
            match self.m.state.clone() {
                State::Running => {
                    self.m.press_key(code);
                    return;
                }
                State::PauseCmd if act == Act::Enter => {
                    self.m.enter();
                    return;
                }
                State::Menu { items, .. } => {
                    match act {
                        Act::Up => self.prog_menu_sel = self.prog_menu_sel.saturating_sub(1),
                        Act::Down => self.prog_menu_sel = (self.prog_menu_sel + 1).min(items.len().saturating_sub(1)),
                        Act::Enter => {
                            let i = self.prog_menu_sel;
                            self.prog_menu_sel = 0;
                            self.m.choose(i);
                        }
                        Act::Tok(t) if t.len() == 1 && t.as_bytes()[0].is_ascii_digit() => {
                            let n = (t.as_bytes()[0] - b'0') as usize;
                            let i = if n == 0 { 9 } else { n - 1 };
                            if i < items.len() {
                                self.prog_menu_sel = 0;
                                self.m.choose(i);
                            }
                        }
                        _ => {}
                    }
                    return;
                }
                State::Input { .. } => {} // edit the input line below
                _ => return,
            }
        }
        match self.view.clone() {
            View::Menu => self.menu_key(act, &key),
            View::Error { line, program, .. } => match act {
                Act::Tok("1") | Act::Quit | Act::Clear | Act::Enter => self.view = View::Home,
                Act::Tok("2") => {
                    if let Some(p) = program {
                        self.goto_request = Some((p, line));
                    }
                    self.view = View::Home;
                }
                _ => {}
            },
            View::Graph => match act {
                Act::Clear | Act::Quit => self.view = View::Home,
                Act::YEq => self.view = View::YEdit { row: 0 },
                Act::Zoom => self.window = Window::default(),
                Act::Left | Act::Right | Act::Up | Act::Down => {
                    let (dx, dy) = ((self.window.xmax - self.window.xmin) / 4.0, (self.window.ymax - self.window.ymin) / 4.0);
                    let (sx, sy) = match act {
                        Act::Left => (-dx, 0.0),
                        Act::Right => (dx, 0.0),
                        Act::Up => (0.0, dy),
                        _ => (0.0, -dy),
                    };
                    self.window.xmin += sx;
                    self.window.xmax += sx;
                    self.window.ymin += sy;
                    self.window.ymax += sy;
                }
                Act::Menu(k) => self.open_menu(k),
                _ => {}
            },
            View::YEdit { row } => self.yedit_key(act, row),
            View::Home => self.home_key(act),
        }
    }

    // ----- editing -----

    fn line_mut(&mut self) -> &mut Vec<u16> {
        match self.view {
            View::YEdit { row } => &mut self.y[row],
            _ => &mut self.input,
        }
    }

    fn insert_text(&mut self, t: &str) {
        let ins = toks(t);
        let at = self.cursor.min(self.line_mut().len());
        let n = ins.len();
        self.line_mut().splice(at..at, ins);
        self.cursor = at + n;
    }

    fn edit_key(&mut self, act: &Act) -> bool {
        match act {
            Act::Tok(t) => self.insert_text(t),
            Act::Left => self.cursor = self.cursor.saturating_sub(1),
            Act::Right => self.cursor = (self.cursor + 1).min(self.line_mut().len()),
            Act::Del => {
                let len = self.line_mut().len();
                if self.cursor < len {
                    let c = self.cursor;
                    self.line_mut().remove(c);
                } else if len > 0 {
                    self.line_mut().pop();
                    self.cursor = len - 1;
                }
            }
            Act::Second => self.second = true,
            Act::Alpha => {
                if self.alpha_lock {
                    self.alpha_lock = false;
                } else {
                    self.alpha = !self.alpha;
                }
            }
            Act::AlphaLock => self.alpha_lock = true,
            Act::Menu(k) => self.open_menu(*k),
            Act::Catalog => self.open_catalog(),
            _ => return false,
        }
        true
    }

    fn home_key(&mut self, act: Act) {
        if self.edit_key(&act) {
            return;
        }
        match act {
            Act::Enter => self.enter(),
            Act::Clear => {
                if self.input.is_empty() && !self.waiting_for_input() {
                    self.m.screen.clear();
                } else {
                    self.input.clear();
                    self.cursor = 0;
                }
            }
            Act::Entry => {
                self.input = self.last_entry.clone();
                self.cursor = self.input.len();
            }
            Act::YEq => self.view = View::YEdit { row: 0 },
            Act::Graph => self.view = View::Graph,
            _ => {}
        }
    }

    fn yedit_key(&mut self, act: Act, row: usize) {
        if self.edit_key(&act) {
            return;
        }
        let goto = |s: &mut Sim, r: usize| {
            s.view = View::YEdit { row: r };
            s.cursor = s.y[r].len();
        };
        match act {
            Act::Up if row > 0 => goto(self, row - 1),
            Act::Down | Act::Enter if row < 5 => goto(self, row + 1),
            Act::Clear => {
                self.y[row].clear();
                self.cursor = 0;
            }
            Act::Graph => self.view = View::Graph,
            Act::Quit => self.view = View::Home,
            _ => {}
        }
    }

    /// Run what's on the entry line (or answer Input).
    fn enter(&mut self) {
        if self.waiting_for_input() {
            let text = texts(&self.input);
            if let Err(e) = self.m.give_input(&text) {
                self.m.screen.line(&e, false);
            }
            self.input.clear();
            self.cursor = 0;
            return;
        }
        if self.input.is_empty() {
            if self.last_entry.is_empty() {
                return;
            }
            self.input = self.last_entry.clone();
        }
        let entry = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.m.screen.line(&texts(&entry), false);
        self.last_entry = entry.clone();
        let name = if entry.first() == Some(&super::tk::PRGM) { Some(texts(&entry[1..]).trim().to_string()) } else { None };
        self.running = Some(name.unwrap_or_default());
        self.m.run_codes(&entry);
        self.tick(Duration::from_millis(50));
    }

    /// Advance a running program; call regularly.
    pub fn tick(&mut self, budget: Duration) {
        if self.running.is_none() {
            return;
        }
        if self.m.state == State::Running {
            self.m.run_for(budget);
        }
        if matches!(self.m.state, State::Done | State::Error { .. }) {
            self.after_program();
        }
    }

    fn after_program(&mut self) {
        let Some(name) = self.running.take() else { return };
        match self.m.state.clone() {
            State::Done => {
                if name.is_empty() && self.m.last_was_value {
                    let ans = show(self.m.ans());
                    self.m.screen.line(&ans, true);
                } else {
                    self.m.screen.line("Done", true);
                }
            }
            State::Error { msg, line } => {
                self.view = View::Error { msg, line, program: (!name.is_empty()).then_some(name) };
            }
            _ => {}
        }
        self.prog_menu_sel = 0;
    }

    // ----- menus -----

    fn open_menu(&mut self, kind: MenuKind) {
        let ins = |list: &[&str]| list.iter().map(|t| (t.trim_end().to_string(), Item::Insert(t.to_string()))).collect::<Vec<_>>();
        let tabs = match kind {
            MenuKind::Math => vec![
                ("MATH".to_string(), ins(&["³", "√(", "!", "abs(", "round(", "int("])),
                ("NUM".to_string(), ins(&["abs(", "round(", "iPart(", "fPart(", "int(", "min(", "max(", "lcm(", "gcd(", "remainder("])),
                ("PROB".to_string(), ins(&["rand", "randInt(", "!"])),
            ],
            MenuKind::Test => vec![
                ("TEST".to_string(), ins(&["=", "≠", ">", "≥", "<", "≤"])),
                ("LOGIC".to_string(), ins(&[" and ", " or ", " xor ", "not("])),
            ],
            MenuKind::Prgm => vec![
                ("EXEC".to_string(), self.m.programs.keys().map(|n| (n.clone(), Item::Program(n.clone()))).collect()),
                (
                    "CTL".to_string(),
                    ins(&["If ", "Then", "Else", "For(", "While ", "Repeat ", "End", "Pause ", "Lbl ", "Goto ", "IS>(", "DS<(", "Menu(", "prgm", "Return", "Stop", "DelVar "]),
                ),
                ("I/O".to_string(), ins(&["Input ", "Prompt ", "Disp ", "ClrHome", "Output(", "getKey"])),
            ],
        };
        self.menu = Some(MenuState { tabs, tab: 0, sel: 0, catalog: false });
        if self.view != View::Menu {
            self.menu_return = self.view.clone();
        }
        self.view = View::Menu;
    }

    fn open_catalog(&mut self) {
        let mut items: Vec<(String, Item)> = TokenTable::get()
            .all()
            .into_iter()
            .filter(|(c, t)| *c != 0x3F && t.trim().chars().count() > 1)
            .map(|(_, t)| (t.trim_end().to_string(), Item::Insert(t.to_string())))
            .collect();
        items.sort_by_key(|(l, _)| (!l.starts_with(|c: char| c.is_alphabetic()), l.to_lowercase()));
        self.menu = Some(MenuState { tabs: vec![("CATALOG".into(), items)], tab: 0, sel: 0, catalog: true });
        if self.view != View::Menu {
            self.menu_return = self.view.clone();
        }
        self.view = View::Menu;
    }

    fn close_menu(&mut self) {
        self.menu = None;
        self.view = self.menu_return.clone();
    }

    fn menu_key(&mut self, act: Act, key: &KeyDef) {
        let Some(menu) = &mut self.menu else {
            self.view = View::Home;
            return;
        };
        let count = menu.tabs[menu.tab].1.len();
        let mut choose = None;
        match &act {
            Act::Left if menu.tabs.len() > 1 => {
                menu.tab = (menu.tab + menu.tabs.len() - 1) % menu.tabs.len();
                menu.sel = 0;
            }
            Act::Right if menu.tabs.len() > 1 => {
                menu.tab = (menu.tab + 1) % menu.tabs.len();
                menu.sel = 0;
            }
            Act::Up if count > 0 => menu.sel = (menu.sel + count - 1) % count,
            Act::Down if count > 0 => menu.sel = (menu.sel + 1) % count,
            Act::Enter if count > 0 => choose = Some(menu.sel),
            Act::Clear | Act::Quit => {
                self.close_menu();
                return;
            }
            _ if menu.catalog => {
                // the catalog is in alpha mode: letters jump
                let letter = key.alpha_label;
                if letter.len() == 1 && letter.as_bytes()[0].is_ascii_uppercase() {
                    let l = letter.to_ascii_lowercase();
                    if let Some(i) = menu.tabs[0].1.iter().position(|(t, _)| t.to_lowercase().starts_with(&l)) {
                        menu.sel = i;
                    }
                }
            }
            Act::Tok(t) if t.len() == 1 && t.as_bytes()[0].is_ascii_digit() => {
                let n = (t.as_bytes()[0] - b'0') as usize;
                let i = if n == 0 { 9 } else { n - 1 };
                if i < count {
                    choose = Some(i);
                }
            }
            _ => {}
        }
        if let Some(i) = choose {
            let item = menu.tabs[menu.tab].1[i].1.clone();
            self.close_menu();
            match item {
                Item::Insert(t) => self.insert_text(&t),
                Item::Program(name) => self.insert_text(&format!("prgm{name}")),
            }
        }
    }

    // ----- graph -----

    /// Parsed Y= functions (index, expression) that are not empty and parse.
    pub fn functions(&self) -> Vec<(usize, Expr)> {
        self.y
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.is_empty())
            .filter_map(|(i, t)| match parse_stmt(t) {
                Ok(Stmt::Expr(e)) => Some((i, e)),
                _ => None,
            })
            .collect()
    }

    /// Y values of a function at the given X values (None where undefined).
    pub fn sample(&mut self, e: &Expr, xs: &[f64]) -> Vec<Option<f64>> {
        xs.iter().map(|&x| self.m.eval_at_x(e, x)).collect()
    }

    /// Rows of the home screen, with the entry line shown at the cursor row.
    pub fn home_rows(&self) -> (Vec<String>, Option<(usize, usize)>) {
        let sc = &self.m.screen;
        let mut rows: Vec<String> = sc.cells.iter().map(|r| r.iter().collect()).collect();
        let accepting = self.running.is_none() || self.waiting_for_input();
        if !accepting {
            return (rows, None);
        }
        let mut r = sc.next_row();
        if r >= sc.rows {
            rows.remove(0);
            rows.push(String::new());
            r = sc.rows - 1;
        }
        let before = texts(&self.input[..self.cursor.min(self.input.len())]);
        let all = texts(&self.input);
        let col = before.chars().count();
        // keep the cursor on screen for long entries
        let skip = col.saturating_sub(sc.cols - 1);
        let shown: String = all.chars().skip(skip).take(sc.cols).collect();
        rows[r] = format!("{shown:<w$}", w = sc.cols);
        (rows, Some((r, col - skip)))
    }

    /// Text of each Y= row and the cursor column in the selected one.
    pub fn y_rows(&self) -> Vec<String> {
        self.y.iter().map(|t| texts(t)).collect()
    }

    pub fn cursor_col(&self) -> usize {
        let line = match self.view {
            View::YEdit { row } => &self.y[row],
            _ => &self.input,
        };
        texts(&line[..self.cursor.min(line.len())]).chars().count()
    }
}

/// getKey code for a desktop key press, for driving the simulator from the keyboard.
pub fn desktop_key(name: &str, shift: bool) -> Option<Vec<u16>> {
    // printable characters map to the keys that type them
    let one = |c: u16| Some(vec![c]);
    let alpha = |c: u16| Some(vec![31, c]);
    let second = |c: u16| Some(vec![21, c]);
    match (name, shift) {
        ("Enter", _) => one(105),
        ("Backspace" | "Delete", _) => one(23),
        ("Escape", _) => one(45),
        ("ArrowUp", _) => one(25),
        ("ArrowDown", _) => one(34),
        ("ArrowLeft", _) => one(24),
        ("ArrowRight", _) => one(26),
        ("+", _) | ("=", true) => one(95),
        ("-", false) | ("Minus", false) => one(85),
        ("*", _) | ("8", true) => one(75),
        ("/", _) | ("Slash", false) => one(65),
        ("^", _) | ("6", true) => one(55),
        ("(", _) | ("9", true) => one(63),
        (")", _) | ("0", true) => one(64),
        (",", _) | ("Comma", false) => one(62),
        (".", _) | ("Period", false) => one(103),
        ("Space", _) => alpha(102),
        ("\"", _) | ("Quote", true) => alpha(95),
        (":", _) | ("Semicolon", true) => alpha(103),
        ("?", _) | ("Slash", true) => alpha(104),
        ("{", _) | ("OpenBracket", true) => second(63),
        ("}", _) | ("CloseBracket", true) => second(64),
        (n, false) if n.len() == 1 && n.as_bytes()[0].is_ascii_digit() => {
            let d = n.as_bytes()[0] - b'0';
            const DIGITS: [u16; 10] = [102, 92, 93, 94, 82, 83, 84, 72, 73, 74];
            one(DIGITS[d as usize])
        }
        (n, _) if n.len() == 1 && n.as_bytes()[0].is_ascii_alphabetic() => {
            let i = (n.as_bytes()[0].to_ascii_uppercase() - b'A') as usize;
            const LETTERS: [u16; 26] = [41, 42, 43, 51, 52, 53, 54, 55, 61, 62, 63, 64, 65, 71, 72, 73, 74, 75, 81, 82, 83, 84, 85, 91, 92, 93];
            alpha(LETTERS[i])
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press_all(s: &mut Sim, keys: &[u16]) {
        for &k in keys {
            s.press(k);
            s.tick(Duration::from_millis(200));
        }
    }

    fn row(s: &Sim, r: usize) -> String {
        s.m.screen.cells[r].iter().collect::<String>()
    }

    #[test]
    fn arithmetic_on_the_home_screen() {
        let mut s = Sim::new();
        // 2 + 3 × 4 enter  →  14
        press_all(&mut s, &[93, 95, 94, 75, 82, 105]);
        assert_eq!(row(&s, 0).trim_end(), "2+3*4");
        assert_eq!(row(&s, 1).trim_start(), "14");
        // 2nd ans × 2 enter (Ans*2) → 28; 2nd √ ( 2nd ans ) … uses √(
        press_all(&mut s, &[21, 104, 75, 93, 105]);
        assert_eq!(row(&s, 3).trim_start(), "28");
        // store: 5 sto→ alpha A enter, then A² enter → 25
        press_all(&mut s, &[83, 91, 31, 41, 105, 31, 41, 61, 105]);
        assert_eq!(row(&s, 7).trim_start(), "25");
    }

    #[test]
    fn errors_and_editing() {
        let mut s = Sim::new();
        press_all(&mut s, &[92, 65, 102, 105]); // 1/0
        assert!(matches!(s.view, View::Error { ref msg, .. } if msg.starts_with("ERR:DIVIDE BY 0")));
        press_all(&mut s, &[92]); // 1: Quit
        assert_eq!(s.view, View::Home);
        press_all(&mut s, &[92, 93, 94, 23, 24, 23]); // 123 del ◀ del → "1"
        assert_eq!(texts(&s.input), "1");
        press_all(&mut s, &[45, 45]); // clear twice clears the screen
        assert!(row(&s, 0).trim().is_empty());
    }

    #[test]
    fn programs_menus_and_getkey() {
        let mut s = Sim::new();
        s.add_program("HELLO", "ClrHome\nDisp \"HI\"\nInput \"N?\",N\nDisp 2N\nRepeat K\ngetKey→K\nEnd\nDisp K");
        // prgm → EXEC 1 → pastes prgmHELLO; enter runs it
        press_all(&mut s, &[43, 92]);
        assert_eq!(texts(&s.input), "prgmHELLO");
        press_all(&mut s, &[105]);
        assert!(matches!(s.m.state, State::Input { .. }));
        press_all(&mut s, &[84, 105]); // 6 enter
        assert_eq!(s.m.state, State::Running); // waiting in the getKey loop
        press_all(&mut s, &[25]); // ▲ → getKey 25
        assert!(s.running.is_none());
        let screen: Vec<String> = (0..6).map(|r| row(&s, r).trim().to_string()).collect();
        assert_eq!(&screen[..5], &["HI", "N?", "6", "12", "25"]);
        assert_eq!(screen[5], "Done");
        // math menu → NUM tab → 1:abs(
        press_all(&mut s, &[41, 26, 92]);
        assert_eq!(texts(&s.input), "abs(");
    }

    #[test]
    fn on_breaks_a_loop() {
        let mut s = Sim::new();
        s.add_program("LOOP", "While 1\nEnd");
        press_all(&mut s, &[43, 92, 105]);
        assert_eq!(s.m.state, State::Running);
        press_all(&mut s, &[101]);
        assert!(matches!(s.view, View::Error { ref msg, ref program, .. } if msg == "ERR:BREAK" && program.as_deref() == Some("LOOP")));
        press_all(&mut s, &[93]); // 2: Goto
        assert_eq!(s.goto_request.as_ref().map(|g| g.0.as_str()), Some("LOOP"));
    }

    #[test]
    fn graphing() {
        let mut s = Sim::new();
        press_all(&mut s, &[11, 32, 61]); // y= X,T,θ,n x²
        assert_eq!(s.y_rows()[0], "X²");
        let f = s.functions();
        assert_eq!(f.len(), 1);
        let e = f[0].1.clone();
        assert_eq!(s.sample(&e, &[-2.0, 3.0]), vec![Some(4.0), Some(9.0)]);
        press_all(&mut s, &[15]);
        assert_eq!(s.view, View::Graph);
    }

    #[test]
    fn catalog_jumps_by_letter() {
        let mut s = Sim::new();
        press_all(&mut s, &[21, 102]); // 2nd catalog
        assert!(s.menu.as_ref().unwrap().catalog);
        press_all(&mut s, &[55]); // the key with alpha label H
        let m = s.menu.as_ref().unwrap();
        assert!(m.tabs[0].1[m.sel].0.starts_with('H') || m.tabs[0].1[m.sel].0.starts_with('h'));
    }

    #[test]
    fn desktop_keys() {
        assert_eq!(desktop_key("5", false), Some(vec![83]));
        assert_eq!(desktop_key("A", false), Some(vec![31, 41]));
        assert_eq!(desktop_key("(", false), Some(vec![63]));
    }
}
