//! The OSL light path expression grammar, parsed into an [`Ast`].
//!
//! Supported, exactly as OSL writes it:
//!
//! - events `<type scatter 'label' …>`, each position a letter, `.`, a quoted
//!   label, or a set `[…]` / `[^…]` of them;
//! - the shorthands `R` = `<R.>` (and every type letter), `D` = `<.D>` (and
//!   every scatter letter), `'x'` = `<..'x'>`, and `.` for any event;
//! - concatenation, `|`, `*`, `+`, `{n}`, `{n,m}`, `{n,}`, `( … )`, and
//!   top-level sets of events `[…]` / `[^…]`.
//!
//! Refused with the column of the offending token: `?` (OSL has none), `!`
//! inversion, RenderMan lobe tokens (`D1`, `U2` …) and prefixes
//! (`unoccluded` …), `B` (crust has no background event; the dome is `L`), and
//! anything else that is not part of the grammar.

use super::{EventType, Scatter};

/// A parse failure: what was wrong, and the 1-based column it was found at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub column: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "column {}: {}", self.column, self.message)
    }
}

/// A set of values for one event position: everything, or a list taken or
/// excluded.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Set<T> {
    Any,
    In(Vec<T>),
    NotIn(Vec<T>),
}

impl<T: PartialEq> Set<T> {
    pub(super) fn contains(&self, x: &T) -> bool {
        match self {
            Set::Any => true,
            Set::In(v) => v.contains(x),
            Set::NotIn(v) => !v.contains(x),
        }
    }
}

/// One event pattern: a set per position. A crust event has one label (or
/// none), so every label position must accept that same label.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Pattern {
    pub(super) types: Set<EventType>,
    pub(super) scatters: Set<Scatter>,
    /// `None` inside is "no label"; a quoted label is `Some(name)`.
    pub(super) labels: Vec<Set<Option<String>>>,
}

impl Pattern {
    fn any() -> Pattern {
        Pattern {
            types: Set::Any,
            scatters: Set::Any,
            labels: Vec::new(),
        }
    }
}

/// A predicate over single events: a union of patterns, possibly
/// complemented (a top-level `[^…]`).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Pred {
    pub(super) patterns: Vec<Pattern>,
    pub(super) negated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Ast {
    Event(Pred),
    Concat(Vec<Ast>),
    Alt(Vec<Ast>),
    Repeat {
        node: Box<Ast>,
        min: u32,
        max: Option<u32>,
    },
}

/// The largest bound `{n}` / `{n,m}` accepts — a bound is expanded into
/// copies of its operand, so it must stay small.
const MAX_REPEAT: u32 = 32;

