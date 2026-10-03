use super::*;

use EventType::{Camera as C, Light as L, Object as O, Reflect as R, Transmit as T, Volume as V};
use Scatter::{Diffuse as D, Glossy as G, None as N, Singular as S, Straight as St};

const DIFF: LabelId = LobeLabel::Diffuse as LabelId;
const SPEC: LabelId = LobeLabel::Specular as LabelId;
const COAT: LabelId = LobeLabel::Coat as LabelId;
const TRANS: LabelId = LobeLabel::Transmission as LabelId;

fn one(expr: &str) -> Lpe {
    Lpe::compile(&[expr]).unwrap_or_else(|e| panic!("{expr}: {e}"))
}

#[test]
fn osl_basics() {
    let direct_diffuse = one("C<RD>L");
    assert!(direct_diffuse.matches(&[(C, N, 0), (R, D, DIFF), (L, N, 0)], 0));
    assert!(!direct_diffuse.matches(&[(C, N, 0), (R, G, SPEC), (L, N, 0)], 0));
    assert!(!direct_diffuse.matches(&[(C, N, 0), (R, D, DIFF), (R, D, DIFF), (L, N, 0)], 0));
    assert!(!direct_diffuse.matches(&[(C, N, 0), (R, D, DIFF), (O, N, 0)], 0));

    let indirect = one("C<RD>.+L");
    assert!(indirect.matches(&[(C, N, 0), (R, D, DIFF), (R, G, SPEC), (L, N, 0)], 0));
    assert!(!indirect.matches(&[(C, N, 0), (R, D, DIFF), (L, N, 0)], 0));
    assert!(!indirect.matches(&[(C, N, 0), (R, G, SPEC), (R, D, DIFF), (L, N, 0)], 0));

    let all = one("C.*[LO]");
    for path in [
        vec![(C, N, 0), (L, N, 0)],
        vec![(C, N, 0), (O, N, 0)],
        vec![(C, N, 0), (T, S, TRANS), (V, N, 0), (R, D, DIFF), (L, N, 0)],
    ] {
        assert!(all.matches(&path, 0), "{path:?}");
    }

    // Shorthands: a scatter letter is any type with it, a type letter any
    // scatter.
    let any_diffuse = one("CD+L");
    assert!(any_diffuse.matches(&[(C, N, 0), (T, D, 6), (R, D, DIFF), (L, N, 0)], 0));
    let transmit = one("C<T.>.*[LO]");
    assert!(transmit.matches(&[(C, N, 0), (T, G, TRANS), (R, D, DIFF), (L, N, 0)], 0));
    assert!(!transmit.matches(&[(C, N, 0), (R, G, SPEC), (T, G, TRANS), (L, N, 0)], 0));
}

#[test]
fn labels_and_negated_sets() {
    let not_coat = one("C<RS[^'coat']>.*L");
    assert!(not_coat.matches(&[(C, N, 0), (R, S, SPEC), (L, N, 0)], 0));
    assert!(!not_coat.matches(&[(C, N, 0), (R, S, COAT), (L, N, 0)], 0));
    assert!(!not_coat.matches(&[(C, N, 0), (R, G, SPEC), (L, N, 0)], 0));

    let coat = one("C<RG'coat'>L");
    assert!(coat.matches(&[(C, N, 0), (R, G, COAT), (L, N, 0)], 0));
    assert!(!coat.matches(&[(C, N, 0), (R, G, SPEC), (L, N, 0)], 0));

    // A bare quoted label is <..'x'>.
    let spec = one("C'specular'L");
    assert!(spec.matches(&[(C, N, 0), (R, G, SPEC), (L, N, 0)], 0));
    assert!(spec.matches(&[(C, N, 0), (T, S, SPEC), (L, N, 0)], 0));

    // Top-level complement: any single event that is not reflective.
    let no_reflection = one("C[^R]*L");
    assert!(no_reflection.matches(&[(C, N, 0), (T, S, TRANS), (V, N, 0), (L, N, 0)], 0));
    assert!(!no_reflection.matches(&[(C, N, 0), (R, D, DIFF), (L, N, 0)], 0));
}

#[test]
fn light_groups() {
    let lpe = Lpe::compile(&["C.*<L.'key'>", "C.*<L.'fill'>"]).unwrap();
    let key = lpe.label("key").unwrap();
    let fill = lpe.label("fill").unwrap();
    let path = |tag| [(C, N, 0), (R, D, DIFF), (L, N, tag)];
    assert!(lpe.matches(&path(key), 0) && !lpe.matches(&path(key), 1));
    assert!(lpe.matches(&path(fill), 1) && !lpe.matches(&path(fill), 0));
    // An untagged light is in neither group.
    assert!(!lpe.matches(&path(0), 0) && !lpe.matches(&path(0), 1));
}

