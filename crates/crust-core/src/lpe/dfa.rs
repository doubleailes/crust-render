//! Compiling a render's LPEs into one DFA: Thompson NFA per expression, one
//! start joining them, subset construction over crust's concrete events.
//!
//! The alphabet is finite and small — 6 types × 5 scatter kinds × the
//! labels — so every DFA state stores a full transition row and stepping a
//! path is one indexed load. State 0 is the dead state: no expression can
//! accept from it, and the integrator skips it.

use std::collections::HashMap;

use super::parse::{Ast, Pattern, Pred, Set, parse};
use super::{
    CompileError, EventType, LabelId, LobeEvent, LobeLabel, MAX_EXPRESSIONS, MAX_STATES, Scatter,
};

/// A compiled set of light path expressions.
#[derive(Debug, Clone)]
pub struct Lpe {
    /// Label names by id: index 0 is "no label".
    labels: Vec<Option<String>>,
    /// Transitions, `states × symbols`, row-major.
    table: Vec<u16>,
    symbols: usize,
    /// Per state, the expressions accepting there (bit `i` = expression `i`).
    accept: Vec<u64>,
    /// Per state, the expressions some accepting state of which is still
    /// reachable from it — a path in a state that cannot accept an
    /// expression contributes nothing to it, whatever follows.
    live: Vec<u64>,
    /// The state after the camera event: where every path starts.
    start: u16,
    expressions: usize,
    /// Per expression, per state: the state's class under that expression
    /// alone — two states in one class accept exactly the same suffixes for
    /// it. The DFA is shared by every expression, so two lobes can lead to
    /// different states that one expression cannot tell apart; this is how
    /// the integrator sees that they agree for it. `classes[i * states + s]`.
    classes: Vec<u16>,
}

/// The dead state.
pub const DEAD: u16 = 0;