pub(super) fn parse(expr: &str) -> Result<Ast, ParseError> {
    let chars: Vec<char> = expr.chars().collect();
    let mut p = Parser { chars, pos: 0 };
    p.skip_ws();
    if p.peek().is_none() {
        return Err(p.err("empty expression"));
    }
    let ast = p.alternation()?;
    p.skip_ws();
    if let Some(c) = p.peek() {
        return Err(p.err(format!("unexpected {c:?}")));
    }
    Ok(ast)
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    fn err(&self, message: impl Into<String>) -> ParseError {
        ParseError {
            column: self.pos + 1,
            message: message.into(),
        }
    }

    fn expect(&mut self, c: char) -> Result<(), ParseError> {
        self.skip_ws();
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.err(format!("expected {c:?}")))
        }
    }

    fn alternation(&mut self) -> Result<Ast, ParseError> {
        let mut alts = vec![self.concatenation()?];
        loop {
            self.skip_ws();
            if self.peek() == Some('|') {
                self.pos += 1;
                alts.push(self.concatenation()?);
            } else {
                break;
            }
        }
        Ok(if alts.len() == 1 {
            alts.pop().expect("one")
        } else {
            Ast::Alt(alts)
        })
    }

    fn concatenation(&mut self) -> Result<Ast, ParseError> {
        let mut items = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None | Some('|') | Some(')') => break,
                _ => items.push(self.repetition()?),
            }
        }
        match items.len() {
            0 => Err(self.err("expected an event")),
            1 => Ok(items.pop().expect("one")),
            _ => Ok(Ast::Concat(items)),
        }
    }

    fn repetition(&mut self) -> Result<Ast, ParseError> {
        let mut node = self.atom()?;
        loop {
            match self.peek() {
                Some('*') => {
                    self.pos += 1;
                    node = Ast::Repeat {
                        node: Box::new(node),
                        min: 0,
                        max: None,
                    };
                }
                Some('+') => {
                    self.pos += 1;
                    node = Ast::Repeat {
                        node: Box::new(node),
                        min: 1,
                        max: None,
                    };
                }
                Some('?') => {
                    return Err(self.err("'?' is not part of the OSL LPE grammar; use {0,1}"));
                }
                Some('{') => {
                    self.pos += 1;
                    let min = self.number()?;
                    let max = if self.peek() == Some(',') {
                        self.pos += 1;
                        if self.peek() == Some('}') {
                            None
                        } else {
                            Some(self.number()?)
                        }
                    } else {
                        Some(min)
                    };
                    if self.peek() != Some('}') {
                        return Err(self.err("expected '}'"));
                    }
                    self.pos += 1;
                    if max.is_some_and(|m| m < min) {
                        return Err(self.err("a repetition's maximum is below its minimum"));
                    }
                    if min > MAX_REPEAT || max.is_some_and(|m| m > MAX_REPEAT) {
                        return Err(self.err(format!("repetition bounds above {MAX_REPEAT}")));
                    }
                    node = Ast::Repeat {
                        node: Box::new(node),
                        min,
                        max,
                    };
                }
                _ => break,
            }
        }
        Ok(node)
    }

    fn number(&mut self) -> Result<u32, ParseError> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if start == self.pos {
            return Err(self.err("expected a number"));
        }
        let s: String = self.chars[start..self.pos].iter().collect();
        s.parse().map_err(|_| ParseError {
            column: start + 1,
            message: "number out of range".into(),
        })
    }

    fn atom(&mut self) -> Result<Ast, ParseError> {
        self.skip_ws();
        let Some(c) = self.peek() else {
            return Err(self.err("expected an event"));
        };
        match c {
            '(' => {
                self.pos += 1;
                let inner = self.alternation()?;
                self.expect(')')?;
                Ok(inner)
            }
            '[' => {
                self.pos += 1;
                let negated = self.peek() == Some('^');
                if negated {
                    self.pos += 1;
                }
                let mut patterns = Vec::new();
                loop {
                    self.skip_ws();
                    match self.peek() {
                        Some(']') => {
                            self.pos += 1;
                            break;
                        }
                        None => return Err(self.err("unterminated '['")),
                        _ => patterns.push(self.single_event()?),
                    }
                }
                if patterns.is_empty() {
                    return Err(self.err("empty set"));
                }
                Ok(Ast::Event(Pred { patterns, negated }))
            }
            _ => Ok(Ast::Event(Pred {
                patterns: vec![self.single_event()?],
                negated: false,
            })),
        }
    }

    /// One event pattern: `<…>`, `.`, a type or scatter letter, or a quoted
    /// label.
    fn single_event(&mut self) -> Result<Pattern, ParseError> {
        let Some(c) = self.peek() else {
            return Err(self.err("expected an event"));
        };
        match c {
            '<' => {
                self.pos += 1;
                self.event_body()
            }
            '.' => {
                self.pos += 1;
                Ok(Pattern::any())
            }
            '\'' => {
                let label = self.quoted()?;
                Ok(Pattern {
                    labels: vec![Set::In(vec![Some(label)])],
                    ..Pattern::any()
                })
            }
            '!' => Err(self.err("'!' (inversion) is not supported")),
            'B' => Err(self.err("'B' is not an event in crust: a dome or the sky is a light, 'L'")),
            c if c.is_alphabetic() => {
                self.check_not_a_word()?;
                self.pos += 1;
                if let Some(t) = EventType::letter(c) {
                    Ok(Pattern {
                        types: Set::In(vec![t]),
                        ..Pattern::any()
                    })
                } else if let Some(s) = Scatter::letter(c) {
                    Ok(Pattern {
                        scatters: Set::In(vec![s]),
                        ..Pattern::any()
                    })
                } else {
                    self.pos -= 1;
                    Err(self.err(format!("unknown event {c:?}")))
                }
            }
            c => Err(self.err(format!("unexpected {c:?}"))),
        }
    }

    /// Refuses a letter that starts a RenderMan lobe token (`D1`, `U12`) or a
    /// word (`unoccluded`): a shorthand is exactly one letter.
    fn check_not_a_word(&self) -> Result<(), ParseError> {
        let next = self.chars.get(self.pos + 1).copied();
        if next.is_some_and(|n| n.is_ascii_digit()) {
            return Err(self.err("RenderMan lobe tokens (D1, U2, …) are not supported"));
        }
        // A run of three lowercase letters is a word; `ss` is two straight
        // events.
        let word = self.chars[self.pos..]
            .iter()
            .take(3)
            .filter(|c| c.is_ascii_lowercase())
            .count()
            == 3;
        if word {
            return Err(self.err("prefixes and words (unoccluded, shadows, …) are not supported"));
        }
        Ok(())
    }

    fn quoted(&mut self) -> Result<String, ParseError> {
        debug_assert_eq!(self.peek(), Some('\''));
        self.pos += 1;
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == '\'' {
                let s: String = self.chars[start..self.pos].iter().collect();
                self.pos += 1;
                return Ok(s);
            }
            self.pos += 1;
        }
        Err(ParseError {
            column: start,
            message: "unterminated label".into(),
        })
    }

    /// After `<`: positions until `>`.
    fn event_body(&mut self) -> Result<Pattern, ParseError> {
        let mut pattern = Pattern::any();
        let mut position = 0;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('>') => {
                    self.pos += 1;
                    return Ok(pattern);
                }
                None => return Err(self.err("unterminated '<'")),
                _ => {}
            }
            match position {
                0 => pattern.types = self.position_set(EventType::letter, "event type")?,
                1 => pattern.scatters = self.position_set(Scatter::letter, "scatter")?,
                _ => pattern.labels.push(self.label_set()?),
            }
            position += 1;
        }
    }

    /// A type or scatter position: a letter, `.`, or `[…]` / `[^…]`.
    fn position_set<T>(
        &mut self,
        letter: fn(char) -> Option<T>,
        what: &str,
    ) -> Result<Set<T>, ParseError> {
        match self.peek() {
            Some('.') => {
                self.pos += 1;
                Ok(Set::Any)
            }
            Some('[') => {
                self.pos += 1;
                let negated = self.peek() == Some('^');
                if negated {
                    self.pos += 1;
                }
                let mut items = Vec::new();
                loop {
                    match self.peek() {
                        Some(']') => {
                            self.pos += 1;
                            break;
                        }
                        Some(c) => match letter(c) {
                            Some(x) => {
                                items.push(x);
                                self.pos += 1;
                            }
                            None => return Err(self.err(format!("{c:?} is not a {what}"))),
                        },
                        None => return Err(self.err("unterminated '['")),
                    }
                }
                Ok(if negated {
                    Set::NotIn(items)
                } else {
                    Set::In(items)
                })
            }
            Some(c) => match letter(c) {
                Some(x) => {
                    self.pos += 1;
                    Ok(Set::In(vec![x]))
                }
                None => Err(self.err(format!("{c:?} is not a {what}"))),
            },
            None => Err(self.err("unterminated '<'")),
        }
    }

    /// A label position: `'x'`, `.`, or a set of quoted labels.
    fn label_set(&mut self) -> Result<Set<Option<String>>, ParseError> {
        match self.peek() {
            Some('.') => {
                self.pos += 1;
                Ok(Set::Any)
            }
            Some('\'') => Ok(Set::In(vec![Some(self.quoted()?)])),
            Some('[') => {
                self.pos += 1;
                let negated = self.peek() == Some('^');
                if negated {
                    self.pos += 1;
                }
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    match self.peek() {
                        Some(']') => {
                            self.pos += 1;
                            break;
                        }
                        Some('\'') => items.push(Some(self.quoted()?)),
                        Some(c) => return Err(self.err(format!("{c:?} is not a quoted label"))),
                        None => return Err(self.err("unterminated '['")),
                    }
                }
                Ok(if negated {
                    Set::NotIn(items)
                } else {
                    Set::In(items)
                })
            }
            Some(c) => Err(self.err(format!("{c:?} is not a label"))),
            None => Err(self.err("unterminated '<'")),
        }
    }
}
