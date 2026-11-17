//! Static problem checker for TI-BASIC programs.

use std::collections::{HashMap, HashSet};

use super::codes;
use super::parse::{Program, Stmt};
use crate::tokens;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Warning,
    Info,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diag {
    /// 1-based source line
    pub line: usize,
    pub level: Level,
    pub msg: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    IfThen,
    For,
    While,
    Repeat,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::IfThen => "If…Then",
            Kind::For => "For(",
            Kind::While => "While",
            Kind::Repeat => "Repeat",
        }
    }
}

/// Check program text. Problems are sorted by line.
pub fn check(text: &str) -> Vec<Diag> {
    match tokens::tokenize(text) {
        Err(e) => vec![Diag {
            line: e.line,
            level: Level::Error,
            msg: format!("column {}: {:?} isn't a TI-84 token", e.column, e.text.chars().take(8).collect::<String>()),
        }],
        Ok(bytes) => check_program(&Program::parse(&codes(&bytes))),
    }
}

pub fn check_program(p: &Program) -> Vec<Diag> {
    let mut d = Vec::new();
    let line = |i: usize| p.line_of(i);
    let mut push = |i: usize, level: Level, msg: String| d.push(Diag { line: line(i), level, msg });

    // neighbouring non-empty statements
    let prev = |i: usize| (0..i).rev().find(|&j| !matches!(p.stmts[j], Ok(Stmt::Empty)));
    let next = |i: usize| (i + 1..p.stmts.len()).find(|&j| !matches!(p.stmts[j], Ok(Stmt::Empty)));

    struct Open {
        kind: Kind,
        start: usize,
        has_else: bool,
    }
    let mut open: Vec<Open> = Vec::new();
    let mut blocks: Vec<(Kind, usize, usize)> = Vec::new();
    let mut seen_labels: HashMap<&str, usize> = HashMap::new();
    let mut used_labels: HashSet<&str> = HashSet::new();
    let mut gotos: Vec<(usize, &str)> = Vec::new();

    for (i, s) in p.stmts.iter().enumerate() {
        let s = match s {
            Ok(s) => s,
            Err(msg) => {
                push(i, Level::Error, msg.clone());
                continue;
            }
        };
        match s {
            Stmt::If(_) => match next(i) {
                None => push(i, Level::Warning, "If at the end of the program has nothing to run".into()),
                Some(j) if matches!(p.stmts[j], Ok(Stmt::Else) | Ok(Stmt::End)) => {
                    push(i, Level::Warning, "If is followed by Else/End, so its condition controls nothing".into())
                }
                _ => {}
            },
            Stmt::Then => {
                if prev(i).is_some_and(|j| matches!(p.stmts[j], Ok(Stmt::If(_)))) {
                    open.push(Open { kind: Kind::IfThen, start: i, has_else: false });
                } else {
                    push(i, Level::Error, "Then must come right after an If".into());
                }
            }
            Stmt::For { .. } => open.push(Open { kind: Kind::For, start: i, has_else: false }),
            Stmt::While(_) => open.push(Open { kind: Kind::While, start: i, has_else: false }),
            Stmt::Repeat(_) => open.push(Open { kind: Kind::Repeat, start: i, has_else: false }),
            Stmt::Else => match open.last_mut() {
                Some(o) if o.kind == Kind::IfThen && !o.has_else => o.has_else = true,
                Some(o) if o.kind == Kind::IfThen => push(i, Level::Error, "this If…Then already has an Else".into()),
                Some(o) => {
                    let msg = format!("Else inside the {} on line {}, which has no If…Then", o.kind.name(), line(o.start));
                    push(i, Level::Error, msg)
                }
                None => push(i, Level::Error, "Else without a matching If…Then".into()),
            },
            Stmt::End => match open.pop() {
                Some(o) => blocks.push((o.kind, o.start, i)),
                None => push(i, Level::Error, "End without a matching If…Then, For(, While or Repeat".into()),
            },
            Stmt::Lbl(name) => {
                if let Some(&first) = seen_labels.get(name.as_str()) {
                    push(i, Level::Warning, format!("Lbl {name} is also defined on line {}; Goto always uses the first one", line(first)));
                } else {
                    seen_labels.insert(name, i);
                }
            }
            Stmt::Goto(name) => {
                used_labels.insert(name);
                gotos.push((i, name));
                if !p.labels.contains_key(name) {
                    push(i, Level::Error, format!("Goto {name}: there is no Lbl {name} (ERR:LABEL)"));
                }
            }
            Stmt::Menu { items, .. } => {
                for (_, name) in items {
                    used_labels.insert(name);
                    if !p.labels.contains_key(name) {
                        push(i, Level::Error, format!("Menu( option goes to Lbl {name}, which doesn't exist (ERR:LABEL)"));
                    }
                }
            }
            Stmt::Prgm(name) => push(i, Level::Info, format!("calls prgm{name} (not checked here)")),
            _ => {}
        }
    }
    for o in &open {
        push(o.start, Level::Warning, format!("{} has no matching End", o.kind.name()));
    }
    for (name, &i) in &seen_labels {
        if !used_labels.contains(name) {
            push(i, Level::Info, format!("Lbl {name} is never used"));
        }
    }
    // Jumping out of a block leaves it open on the calculator's stack.
    for &(i, name) in &gotos {
        let Some(&target) = p.labels.get(name) else { continue };
        let innermost_left = blocks
            .iter()
            .filter(|&&(_, s, e)| s < i && i < e && !(s < target && target < e))
            .min_by_key(|&&(_, s, e)| e - s);
        if let Some(&(kind, start, _)) = innermost_left {
            let msg = format!(
                "Goto {name} jumps out of the {} on line {}; done repeatedly this leaks memory and ends in ERR:MEMORY",
                kind.name(),
                line(start)
            );
            push(i, Level::Warning, msg);
        }
    }
    d.sort_by_key(|x| (x.line, x.level));
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs(src: &str) -> Vec<(usize, Level, String)> {
        check(src).into_iter().map(|d| (d.line, d.level, d.msg)).collect()
    }

    #[test]
    fn clean_program() {
        let src = "ClrHome\nFor(I,1,5)\nIf I=3\nThen\nDisp \"THREE\"\nElse\nDisp I\nEnd\nEnd\nLbl A\nGoto A";
        assert_eq!(msgs(src), vec![]);
    }

    #[test]
    fn problems() {
        let src = "If A=1\nThen\nDisp 1\nWhile 1\nGoto Z\nDisp -2\nEnd\nElse\nLbl Q\nLbl Q\n\"HI→Ans";
        let m = msgs(src);
        let has = |line: usize, lvl: Level, frag: &str| m.iter().any(|(l, v, s)| *l == line && *v == lvl && s.contains(frag));
        assert!(has(2, Level::Warning, "has no matching End"), "{m:#?}"); // the If…Then never closes
        assert!(has(5, Level::Error, "no Lbl Z"), "{m:#?}");
        assert!(has(6, Level::Error, "negative sign"), "{m:#?}");
        assert!(has(10, Level::Warning, "also defined on line 9"), "{m:#?}");
        assert!(has(11, Level::Error, "store to Ans"), "{m:#?}");
        assert!(has(9, Level::Info, "never used"), "{m:#?}");
    }

    #[test]
    fn structure_errors() {
        let m = msgs("Then\nElse\nEnd");
        assert!(m.iter().any(|(l, _, s)| *l == 1 && s.contains("right after an If")));
        assert!(m.iter().any(|(l, _, s)| *l == 2 && s.contains("Else without")));
        assert!(m.iter().any(|(l, _, s)| *l == 3 && s.contains("End without")));
    }

    #[test]
    fn goto_out_of_loop() {
        let m = msgs("For(I,1,9)\nIf I=5\nGoto X\nEnd\nLbl X");
        assert!(m.iter().any(|(l, v, s)| *l == 3 && *v == Level::Warning && s.contains("jumps out of the For(")), "{m:#?}");
    }

    #[test]
    fn bad_token() {
        let m = msgs("Disp 1\nDisp ≈");
        assert_eq!(m[0].0, 2);
        assert_eq!(m[0].1, Level::Error);
    }
}
