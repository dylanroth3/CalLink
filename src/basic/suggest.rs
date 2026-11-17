//! Completion suggestions and argument hints for the Program Editor.
//!
//! Suggestions come from the token catalog. Ranking is "predictive": tokens already used in
//! the program, commonly used tokens, and tokens that fit the position (a command at the start
//! of a statement, a function inside an expression) come first. Plain-ASCII shortcuts
//! (`sqrt`, `->`, `<=`, `L1`, `pi`…) suggest the matching calculator symbol.

use std::collections::HashMap;

use super::{codes, tk};
use crate::tokens::{self, TokenTable};

#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    /// Text that replaces the typed prefix.
    pub insert: String,
    /// What the list shows (the token, or "shortcut → token").
    pub label: String,
    pub hint: Option<&'static str>,
    pub code: Option<u16>,
}

/// Short descriptions, written for CalLink. Square brackets mark optional arguments.
const HINTS: &[(&str, &str)] = &[
    ("Disp", "Disp [value,…] — show values or text on the home screen"),
    ("Output(", "Output(row,column,value) — write at a position on the home screen"),
    ("Input", "Input [\"prompt\",]variable — ask the user for a value"),
    ("Prompt", "Prompt variable[,variable…] — ask for each variable as NAME=?"),
    ("ClrHome", "ClrHome — clear the home screen"),
    ("If", "If condition — run the next line (or Then…End) only when true"),
    ("Then", "Then — starts the block run when the If above is true"),
    ("Else", "Else — starts the block run when the If…Then condition is false"),
    ("End", "End — closes If…Then, For(, While or Repeat"),
    ("For(", "For(variable,start,end[,step]) … End — count a variable from start to end"),
    ("While", "While condition … End — repeat while the condition is true"),
    ("Repeat", "Repeat condition … End — repeat until the condition is true (runs at least once)"),
    ("Lbl", "Lbl name — a jump target (1–2 characters)"),
    ("Goto", "Goto name — jump to Lbl name"),
    ("Pause", "Pause [value] — wait for ENTER, optionally showing a value"),
    ("Stop", "Stop — end the program"),
    ("Return", "Return — leave this program (back to the caller, if any)"),
    ("Menu(", "Menu(\"title\",\"option\",label[,\"option\",label…]) — show a menu and jump to a label"),
    ("getKey", "getKey — code of the last key pressed, or 0 if none"),
    ("DelVar", "DelVar variable — delete a variable"),
    ("IS>(", "IS>(variable,value) — add 1; skip the next line if now greater than value"),
    ("DS<(", "DS<(variable,value) — subtract 1; skip the next line if now less than value"),
    ("prgm", "prgmNAME — run another program"),
    ("randInt(", "randInt(lower,upper[,count]) — random whole number(s) between lower and upper"),
    ("rand", "rand — random number from 0 to 1"),
    ("abs(", "abs(value) — absolute value"),
    ("int(", "int(value) — round down to a whole number"),
    ("iPart(", "iPart(value) — integer part (toward zero)"),
    ("fPart(", "fPart(value) — fractional part"),
    ("round(", "round(value[,decimals]) — round to a number of decimal places"),
    ("√(", "√(value) — square root"),
    ("min(", "min(a,b) or min(list) — smallest value"),
    ("max(", "max(a,b) or max(list) — largest value"),
    ("dim(", "dim(list) — number of elements (store to it to resize)"),
    ("sum(", "sum(list) — total of the elements"),
    ("prod(", "prod(list) — product of the elements"),
    ("mean(", "mean(list) — average of the elements"),
    ("length(", "length(string) — number of characters"),
    ("sub(", "sub(string,start,length) — part of a string"),
    ("inString(", "inString(string,search[,start]) — position of search in string, 0 if absent"),
    ("remainder(", "remainder(dividend,divisor) — remainder after division"),
    ("gcd(", "gcd(a,b) — greatest common divisor"),
    ("lcm(", "lcm(a,b) — least common multiple"),
    ("toString(", "toString(value) — convert a number to a string"),
    ("eval(", "eval(expression) — value of an expression, for use in strings"),
    ("expr(", "expr(string) — evaluate the expression in a string"),
    ("not(", "not(value) — 1 if value is 0, otherwise 0"),
    ("sin(", "sin(angle)"),
    ("cos(", "cos(angle)"),
    ("tan(", "tan(angle)"),
    ("sin⁻¹(", "sin⁻¹(value) — arcsine"),
    ("cos⁻¹(", "cos⁻¹(value) — arccosine"),
    ("tan⁻¹(", "tan⁻¹(value) — arctangent"),
    ("ln(", "ln(value) — natural logarithm"),
    ("log(", "log(value) — base-10 logarithm"),
    ("ℯ^(", "ℯ^(power) — e raised to a power"),
    ("₁₀^(", "₁₀^(power) — 10 raised to a power"),
    ("seq(", "seq(expression,variable,start,end[,step]) — build a list"),
    ("augment(", "augment(listA,listB) — join two lists"),
    ("cumSum(", "cumSum(list) — running totals"),
    ("SortA(", "SortA(list) — sort ascending"),
    ("SortD(", "SortD(list) — sort descending"),
    ("Fill(", "Fill(value,list) — set every element"),
    ("ClrDraw", "ClrDraw — clear the graph screen"),
    ("Line(", "Line(x1,y1,x2,y2) — draw a line on the graph screen"),
    ("Circle(", "Circle(x,y,radius) — draw a circle on the graph screen"),
    ("Text(", "Text(row,column,value) — write on the graph screen"),
    ("Pt-On(", "Pt-On(x,y) — turn a point on"),
    ("Pt-Off(", "Pt-Off(x,y) — turn a point off"),
    ("Pxl-On(", "Pxl-On(row,column) — turn a pixel on"),
    ("Pxl-Off(", "Pxl-Off(row,column) — turn a pixel off"),
    ("pxl-Test(", "pxl-Test(row,column) — 1 if the pixel is on"),
    ("Horizontal", "Horizontal y — draw a horizontal line"),
    ("Vertical", "Vertical x — draw a vertical line"),
    ("DispGraph", "DispGraph — show the graph screen"),
    ("Wait", "Wait seconds — pause for a time (CE)"),
    ("BackgroundOn", "BackgroundOn color — set a graph background (CE)"),
    ("TextColor(", "TextColor(color) — text colour on the graph screen (CE)"),
    ("Ans", "Ans — the last result"),
    ("π", "π — pi"),
    ("→", "→ — store into a variable"),
];