#[test]
fn bounded_repeats() {
    let two = one("C<R.>{2}L");
    assert!(two.matches(&[(C, N, 0), (R, D, DIFF), (R, G, SPEC), (L, N, 0)], 0));
    assert!(!two.matches(&[(C, N, 0), (R, D, DIFF), (L, N, 0)], 0));
    let one_to_two = one("C<R.>{1,2}L");
    assert!(one_to_two.matches(&[(C, N, 0), (R, D, DIFF), (L, N, 0)], 0));
    assert!(!one_to_two.matches(
        &[
            (C, N, 0),
            (R, D, DIFF),
            (R, D, DIFF),
            (R, D, DIFF),
            (L, N, 0)
        ],
        0
    ));
    let at_least_two = one("CD{2,}L");
    assert!(!at_least_two.matches(&[(C, N, 0), (R, D, DIFF), (L, N, 0)], 0));
    assert!(at_least_two.matches(
        &[
            (C, N, 0),
            (R, D, DIFF),
            (R, D, DIFF),
            (R, D, DIFF),
            (L, N, 0)
        ],
        0
    ));
    let alternation = one("C(<RD>|<TS>)L");
    assert!(alternation.matches(&[(C, N, 0), (T, S, TRANS), (L, N, 0)], 0));
    assert!(!alternation.matches(&[(C, N, 0), (T, D, TRANS), (L, N, 0)], 0));
    let straight = one("Cs*<RD>L");
    assert!(straight.matches(
        &[(C, N, 0), (T, St, 0), (T, St, 0), (R, D, DIFF), (L, N, 0)],
        0
    ));
}

#[test]
fn one_dfa_holds_every_expression() {
    let exprs = ["C<RD>[LO]", "C<RD>.+[LO]", "C<RG>[LO]", "C.*[LO]"];
    let lpe = Lpe::compile(&exprs).unwrap();
    assert_eq!(lpe.len(), 4);
    let direct = [(C, N, 0), (R, D, DIFF), (L, N, 0)];
    let mask: Vec<bool> = (0..4).map(|i| lpe.matches(&direct, i)).collect();
    assert_eq!(mask, [true, false, false, true]);
    // The dead state accepts nothing and leads nowhere.
    assert_eq!(lpe.accepts(0), 0);
    assert!(!lpe.live(0));
    assert!(lpe.live(lpe.start()));
    // After a glossy then a diffuse bounce, only C.*[LO] can still accept.
    let mut s = lpe.start();
    s = lpe.step(s, lpe.symbol(R, G, SPEC));
    s = lpe.step(s, lpe.symbol(R, D, DIFF));
    assert!(lpe.live(s));
    let end = lpe.step(s, lpe.symbol(L, N, 0));
    assert_eq!(lpe.accepts(end), 0b1000);
}

#[test]
fn refused_syntax_names_its_column() {
    for (expr, column) in [
        ("C<RD>?L", 6),
        ("CD1L", 2),
        ("C!L", 2),
        ("CBL", 2),
        ("C<RX>L", 4),
        ("C<RD", 5),
        ("C(RDL", 6),
        ("unoccluded C<RD>L", 1),
        ("C{2", 4),
        ("", 1),
        ("C'open", 2),
        ("CQL", 2),
    ] {
        let err = validate(expr).expect_err(expr);
        assert_eq!(err.column, column, "{expr}: {err}");
    }
    assert!(validate("C<RD>L").is_ok());
    assert_eq!(strip_prefix("lpe:C<RD>L"), "C<RD>L");
}

#[test]
fn oversized_input_is_refused_not_panicked() {
    // More expressions than the accept masks have bits.
    let many: Vec<String> = (0..=MAX_EXPRESSIONS)
        .map(|i| format!("C<RD>{{{}}}L", i % 4))
        .collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    assert_eq!(
        Lpe::compile(&refs).unwrap_err(),
        CompileError::TooManyExpressions(MAX_EXPRESSIONS + 1)
    );

    // `.*x.{n}`: a small expression whose DFA needs about 2^(n+1) states.
    assert_eq!(
        Lpe::compile(&["C.*<RD>.{16}[LO]"]).unwrap_err(),
        CompileError::TooComplex
    );
    // The same shape at a size that fits still compiles.
    assert!(Lpe::compile(&["C.*<RD>.{4}[LO]"]).is_ok());

    // Nested bounds multiply: each is within its limit, the whole is not.
    let err = validate("C(((.{32}){32}){32}){32}L").unwrap_err();
    assert!(err.message.contains("expand"), "{err}");
    assert!(validate("C(.{4}){4}L").is_ok());

    // Labels no expression names stay out of the alphabet: a light tagged
    // anything else reads as untagged.
    let lpe = Lpe::compile(&["C.*<L.'key'>"]).unwrap();
    assert!(lpe.label("key").is_some());
    assert!(lpe.label("rim").is_none());
}

#[test]
fn diffuse_starts_are_recognised_in_any_spelling() {
    for expr in [
        "C<RD>[LO]",
        "C<RD>.*<L.'key'>",
        "C<RD'diffuse'>.*L",
        "C(<RD>|<RD'diffuse'>)L",
        "C<RD>.+[LO]",
    ] {
        let lpe = Lpe::compile(&[expr]).unwrap();
        assert!(lpe.starts_with_diffuse_reflection(0), "{expr}");
    }
    for expr in [
        "C.*[LO]",
        "C<RG>L",
        "C[LO]",
        "C<.D>L",
        "C'diffuse'.*L",
        "C<RD>L|CL",
    ] {
        let lpe = Lpe::compile(&[expr]).unwrap();
        assert!(!lpe.starts_with_diffuse_reflection(0), "{expr}");
    }
    // Per expression, in a shared DFA.
    let lpe = Lpe::compile(&["C.*[LO]", "C<RD>[LO]"]).unwrap();
    assert!(!lpe.starts_with_diffuse_reflection(0));
    assert!(lpe.starts_with_diffuse_reflection(1));
}