impl Lpe {
    /// Compiles `expressions` (each without an `lpe:` prefix). Expression `i`
    /// accepts as bit `i`; at most [`MAX_EXPRESSIONS`], and at most
    /// [`MAX_STATES`] DFA states between them.
    ///
    /// The labels are the lobe labels and the ones the expressions name.
    /// A light whose tag no expression names carries "no label" ([`Lpe::label`]
    /// is `None` for it): nothing could tell it from an untagged light, and
    /// keeping it out keeps the alphabet bounded by the expressions, not by
    /// the scene.
    pub fn compile(expressions: &[&str]) -> Result<Lpe, CompileError> {
        if expressions.len() > MAX_EXPRESSIONS {
            return Err(CompileError::TooManyExpressions(expressions.len()));
        }
        let asts = expressions
            .iter()
            .enumerate()
            .map(|(index, e)| parse(e).map_err(|error| CompileError::Parse { index, error }))
            .collect::<Result<Vec<_>, _>>()?;

        // Labels: none, the lobe labels, then every label an expression
        // names (a light tag, or one nothing carries — it can still be
        // excluded).
        let mut labels: Vec<Option<String>> = vec![None];
        labels.extend(LobeLabel::ALL.iter().map(|l| Some(l.name().to_owned())));
        for ast in &asts {
            collect_labels(ast, &mut labels);
        }
        let symbols = EventType::COUNT * Scatter::COUNT * labels.len();
        // Every symbol must fit a `u16` below `u16::MAX`, which the router
        // keeps for "no event".
        if symbols >= u16::MAX as usize {
            return Err(CompileError::TooManyLabels(labels.len()));
        }

        // Thompson construction.
        let mut nfa = Nfa::default();
        let start = nfa.state();
        for (i, ast) in asts.iter().enumerate() {
            let (s, e) = nfa.build(ast, &labels, symbols);
            nfa.eps(start, s);
            nfa.finals.push((e, i));
        }

        // Subset construction. Sets are sorted state lists; set 0 is empty.
        let mut ids: HashMap<Vec<usize>, u16> = HashMap::new();
        let mut sets: Vec<Vec<usize>> = vec![Vec::new()];
        ids.insert(Vec::new(), DEAD);
        let first = nfa.closure(vec![start]);
        let first_id = intern(&mut ids, &mut sets, first)?;
        let mut table: Vec<u16> = Vec::new();
        let mut done = 0;
        while done < sets.len() {
            let set = sets[done].clone();
            for sym in 0..symbols {
                let mut next = Vec::new();
                for &q in &set {
                    for (pred, to) in &nfa.edges[q] {
                        if pred.get(sym) {
                            next.push(*to);
                        }
                    }
                }
                let next = nfa.closure(next);
                let id = intern(&mut ids, &mut sets, next)?;
                table.push(id);
            }
            done += 1;
        }
        let accept: Vec<u64> = sets
            .iter()
            .map(|set| {
                nfa.finals
                    .iter()
                    .filter(|(f, _)| set.contains(f))
                    .fold(0u64, |m, (_, i)| m | (1 << i))
            })
            .collect();
        // Live: the expressions an accepting state is reachable for.
        // Iterate to a fixed point.
        let n = sets.len();
        let mut live: Vec<u64> = accept.clone();
        loop {
            let mut changed = false;
            for s in 0..n {
                let reach = table[s * symbols..(s + 1) * symbols]
                    .iter()
                    .fold(live[s], |m, &t| m | live[t as usize]);
                if reach != live[s] {
                    live[s] = reach;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let classes = (0..expressions.len())
            .flat_map(|i| refine(&table, symbols, &accept, i))
            .collect();
        let mut lpe = Lpe {
            labels,
            table,
            symbols,
            accept,
            live,
            start: first_id,
            expressions: expressions.len(),
            classes,
        };
        lpe.start = lpe.step(first_id, lpe.symbol(EventType::Camera, Scatter::None, 0));
        Ok(lpe)
    }

    /// How many expressions were compiled.
    pub fn len(&self) -> usize {
        self.expressions
    }

    pub fn is_empty(&self) -> bool {
        self.expressions == 0
    }

    /// The number of DFA states, the dead one included.
    pub fn states(&self) -> usize {
        self.accept.len()
    }

    /// The state every path is in after its camera event.
    #[inline]
    pub fn start(&self) -> u16 {
        self.start
    }

    /// The id of label `name`, if any expression or light can carry it.
    pub fn label(&self, name: &str) -> Option<LabelId> {
        self.labels
            .iter()
            .position(|l| l.as_deref() == Some(name))
            .map(|i| i as LabelId)
    }

    /// The symbol of an event.
    #[inline]
    pub fn symbol(&self, ty: EventType, scatter: Scatter, label: LabelId) -> u16 {
        ((ty as usize * Scatter::COUNT + scatter as usize) * self.labels.len() + label as usize)
            as u16
    }

    /// The symbol of a surface lobe's event.
    #[inline]
    pub fn lobe_symbol(&self, e: LobeEvent) -> u16 {
        let ty = if e.transmit {
            EventType::Transmit
        } else {
            EventType::Reflect
        };
        self.symbol(ty, e.scatter, e.label as LabelId)
    }

    /// One transition.
    #[inline]
    pub fn step(&self, state: u16, symbol: u16) -> u16 {
        self.table[state as usize * self.symbols + symbol as usize]
    }

    /// The expressions accepting in `state`.
    #[inline]
    pub fn accepts(&self, state: u16) -> u64 {
        self.accept[state as usize]
    }

    /// `state`'s class under expression `i` alone (see `classes`).
    #[inline]
    pub fn class(&self, i: usize, state: u16) -> u16 {
        self.classes[i * self.accept.len() + state as usize]
    }

    /// Whether any expression can still accept from `state`.
    #[inline]
    pub fn live(&self, state: u16) -> bool {
        self.live[state as usize] != 0
    }

    /// The expressions that can still accept from `state`.
    #[inline]
    pub fn live_mask(&self, state: u16) -> u64 {
        self.live[state as usize]
    }

    /// Whether the whole word `events` (camera first) is accepted by
    /// expression `i` — for tests and probes.
    pub fn matches(&self, events: &[(EventType, Scatter, LabelId)], i: usize) -> bool {
        let mut s = self.start_state_before_camera();
        for (t, sc, l) in events {
            s = self.step(s, self.symbol(*t, *sc, *l));
        }
        self.accepts(s) & (1 << i) != 0
    }

    fn start_state_before_camera(&self) -> u16 {
        // Recomputed rather than stored: the first set is always id 1.
        1
    }
}

/// Moore's partition refinement for expression `i`: states start split by
/// whether `i` accepts there, and are split again until every class sends
/// each symbol to one class.
fn refine(table: &[u16], symbols: usize, accept: &[u64], i: usize) -> Vec<u16> {
    let n = accept.len();
    let mut class: Vec<u16> = accept.iter().map(|a| ((a >> i) & 1) as u16).collect();
    loop {
        let mut ids: HashMap<Vec<u16>, u16> = HashMap::new();
        let next: Vec<u16> = (0..n)
            .map(|s| {
                let mut sig = Vec::with_capacity(symbols + 1);
                sig.push(class[s]);
                sig.extend(
                    table[s * symbols..(s + 1) * symbols]
                        .iter()
                        .map(|&t| class[t as usize]),
                );
                let len = ids.len() as u16;
                *ids.entry(sig).or_insert(len)
            })
            .collect();
        let stable = ids.len() == count(&class);
        class = next;
        if stable {
            return class;
        }
    }
}

fn count(class: &[u16]) -> usize {
    let mut v: Vec<u16> = class.to_vec();
    v.sort_unstable();
    v.dedup();
    v.len()
}

/// The id of `set`, adding it as a new DFA state if it is one — refused past
/// [`MAX_STATES`], before an id could overflow.
fn intern(
    ids: &mut HashMap<Vec<usize>, u16>,
    sets: &mut Vec<Vec<usize>>,
    set: Vec<usize>,
) -> Result<u16, CompileError> {
    if let Some(&id) = ids.get(&set) {
        return Ok(id);
    }
    if sets.len() >= MAX_STATES {
        return Err(CompileError::TooComplex);
    }
    let id = sets.len() as u16;
    ids.insert(set.clone(), id);
    sets.push(set);
    Ok(id)
}

fn collect_labels(ast: &Ast, labels: &mut Vec<Option<String>>) {
    match ast {
        Ast::Event(pred) => {
            for p in &pred.patterns {
                for set in &p.labels {
                    let names = match set {
                        Set::Any => continue,
                        Set::In(v) | Set::NotIn(v) => v,
                    };
                    for name in names.iter().flatten() {
                        if !labels.iter().any(|l| l.as_deref() == Some(name)) {
                            labels.push(Some(name.clone()));
                        }
                    }
                }
            }
        }
        Ast::Concat(v) | Ast::Alt(v) => v.iter().for_each(|a| collect_labels(a, labels)),
        Ast::Repeat { node, .. } => collect_labels(node, labels),
    }
}

/// A set of symbols.
#[derive(Debug, Clone)]
struct SymSet(Vec<u64>);

impl SymSet {
    fn get(&self, i: usize) -> bool {
        self.0[i / 64] >> (i % 64) & 1 != 0
    }
}

fn pattern_matches(p: &Pattern, ty: EventType, sc: Scatter, label: &Option<String>) -> bool {
    p.types.contains(&ty) && p.scatters.contains(&sc) && p.labels.iter().all(|s| s.contains(label))
}

fn pred_symbols(pred: &Pred, labels: &[Option<String>], symbols: usize) -> SymSet {
    let mut bits = vec![0u64; symbols.div_ceil(64)];
    let mut i = 0;
    for ty in EventType::ALL {
        for sc in 0..Scatter::COUNT {
            let sc = [
                Scatter::Diffuse,
                Scatter::Glossy,
                Scatter::Singular,
                Scatter::Straight,
                Scatter::None,
            ][sc];
            for label in labels {
                let any = pred
                    .patterns
                    .iter()
                    .any(|p| pattern_matches(p, ty, sc, label));
                if any != pred.negated {
                    bits[i / 64] |= 1 << (i % 64);
                }
                i += 1;
            }
        }
    }
    SymSet(bits)
}

#[derive(Default)]
struct Nfa {
    /// Per state: symbol-labelled edges.
    edges: Vec<Vec<(SymSet, usize)>>,
    /// Per state: epsilon edges.
    eps: Vec<Vec<usize>>,
    /// Accepting states and the expression each accepts.
    finals: Vec<(usize, usize)>,
}

impl Nfa {
    fn state(&mut self) -> usize {
        self.edges.push(Vec::new());
        self.eps.push(Vec::new());
        self.edges.len() - 1
    }

    fn eps(&mut self, from: usize, to: usize) {
        self.eps[from].push(to);
    }

    /// Builds `ast`, returning its entry and exit states.
    fn build(&mut self, ast: &Ast, labels: &[Option<String>], symbols: usize) -> (usize, usize) {
        match ast {
            Ast::Event(pred) => {
                let (s, e) = (self.state(), self.state());
                self.edges[s].push((pred_symbols(pred, labels, symbols), e));
                (s, e)
            }
            Ast::Concat(items) => {
                let mut ends: Option<(usize, usize)> = None;
                for item in items {
                    let (s, e) = self.build(item, labels, symbols);
                    ends = Some(match ends {
                        None => (s, e),
                        Some((first, last)) => {
                            self.eps(last, s);
                            (first, e)
                        }
                    });
                }
                ends.expect("a concatenation has items")
            }
            Ast::Alt(alts) => {
                let (s, e) = (self.state(), self.state());
                for alt in alts {
                    let (a, b) = self.build(alt, labels, symbols);
                    self.eps(s, a);
                    self.eps(b, e);
                }
                (s, e)
            }
            Ast::Repeat { node, min, max } => {
                let (s, mut last) = {
                    let s = self.state();
                    (s, s)
                };
                for _ in 0..*min {
                    let (a, b) = self.build(node, labels, symbols);
                    self.eps(last, a);
                    last = b;
                }
                match max {
                    None => {
                        // Kleene star on one more copy.
                        let (a, b) = self.build(node, labels, symbols);
                        let e = self.state();
                        self.eps(last, a);
                        self.eps(last, e);
                        self.eps(b, a);
                        self.eps(b, e);
                        (s, e)
                    }
                    Some(max) => {
                        let e = self.state();
                        self.eps(last, e);
                        for _ in *min..*max {
                            let (a, b) = self.build(node, labels, symbols);
                            self.eps(last, a);
                            self.eps(b, e);
                            last = b;
                        }
                        (s, e)
                    }
                }
            }
        }
    }

    /// Epsilon closure, as a sorted list.
    fn closure(&self, mut stack: Vec<usize>) -> Vec<usize> {
        let mut seen = vec![false; self.edges.len()];
        let mut out = Vec::new();
        while let Some(q) = stack.pop() {
            if std::mem::replace(&mut seen[q], true) {
                continue;
            }
            out.push(q);
            stack.extend(self.eps[q].iter().copied());
        }
        out.sort_unstable();
        out
    }
}