/// ASCII shortcuts → token text.
const ALIASES: &[(&str, &str)] = &[
    ("sqrt", "√("),
    ("pi", "π"),
    ("theta", "θ"),
    ("->", "→"),
    ("store", "→"),
    ("<=", "≤"),
    (">=", "≥"),
    ("!=", "≠"),
    ("<>", "≠"),
    ("neg", "⁻"),
    ("negative", "⁻"),
    ("squared", "²"),
    ("cubed", "³"),
    ("inverse", "⁻¹"),
    ("exp(", "ℯ^("),
    ("e^(", "ℯ^("),
    ("10^(", "₁₀^("),
    ("asin(", "sin⁻¹("),
    ("acos(", "cos⁻¹("),
    ("atan(", "tan⁻¹("),
    ("L1", "L₁"),
    ("L2", "L₂"),
    ("L3", "L₃"),
    ("L4", "L₄"),
    ("L5", "L₅"),
    ("L6", "L₆"),
    ("list", "ʟ"),
    ("Y1", "Y₁"),
    ("Y2", "Y₂"),
];

/// Commonly used tokens get a ranking boost.
const POPULAR: &[&str] = &[
    "Disp ", "If ", "Then", "Else", "End", "For(", "While ", "Repeat ", "Input ", "Prompt ", "Output(", "ClrHome", "getKey",
    "randInt(", "Lbl ", "Goto ", "Pause ", "Stop", "Return", "Menu(", "abs(", "int(", "round(", "sub(", "length(", "dim(",
    "remainder(", "Ans", "not(", "max(", "min(", "sum(",
];

pub fn hint(token_text: &str) -> Option<&'static str> {
    let t = token_text.trim_end();
    HINTS.iter().find(|(k, _)| *k == t).map(|(_, h)| *h)
}

/// Is this a command that begins a statement (rather than a value or function)?
fn is_command(code: u16, text: &str) -> bool {
    (tk::IF..=tk::MENU).contains(&code) || text.ends_with(' ') || matches!(code, tk::DELVAR | tk::PRGM | 0x85..=0x9F)
}

/// The word being typed just before the cursor: (characters, word). Returns None inside a
/// string, in the middle of a word, or when nothing completable is being typed.
pub fn word_before(line_before: &str, line_after: &str) -> Option<(usize, String)> {
    if in_string(line_before) {
        return None;
    }
    if line_after.chars().next().is_some_and(|c| c.is_alphanumeric()) {
        return None;
    }
    for op in ["->", "<=", ">=", "!=", "<>"] {
        if line_before.ends_with(op) {
            return Some((2, op.to_string()));
        }
    }
    let word: Vec<char> = line_before.chars().rev().take_while(|c| c.is_alphanumeric() || *c == '|' || *c == '^').collect();
    let word: String = word.into_iter().rev().collect();
    // must start with a letter (or | for the statistics variables)
    let word = word.trim_start_matches(|c: char| c.is_ascii_digit() || c == '^').to_string();
    let first = word.chars().next()?;
    if !(first.is_alphabetic() || first == '|') || word.chars().count() < 2 {
        return None;
    }
    Some((word.chars().count(), word))
}

fn in_string(line_before: &str) -> bool {
    let mut inside = false;
    for c in line_before.chars() {
        match c {
            '"' => inside = !inside,
            '→' | ':' if inside && c == '→' => inside = false,
            ':' if !inside => {}
            _ => {}
        }
    }
    inside
}

/// Does the word start a statement (beginning of line or after ':')?
pub fn at_statement_start(line_before: &str, word_len: usize) -> bool {
    let n = line_before.chars().count().saturating_sub(word_len);
    let head: String = line_before.chars().take(n).collect();
    let head = head.trim_end();
    head.is_empty() || head.ends_with(':')
}

/// How often each token appears in the program (for predictive ranking).
pub fn usage(program: &str) -> HashMap<u16, usize> {
    let mut m = HashMap::new();
    if let Ok(b) = tokens::tokenize(program) {
        for c in codes(&b) {
            *m.entry(c).or_insert(0) += 1;
        }
    }
    m
}

/// Ranked suggestions for a typed word.
pub fn suggest(word: &str, statement_start: bool, usage: &HashMap<u16, usize>, max: usize) -> Vec<Suggestion> {
    let lw = word.to_lowercase();
    let table = TokenTable::get();
    let strict = word.chars().count() <= 2 && word.chars().all(|c| c.is_ascii_uppercase());
    let mut scored: Vec<(i32, Suggestion)> = Vec::new();
    for (code, text) in table.all() {
        let shown = text.trim_end();
        if shown.chars().count() < 2 || shown == word || !shown.to_lowercase().starts_with(&lw) {
            continue;
        }
        if shown.starts_with('|') != word.starts_with('|') {
            continue;
        }
        // "AB" is usually two variables, so short all-caps words only match case-exactly
        if strict && !shown.starts_with(word) {
            continue;
        }
        let mut score = 0;
        if shown.starts_with(word) {
            score += 3; // same case
        }
        score += usage.get(&code).copied().unwrap_or(0).min(5) as i32 * 2;
        if POPULAR.contains(&text) {
            score += 4;
        }
        let cmd = is_command(code, text);
        if statement_start && cmd {
            score += 3;
        }
        if !statement_start && !cmd {
            score += 2;
        }
        if hint(text).is_some() {
            score += 1;
        }
        scored.push((score, Suggestion { insert: text.to_string(), label: shown.to_string(), hint: hint(text), code: Some(code) }));
    }
    for (alias, target) in ALIASES {
        let la = alias.to_lowercase();
        if la.starts_with(&lw) && lw.len() >= 2 {
            let code = table.code(target);
            let exact = la == lw;
            scored.push((
                if exact { 20 } else { 6 },
                Suggestion { insert: target.to_string(), label: format!("{alias} → {target}"), hint: hint(target), code },
            ));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.label.len().cmp(&b.1.label.len())).then(a.1.label.cmp(&b.1.label)));
    let mut seen = std::collections::HashSet::new();
    scored.into_iter().map(|(_, s)| s).filter(|s| seen.insert(s.insert.clone())).take(max).collect()
}

/// Argument hint for the innermost unclosed function call before the cursor:
/// (hint text, index of the argument being typed).
pub fn call_hint(line_before: &str) -> Option<(&'static str, usize)> {
    let chars: Vec<char> = line_before.chars().collect();
    let mut depth = 0i32;
    let mut commas = 0usize;
    let mut inside = false;
    // scan backwards for the unmatched '(' (strings are skipped by quote parity from the start)
    let mut quote_open = vec![false; chars.len()];
    for (i, &c) in chars.iter().enumerate() {
        if c == '"' {
            inside = !inside;
        } else if c == '→' {
            inside = false;
        }
        quote_open[i] = inside;
    }
    for i in (0..chars.len()).rev() {
        if quote_open[i] || chars[i] == '"' {
            continue;
        }
        match chars[i] {
            ')' | '}' => depth += 1,
            '{' => depth -= 1,
            ',' if depth == 0 => commas += 1,
            '(' if depth > 0 => depth -= 1,
            '(' => {
                let head: String = chars[..=i].iter().collect();
                let (_, h) = HINTS
                    .iter()
                    .filter(|(k, _)| k.ends_with('(') && head.ends_with(k))
                    .max_by_key(|(k, _)| k.len())?;
                return Some((h, commas));
            }
            ':' => return None,
            _ => {}
        }
    }
    None
}

/// Split a hint into (before, current argument, after) for highlighting.
pub fn split_hint(hint: &str, arg: usize) -> (String, String, String) {
    let sig = hint.split(" — ").next().unwrap_or(hint);
    let Some(open) = sig.find('(') else { return (hint.to_string(), String::new(), String::new()) };
    let close = sig.rfind(')').unwrap_or(sig.len());
    let inner = &sig[open + 1..close];
    // split on commas, keeping optional-argument brackets with their argument
    let mut parts: Vec<(usize, usize)> = Vec::new();
    let mut start = 0;
    for (i, c) in inner.char_indices() {
        if c == ',' {
            parts.push((start, i));
            start = i + 1;
        }
    }
    parts.push((start, inner.len()));
    let Some(&(s, e)) = parts.get(arg) else { return (hint.to_string(), String::new(), String::new()) };
    let (mut s, mut e) = (open + 1 + s, open + 1 + e);
    // don't highlight the brackets that mark optional arguments
    while s < e && matches!(hint.as_bytes()[s], b'[' | b',') {
        s += 1;
    }
    while e > s && matches!(hint.as_bytes()[e - 1], b'[' | b']') {
        e -= 1;
    }
    (hint[..s].to_string(), hint[s..e].to_string(), hint[e..].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words() {
        assert_eq!(word_before("Disp ran", ""), Some((3, "ran".into())));
        assert_eq!(word_before("Disp \"ran", ""), None); // inside a string
        assert_eq!(word_before("5-", ""), None);
        assert_eq!(word_before("A->", ""), Some((2, "->".into())));
        assert_eq!(word_before("Di", "sp"), None); // middle of a word
        assert_eq!(word_before("3Di", ""), Some((2, "Di".into())));
        assert!(at_statement_start("If 1:Di", 2));
        assert!(!at_statement_start("A+Di", 2));
    }

    #[test]
    fn ranking() {
        let none = HashMap::new();
        let s = suggest("ran", false, &none, 8);
        assert_eq!(s[0].insert, "randInt(", "{s:?}");
        let s = suggest("Di", true, &none, 8);
        assert_eq!(s[0].insert, "Disp ", "{s:?}");
        assert!(s[0].hint.unwrap().starts_with("Disp"));
        assert!(suggest("AB", false, &none, 8).is_empty());
        // tokens used in the program float up
        let used = usage("DispGraph\nDispGraph\nDispGraph");
        let s = suggest("Di", true, &used, 8);
        assert_eq!(s[0].insert, "DispGraph", "{s:?}");
    }

    #[test]
    fn shortcuts() {
        let none = HashMap::new();
        assert_eq!(suggest("sqrt", false, &none, 3)[0].insert, "√(");
        assert_eq!(suggest("->", false, &none, 3)[0].insert, "→");
        assert_eq!(suggest("L1", false, &none, 3)[0].insert, "L₁");
        assert_eq!(suggest("pi", false, &none, 3)[0].insert, "π");
        // every shortcut target is a real token
        for (_, t) in ALIASES {
            assert!(TokenTable::get().code(t).is_some(), "{t}");
        }
    }

    #[test]
    fn hints_are_tokens() {
        for (k, _) in HINTS {
            let t = TokenTable::get();
            assert!(t.code(k).is_some() || t.code(&format!("{k} ")).is_some(), "hint key {k:?} is not a token");
        }
    }

    #[test]
    fn argument_hints() {
        let (h, i) = call_hint("Disp randInt(1,").unwrap();
        assert!(h.starts_with("randInt("));
        assert_eq!(i, 1);
        assert_eq!(split_hint(h, i).1, "upper");
        assert_eq!(split_hint(h, 2).1, "count");
        let (h, i) = call_hint("Output(1,abs(X),\"A,B").unwrap();
        assert!(h.starts_with("Output("));
        assert_eq!(i, 2);
        assert!(call_hint("Disp abs(3)").is_none());
    }
}
